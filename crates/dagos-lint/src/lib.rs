//! DAGOS semantic linter.
//!
//! Syntax linters cannot check meaning; this one does. A project's rules are plain-English
//! sentences ("Swallows errors.", "Has a hidden side effect."). Functions are extracted from
//! source files, and Jev answers one calibrated yes/no question per rule for each function, all
//! functions in parallel. Jev only judges: findings are probabilities, and fixing them is the
//! coding agent's job.
//!
//! [`LintReviewer`] closes the loop inside DAGOS runs: it lints the functions a run's tool calls
//! changed each time the model says it is done, and the runtime hands the findings back to the
//! model until the code is clean, stops changing, or the run runs out of reviews.

pub mod extract;
pub mod judge;
mod reviewer;
pub mod rules;

use std::path::Path;
use std::sync::Arc;

use dagos_core::context::JevClassifier;
use dagos_core::domain::IrFinding;

pub use extract::{Function, Language};
pub use judge::{Cache, JudgeError, Unit};
pub use reviewer::{LintReviewer, MAX_FINDINGS, MAX_UNITS};
pub use rules::{LintConfig, Rule, default_rules};

/// The linter's configuration file inside the DAGOS directory.
pub const LINT_FILE: &str = "lint.json";

/// What linting some files found.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// The functions judged.
    pub units: Vec<Unit>,
    /// Every (function, rule) probability, in unit order then rule order.
    pub judgments: judge::Judgments,
    /// The rules at or above the threshold, most probable first.
    pub findings: Vec<IrFinding>,
}

/// Lints every function in `files` (named as they should appear in findings, relative to `root`)
/// against `config`'s rules.
pub async fn lint_files(
    root: &Path,
    files: &[String],
    jev: Arc<dyn JevClassifier>,
    config: &LintConfig,
) -> Result<Report, String> {
    let mut units = Vec::new();
    for file in files {
        let path = root.join(file);
        let Some(language) = Language::of(&path) else { continue };
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        for function in extract::functions(language, &text) {
            units.push(Unit { file: file.replace('\\', "/"), language, function });
        }
    }
    let judgments = judge::judge(jev, &Cache::default(), &config.rules, &units)
        .await
        .map_err(|error| error.to_string())?;
    let findings = judge::findings(&config.rules, &units, &judgments, config.threshold);
    Ok(Report { units, judgments, findings })
}
