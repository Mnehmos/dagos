//! Judging: Jev answers one yes/no question per rule for each function, in parallel, and
//! remembers the answers so an unchanged function is never judged twice.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex};

use dagos_core::context::{JevClassifier, NoulQuestion};
use dagos_core::domain::IrFinding;
use serde_json::json;
use tokio::task::JoinSet;

use crate::extract::{Function, Language};
use crate::rules::Rule;

/// The longest function source Jev is shown; longer functions are cut.
pub const MAX_SOURCE_CHARS: usize = 12_000;

/// A function to judge, with where it lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    /// The file as findings name it: relative to the project root, `/` separators.
    pub file: String,
    pub language: Language,
    pub function: Function,
}

/// Why nothing could be judged.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JudgeError {
    #[error(
        "semantic lint needs a Jev that answers yes/no questions (TypeSafe's ~typesafe/jev-latest)"
    )]
    Unsupported,
    #[error("Jev failed to judge `{function}` in {file}: {message}")]
    Failed { file: String, function: String, message: String },
}

/// Remembered probabilities: (function source, rule) → probability.
#[derive(Default)]
pub struct Cache {
    answers: Mutex<HashMap<u64, f64>>,
}

impl Cache {
    fn key(unit: &Unit, rule: &Rule) -> u64 {
        let mut hasher = DefaultHasher::new();
        (unit.language.name(), &unit.function.text, &rule.text, &rule.applies, &rule.except)
            .hash(&mut hasher);
        hasher.finish()
    }

    pub fn len(&self) -> usize {
        self.answers.lock().expect("cache lock").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The cache saved at `path`; empty if there is none or it cannot be read.
    pub fn load(path: &std::path::Path) -> Self {
        let answers = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<HashMap<String, f64>>(&text).ok())
            .map(|saved| {
                saved
                    .into_iter()
                    .filter_map(|(key, p)| Some((u64::from_str_radix(&key, 16).ok()?, p)))
                    .collect()
            })
            .unwrap_or_default();
        Self { answers: Mutex::new(answers) }
    }

    /// Saves the cache to `path`, keeping at most [`MAX_CACHED`] answers.
    pub fn save(&self, path: &std::path::Path) -> Result<(), String> {
        let answers = self.answers.lock().expect("cache lock");
        let saved: HashMap<String, f64> =
            answers.iter().take(MAX_CACHED).map(|(key, p)| (format!("{key:016x}"), *p)).collect();
        let text = serde_json::to_string(&saved).expect("the cache serializes");
        std::fs::write(path, text)
            .map_err(|error| format!("cannot save {}: {error}", path.display()))
    }
}

/// The most answers the saved cache keeps.
pub const MAX_CACHED: usize = 50_000;

/// Every (unit, rule) probability, in unit order then rule order.
pub type Judgments = Vec<Vec<f64>>;

/// Judges each unit against every rule: cached answers are reused, the rest are asked in one
/// Decisions request per unit, all units in parallel.
pub async fn judge(
    jev: Arc<dyn JevClassifier>,
    cache: &Cache,
    rules: &[Rule],
    units: &[Unit],
) -> Result<Judgments, JudgeError> {
    let mut judgments: Judgments = vec![vec![0.0; rules.len()]; units.len()];
    let mut asks = JoinSet::new();
    {
        let answers = cache.answers.lock().expect("cache lock");
        for (index, unit) in units.iter().enumerate() {
            let mut missing = Vec::new();
            for (rule_index, rule) in rules.iter().enumerate() {
                match answers.get(&Cache::key(unit, rule)) {
                    Some(probability) => judgments[index][rule_index] = *probability,
                    None => missing.push(rule_index),
                }
            }
            if missing.is_empty() {
                continue;
            }
            let questions: Vec<NoulQuestion> =
                missing.iter().map(|&rule_index| question(&rules[rule_index])).collect();
            let state = state(unit);
            let jev = jev.clone();
            asks.spawn(async move {
                let answer = jev.decide(&state, &questions).await;
                (index, missing, answer)
            });
        }
    }
    while let Some(joined) = asks.join_next().await {
        let (index, missing, answer) = joined.expect("judging tasks do not panic");
        let unit = &units[index];
        let failed = |message: String| JudgeError::Failed {
            file: unit.file.clone(),
            function: unit.function.name.clone(),
            message,
        };
        let probabilities = match answer {
            Ok(Some(probabilities)) if probabilities.len() == missing.len() => probabilities,
            Ok(Some(_)) => return Err(failed("wrong number of answers".into())),
            Ok(None) => return Err(JudgeError::Unsupported),
            Err(error) => return Err(failed(error.to_string())),
        };
        let mut answers = cache.answers.lock().expect("cache lock");
        for (rule_index, probability) in missing.into_iter().zip(probabilities) {
            judgments[index][rule_index] = probability;
            answers.insert(Cache::key(unit, &rules[rule_index]), probability);
        }
    }
    Ok(judgments)
}

/// The rules at or above `threshold`, most probable first.
pub fn findings(
    rules: &[Rule],
    units: &[Unit],
    judgments: &Judgments,
    threshold: f64,
) -> Vec<IrFinding> {
    let mut findings: Vec<IrFinding> = Vec::new();
    for (unit, row) in units.iter().zip(judgments) {
        for (rule, probability) in rules.iter().zip(row) {
            if *probability >= threshold {
                findings.push(IrFinding {
                    rule: rule.id.clone(),
                    text: rule.text.clone(),
                    file: unit.file.clone(),
                    function: unit.function.name.clone(),
                    line: unit.function.line,
                    probability: (probability * 100.0).round() / 100.0,
                });
            }
        }
    }
    findings.sort_by(|a, b| b.probability.total_cmp(&a.probability));
    findings
}

/// The Decisions state for one function.
fn state(unit: &Unit) -> serde_json::Value {
    let text = &unit.function.text;
    let source = match text.char_indices().nth(MAX_SOURCE_CHARS) {
        Some((end, _)) => format!("{}\n… (cut)", &text[..end]),
        None => text.clone(),
    };
    json!({
        "file": unit.file,
        "language": unit.language.name(),
        "function": unit.function.name,
        "source": source,
    })
}

fn question(rule: &Rule) -> NoulQuestion {
    NoulQuestion {
        key: rule.id.clone(),
        instructions: json!({
            "task": "Decide whether this lint rule applies to the function in `state.source`.",
            "rule": rule.text,
        }),
        if_true: rule
            .applies
            .clone()
            .unwrap_or_else(|| "The function clearly has the problem the rule names.".into()),
        if_false: rule.except.clone().map_or_else(
            || {
                "The function does not have this problem, or only trivially or intentionally."
                    .into()
            },
            |except| format!("The function does not have this problem. Not a problem: {except}"),
        ),
    }
}
