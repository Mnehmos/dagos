//! Lint rules: plain-English sentences a project keeps in `.dagos/lint.json`.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The linter's configuration for a project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LintConfig {
    /// Whether runs review the code they change (the review loop).
    #[serde(default = "yes")]
    pub enabled: bool,
    /// The probability at which a rule counts as applying.
    #[serde(default = "default_threshold")]
    pub threshold: f64,
    /// How many reviews one run may have.
    #[serde(default = "default_max_rounds")]
    pub max_rounds: usize,
    #[serde(default = "default_rules")]
    pub rules: Vec<Rule>,
    /// Findings a person marked "not a problem here": never reported again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dismissed: Vec<Dismissal>,
}

/// A rule a person decided does not apply to one function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dismissal {
    pub rule: String,
    /// The file as findings name it (relative to the project root, `/` separators).
    pub file: String,
    pub function: String,
}

/// One rule: a short sentence naming a problem, with optional hints for the judge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// Stable ID, e.g. `swallows-errors`.
    pub id: String,
    /// The rule, e.g. "Swallows errors."
    pub text: String,
    /// When the rule applies, in more detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applies: Option<String>,
    /// When it does not, e.g. accepted exceptions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub except: Option<String>,
}

fn yes() -> bool {
    true
}

fn default_threshold() -> f64 {
    0.7
}

fn default_max_rounds() -> usize {
    3
}

impl Default for LintConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold: default_threshold(),
            max_rounds: default_max_rounds(),
            rules: default_rules(),
            dismissed: Vec::new(),
        }
    }
}

impl LintConfig {
    /// Reads `path`; a missing file means the defaults.
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let config: Self = serde_json::from_str(&text)
            .map_err(|error| format!("invalid {}: {error}", path.display()))?;
        config.validate().map_err(|error| format!("invalid {}: {error}", path.display()))?;
        Ok(config)
    }

    /// Whether a person dismissed `rule` for `function` in `file`.
    pub fn is_dismissed(&self, rule: &str, file: &str, function: &str) -> bool {
        self.dismissed.iter().any(|d| d.rule == rule && d.file == file && d.function == function)
    }

    /// Writes the configuration to `path`.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        let text = serde_json::to_string_pretty(self).expect("lint configuration serializes");
        std::fs::write(path, format!("{text}\n"))
            .map_err(|error| format!("cannot save {}: {error}", path.display()))
    }

    /// Whether the configuration is usable: a threshold between 0 and 1, at least one review,
    /// and rules with unique IDs and some text.
    pub fn validate(&self) -> Result<(), String> {
        if !(0.0..=1.0).contains(&self.threshold) {
            return Err("threshold must be between 0 and 1".into());
        }
        if self.max_rounds == 0 {
            return Err("max_rounds must be at least 1".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for rule in &self.rules {
            let valid_id = !rule.id.is_empty()
                && rule.id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
            if !valid_id {
                return Err(format!("rule id `{}`: use letters, digits, - and _", rule.id));
            }
            if !seen.insert(&rule.id) {
                return Err(format!("rule id `{}` is used twice", rule.id));
            }
            if rule.text.trim().is_empty() {
                return Err(format!("rule `{}` has no text", rule.id));
            }
        }
        Ok(())
    }
}

/// The rules projects start with.
pub fn default_rules() -> Vec<Rule> {
    let rule = |id: &str, text: &str, applies: &str| Rule {
        id: id.into(),
        text: text.into(),
        applies: Some(applies.into()),
        except: None,
    };
    vec![
        rule(
            "swallows-errors",
            "Swallows errors.",
            "An error or failure is caught, ignored, or discarded without being handled, reported, \
             or passed on, so a caller can never learn it happened.",
        ),
        rule(
            "hidden-side-effect",
            "Has a hidden side effect.",
            "It changes state, files, globals, or the outside world in a way its name and \
             signature do not suggest.",
        ),
        rule(
            "does-too-many-jobs",
            "Does too many jobs.",
            "It does several unrelated things that would read better as separate functions.",
        ),
        rule(
            "misleading-name",
            "Its name lies about what it does.",
            "Its name or documentation promises something different from what the body does.",
        ),
        rule(
            "magic-values",
            "Relies on unexplained magic values.",
            "Behaviour depends on literal numbers or strings whose meaning is not named or \
             explained.",
        ),
        rule(
            "deep-nesting",
            "Nests so deeply that the logic is hard to follow.",
            "Conditions and loops are nested several levels deep where early returns or helpers \
             would make it clear.",
        ),
        rule(
            "repeats-itself",
            "Repeats logic that should be shared.",
            "The same logic appears more than once within the function.",
        ),
        rule(
            "trusts-input",
            "Trusts input it should validate.",
            "It uses caller-provided or external data without checks where a bad value would \
             cause harm: a crash, corruption, or a security problem.",
        ),
        rule(
            "leaks-resources",
            "Can leak a resource.",
            "Files, connections, locks, processes, or tasks it opens are not reliably released on \
             every path, including errors.",
        ),
        rule(
            "silent-fallback",
            "Silently falls back to a default that hides a problem.",
            "When something is missing or fails it substitutes a default value without saying \
             so, so the problem goes unnoticed.",
        ),
        rule(
            "dead-code",
            "Contains dead or unreachable code.",
            "Some of its code can never run or its result is never used.",
        ),
        rule(
            "vague-errors",
            "Reports errors too vaguely to act on.",
            "Its error messages or error values do not say what failed or why.",
        ),
        rule(
            "hardcoded-environment",
            "Hardcodes a secret, credential, or machine-specific path.",
            "It embeds a password, key, token, or a path that only exists on one machine.",
        ),
        rule(
            "comment-drift",
            "Has comments that contradict the code.",
            "A comment or doc comment describes behaviour the code does not have.",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_means_the_defaults_and_files_are_validated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lint.json");
        let config = LintConfig::load(&path).unwrap();
        assert_eq!(config.rules.len(), 14);
        assert!(config.enabled && config.threshold == 0.7 && config.max_rounds == 3);

        std::fs::write(&path, r#"{"rules": [{"id": "no-todo", "text": "Leaves a TODO."}]}"#)
            .unwrap();
        let config = LintConfig::load(&path).unwrap();
        assert_eq!(config.rules[0].text, "Leaves a TODO.");
        assert_eq!(config.threshold, 0.7, "unset fields keep their defaults");

        for bad in [
            r#"{"threshold": 2}"#,
            r#"{"max_rounds": 0}"#,
            r#"{"rules": [{"id": "a b", "text": "x"}]}"#,
            r#"{"rules": [{"id": "a", "text": "x"}, {"id": "a", "text": "y"}]}"#,
            r#"{"rules": [{"id": "a", "text": " "}]}"#,
            r#"{"unknown": true}"#,
        ] {
            std::fs::write(&path, bad).unwrap();
            assert!(LintConfig::load(&path).is_err(), "accepted {bad}");
        }
    }
}
