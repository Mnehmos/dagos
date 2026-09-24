//! The review loop's reviewer: lints the functions a run's tool calls changed.
//!
//! A model reaches files only through tool calls, and DAGOS shows the reviewer every permitted
//! call before it runs. The reviewer finds the project files a call may touch in its arguments
//! (paths, and file names inside commands such as `exec_cli`'s) and remembers what each looked
//! like before the run first touched it. A review compares each remembered file with what is on
//! disk now and judges only the functions that are new or changed.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dagos_core::context::JevClassifier;
use dagos_core::domain::{Payload, RunId};
use dagos_core::tools::{Review, Reviewer, ToolRequest};
use serde_json::Value;

use crate::extract::{Language, functions};
use crate::judge::{Cache, JudgeError, Unit, findings, judge};
use crate::rules::LintConfig;

/// The most changed functions one review judges.
pub const MAX_UNITS: usize = 40;

/// The most findings one review hands back to the model.
pub const MAX_FINDINGS: usize = 12;

/// Lints what runs change, against the project's rules.
pub struct LintReviewer {
    root: PathBuf,
    jev: Arc<dyn JevClassifier>,
    config: LintConfig,
    cache: Cache,
    /// Per run: each touched file and its content before the run first touched it (`None` if it
    /// did not exist).
    before: Mutex<HashMap<RunId, BTreeMap<PathBuf, Option<String>>>>,
}

impl LintReviewer {
    /// A reviewer for the project in `root`.
    pub fn new(root: &Path, jev: Arc<dyn JevClassifier>, config: LintConfig) -> Self {
        let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        Self { root, jev, config, cache: Cache::default(), before: Mutex::default() }
    }

    pub fn config(&self) -> &LintConfig {
        &self.config
    }

    /// The project files (in a language the linter reads) that `arguments` name: string values
    /// that are paths, and path-like words inside longer strings such as shell commands.
    pub fn named_files(&self, arguments: &Payload) -> Vec<PathBuf> {
        let mut words = Vec::new();
        for value in arguments.values() {
            collect_strings(value, &mut words);
        }
        let mut files: Vec<PathBuf> = Vec::new();
        for word in words.iter().flat_map(|text| path_words(text)) {
            if let Some(path) = self.project_file(&word)
                && !files.contains(&path)
            {
                files.push(path);
            }
        }
        files
    }

    /// `word` as a file inside the project in a language the linter reads, whether or not it
    /// exists yet (its directory must).
    fn project_file(&self, word: &str) -> Option<PathBuf> {
        let path = Path::new(word);
        Language::of(path)?;
        let joined = if path.is_absolute() || word.starts_with('/') {
            PathBuf::from(word)
        } else {
            self.root.join(word)
        };
        let parent = std::fs::canonicalize(joined.parent()?).ok()?;
        let file = parent.join(joined.file_name()?);
        file.starts_with(&self.root).then_some(file)
    }

    /// `path` relative to the project root, with `/` separators.
    fn display(&self, path: &Path) -> String {
        let relative = path.strip_prefix(&self.root).unwrap_or(path);
        relative.to_string_lossy().replace('\\', "/")
    }

    /// The new or changed functions in the files the run touched.
    fn changed_units(&self, touched: &BTreeMap<PathBuf, Option<String>>) -> Vec<Unit> {
        let mut units = Vec::new();
        for (path, before) in touched {
            let Some(language) = Language::of(path) else { continue };
            let Ok(now) = std::fs::read_to_string(path) else { continue };
            if before.as_deref() == Some(now.as_str()) {
                continue;
            }
            let earlier: HashSet<String> = before
                .as_deref()
                .map(|text| functions(language, text).into_iter().map(|f| f.text).collect())
                .unwrap_or_default();
            for function in functions(language, &now) {
                if !earlier.contains(&function.text) {
                    units.push(Unit { file: self.display(path), language, function });
                }
            }
        }
        units.truncate(MAX_UNITS);
        units
    }
}

#[async_trait]
impl Reviewer for LintReviewer {
    async fn before_call(&self, run_id: &RunId, request: &ToolRequest) {
        let files = self.named_files(&request.arguments);
        let mut before = self.before.lock().expect("reviewer lock");
        let touched = before.entry(run_id.clone()).or_default();
        for file in files {
            touched.entry(file.clone()).or_insert_with(|| std::fs::read_to_string(&file).ok());
        }
    }

    async fn review(&self, run_id: &RunId) -> Result<Review, String> {
        let touched = self.before.lock().expect("reviewer lock").get(run_id).cloned();
        let Some(touched) = touched else { return Ok(Review::default()) };
        let units = self.changed_units(&touched);
        if units.is_empty() {
            return Ok(Review::default());
        }
        let judgments = match judge(self.jev.clone(), &self.cache, &self.config.rules, &units).await
        {
            Ok(judgments) => judgments,
            // Without a Jev that answers yes/no questions there is nothing to review with.
            Err(JudgeError::Unsupported) => return Ok(Review::default()),
            Err(error) => return Err(error.to_string()),
        };
        let mut found = findings(&self.config.rules, &units, &judgments, self.config.threshold);
        found.truncate(MAX_FINDINGS);
        let mut hasher = DefaultHasher::new();
        for unit in &units {
            (&unit.file, &unit.function.text).hash(&mut hasher);
        }
        Ok(Review {
            findings: found,
            judged: units.len(),
            fingerprint: format!("{:016x}", hasher.finish()),
        })
    }

    fn end(&self, run_id: &RunId) {
        self.before.lock().expect("reviewer lock").remove(run_id);
    }
}

fn collect_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => out.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| collect_strings(item, out)),
        Value::Object(map) => map.values().for_each(|item| collect_strings(item, out)),
        _ => {}
    }
}

/// The words of `text` that could be paths: split at whitespace and shell punctuation, with
/// quotes removed. A plain path is its own single word.
fn path_words(text: &str) -> Vec<String> {
    text.split(|c: char| {
        c.is_whitespace() || matches!(c, ';' | '|' | '&' | '<' | '>' | '(' | ')' | '`' | ',' | '=')
    })
    .map(|word| word.trim_matches(|c| matches!(c, '"' | '\'')))
    .filter(|word| !word.is_empty() && word.contains('.'))
    .map(str::to_owned)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_words_come_from_paths_and_commands() {
        assert_eq!(path_words("src/lib.rs"), ["src/lib.rs"]);
        assert_eq!(
            path_words(r#"sed -i 's/a/b/' "src/main.rs" && cat x.py > out.ts"#),
            ["src/main.rs", "x.py", "out.ts"]
        );
    }
}
