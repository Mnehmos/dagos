//! The tool guard: Jev's risk questions for each tool call about to run.
//!
//! Tool policies are per tool, but one tool can do very different things: `exec_cli` runs any
//! command, `batch_tools` runs other tools, `jev_dispatch` picks tools itself. Before a call runs,
//! the guard asks Jev one yes/no question per risk about this call, in the light of the user's
//! request. A risk judged likely turns an `allow` into a question for the person; Jev only
//! classifies, and the approvals gate acts on it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use dagos_core::context::{JevClassifier, NoulQuestion};
use dagos_core::domain::{EventData, RunId};
use dagos_core::store::Store;
use dagos_core::tools::ToolRequest;
use serde_json::{Value, json};

/// How long the guard may take; a call it could not check in time asks the person.
pub const GUARD_TIMEOUT: Duration = Duration::from_secs(20);

/// The most argument JSON Jev is shown at once; longer arguments are judged in overlapping
/// windows, and a risk counts when any window shows it.
const WINDOW_CHARS: usize = 6000;

/// How far consecutive windows overlap, so a command split by a window edge is seen whole.
const WINDOW_OVERLAP: usize = 500;

/// The most windows one call is judged in; arguments longer than that cannot be checked, and the
/// call asks the person.
const MAX_WINDOWS: usize = 8;

/// The risks the guard asks about: ID, what a yes means, and whether the user's request can
/// excuse it. Deleting data, touching secrets, and delegating to unseen tools always need a
/// person, even when the request asked for them.
pub const RISKS: &[(&str, &str, bool)] = &[
    (
        "deletes-data",
        "Deletes files, folders, or data, or destroys them beyond recovery (for example rm, del, \
         Remove-Item, rmdir, DROP, TRUNCATE, git clean, git reset --hard, format).",
        false,
    ),
    (
        "destroys-data",
        "Overwrites or changes files or data that the user's request did not ask to change.",
        true,
    ),
    (
        "changes-system",
        "Changes the system: installs or removes software, or changes settings, services, \
         permissions, users, the registry, scheduled tasks, or startup items.",
        true,
    ),
    (
        "uses-network",
        "Sends data to, downloads from, or runs something from the internet or another machine.",
        true,
    ),
    (
        "touches-secrets",
        "Reads, prints, copies, or sends passwords, keys, tokens, or credentials.",
        false,
    ),
    ("outside-project", "Creates, changes, or deletes files outside the project folder.", true),
    (
        "controls-processes",
        "Stops, kills, or restarts processes or services it did not start itself.",
        true,
    ),
    (
        "controls-computer",
        "Controls the mouse, keyboard, screen, or other applications' windows.",
        true,
    ),
    (
        "delegates",
        "Hands the work to other tools or agents chosen while it runs, so what actually runs is \
         not visible in this call.",
        false,
    ),
];

/// A risk Jev judged likely.
#[derive(Debug, Clone, PartialEq)]
pub struct Risk {
    pub id: &'static str,
    pub probability: f64,
}

/// What the guard concluded about a call.
#[derive(Debug, Clone, PartialEq)]
pub enum Assessment {
    /// Jev does not answer yes/no questions: policies decide alone.
    Unchecked,
    /// Jev was asked and failed; the call is treated as needing a person.
    Failed(String),
    /// The risks at or above the threshold; empty means none.
    Risks(Vec<Risk>),
}

impl Assessment {
    /// Why a person must decide, if one must.
    pub fn concern(&self) -> Option<String> {
        match self {
            Self::Unchecked => None,
            Self::Risks(risks) if risks.is_empty() => None,
            Self::Failed(error) => Some(format!("the safety check could not run ({error})")),
            Self::Risks(risks) => {
                let named: Vec<String> = risks
                    .iter()
                    .map(|risk| format!("{} {:.2}", risk.id, risk.probability))
                    .collect();
                Some(format!("Jev flagged it: {}", named.join(", ")))
            }
        }
    }
}

/// Asks Jev the risk questions.
pub struct Guard {
    jev: Arc<dyn JevClassifier>,
    store: Arc<Store>,
    root: PathBuf,
}

impl Guard {
    pub fn new(jev: Arc<dyn JevClassifier>, store: Arc<Store>, root: PathBuf) -> Self {
        Self { jev, store, root }
    }

    /// Judges `request` (a call of the tool described by `description`) in `run_id`; risks at or
    /// above `threshold` count.
    pub async fn assess(
        &self,
        run_id: &RunId,
        request: &ToolRequest,
        description: &str,
        threshold: f64,
    ) -> Assessment {
        let message = self
            .store
            .transaction(|tx| tx.events(run_id))
            .ok()
            .and_then(|events| {
                events.into_iter().find_map(|event| match event.data {
                    EventData::MessageRecorded { text, .. } => Some(text),
                    _ => None,
                })
            })
            .unwrap_or_default();
        let arguments = Value::Object(request.arguments.clone()).to_string();
        let Some(windows) = windows(&arguments) else {
            let size = arguments.chars().count();
            return Assessment::Failed(format!(
                "its arguments are too long to check ({size} characters)"
            ));
        };
        let questions: Vec<NoulQuestion> = RISKS
            .iter()
            .map(|(id, meaning, excusable)| NoulQuestion {
                key: (*id).to_owned(),
                instructions: json!({
                    "task": "Decide whether this tool call, as its arguments show, carries this \
                             risk. Judge the call itself, in the light of the user's request.",
                    "risk": meaning,
                }),
                if_true: format!("Running this call {}", lowercase_first(meaning)),
                if_false: if *excusable {
                    "The call does not do this, or only in a way the request plainly asks for, \
                     inside the project."
                        .to_owned()
                } else {
                    "The call does not do this, even if the request asked for it.".to_owned()
                },
            })
            .collect();
        let mut highest = vec![0.0_f64; RISKS.len()];
        let count = windows.len();
        for (index, window) in windows.into_iter().enumerate() {
            let part = (count > 1).then(|| format!("part {} of {count}", index + 1));
            let state = state(&message, &self.root, request, description, window, part);
            let answer =
                tokio::time::timeout(GUARD_TIMEOUT, self.jev.decide(&state, &questions)).await;
            let scores = match answer {
                Err(_elapsed) => {
                    return Assessment::Failed(format!("no answer within {GUARD_TIMEOUT:?}"));
                }
                Ok(Err(error)) => return Assessment::Failed(error.to_string()),
                Ok(Ok(None)) => return Assessment::Unchecked,
                Ok(Ok(Some(scores))) if scores.len() != RISKS.len() => {
                    return Assessment::Failed("wrong number of answers".into());
                }
                Ok(Ok(Some(scores))) => scores,
            };
            for (high, score) in highest.iter_mut().zip(scores) {
                *high = high.max(score);
            }
        }
        Assessment::Risks(
            RISKS
                .iter()
                .zip(highest)
                .filter(|(_, probability)| *probability >= threshold)
                .map(|((id, _, _), probability)| Risk {
                    id,
                    probability: (probability * 100.0).round() / 100.0,
                })
                .collect(),
        )
    }
}

/// `arguments` in overlapping windows of at most [`WINDOW_CHARS`] characters, or `None` when it
/// would take more than [`MAX_WINDOWS`].
fn windows(arguments: &str) -> Option<Vec<String>> {
    let chars: Vec<char> = arguments.chars().collect();
    if chars.len() <= WINDOW_CHARS {
        return Some(vec![arguments.to_owned()]);
    }
    let step = WINDOW_CHARS - WINDOW_OVERLAP;
    let count = (chars.len() - WINDOW_OVERLAP).div_ceil(step);
    if count > MAX_WINDOWS {
        return None;
    }
    Some(
        (0..count)
            .map(|index| {
                let start = index * step;
                chars[start..(start + WINDOW_CHARS).min(chars.len())].iter().collect()
            })
            .collect(),
    )
}

/// The Decisions state for one window of a call's arguments.
fn state(
    message: &str,
    root: &std::path::Path,
    request: &ToolRequest,
    description: &str,
    arguments: String,
    part: Option<String>,
) -> Value {
    let description: String = description.chars().take(800).collect();
    let mut state = json!({
        "request": message,
        "project_folder": root.to_string_lossy(),
        "tool": request.name,
        "tool_description": description,
        "arguments": arguments,
    });
    if let Some(part) = part {
        state["arguments_part"] = json!(part);
    }
    state
}

fn lowercase_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The tools of the same server that `request`'s arguments name (by short or full name), e.g.
/// `ooda.exec_cli` inside a `ooda.batch_tools` call. `tools` are all known IR tool names.
pub fn named_tools<'a>(
    request: &ToolRequest,
    tools: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let Some((server, own)) = request.name.split_once('.') else { return Vec::new() };
    let mut words = std::collections::BTreeSet::new();
    for value in request.arguments.values() {
        collect_words(value, &mut words);
    }
    tools
        .into_iter()
        .filter_map(|name| {
            let (tool_server, tool) = name.split_once('.')?;
            let named = tool_server == server
                && tool != own
                && (words.contains(tool) || words.contains(name));
            named.then(|| name.to_owned())
        })
        .collect()
}

fn collect_words(value: &Value, out: &mut std::collections::BTreeSet<String>) {
    match value {
        Value::String(text) => {
            out.insert(text.clone());
            for word in text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.')) {
                out.insert(word.to_owned());
            }
        }
        Value::Array(items) => items.iter().for_each(|item| collect_words(item, out)),
        Value::Object(map) => map.values().for_each(|item| collect_words(item, out)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(name: &str, arguments: Value) -> ToolRequest {
        ToolRequest {
            call_id: "call_1".into(),
            name: name.into(),
            arguments: arguments.as_object().unwrap().clone(),
        }
    }

    #[test]
    fn meta_tools_name_the_tools_they_run() {
        let tools = ["ooda.exec_cli", "ooda.read_file", "ooda.batch_tools", "other.exec_cli"];
        let batch = request(
            "ooda.batch_tools",
            json!({"operations": [{"tool": "exec_cli", "args": {"command": "ls"}}, {"tool": "read_file"}]}),
        );
        assert_eq!(named_tools(&batch, tools), ["ooda.exec_cli", "ooda.read_file"]);
        let prose = request("ooda.jev_dispatch", json!({"task": "use exec_cli to list files"}));
        assert_eq!(named_tools(&prose, tools), ["ooda.exec_cli"]);
        let plain = request("ooda.exec_cli", json!({"command": "cargo test"}));
        assert!(named_tools(&plain, tools).is_empty(), "a tool does not name itself");
    }

    #[test]
    fn long_arguments_are_judged_in_overlapping_windows_up_to_a_limit() {
        assert_eq!(windows("short").unwrap(), ["short"]);
        let exact = "x".repeat(WINDOW_CHARS);
        assert_eq!(windows(&exact).unwrap().len(), 1);
        let tail = format!("{}rm -rf ~", "p".repeat(WINDOW_CHARS));
        let parts = windows(&tail).unwrap();
        assert_eq!(parts.len(), 2);
        assert!(parts[1].ends_with("rm -rf ~"), "the end of the arguments is judged too");
        assert!(parts.iter().all(|part| part.chars().count() <= WINDOW_CHARS));
        let straddle = windows(&"y".repeat(2 * WINDOW_CHARS)).unwrap();
        assert!(straddle.len() >= 2);
        let limit = WINDOW_OVERLAP + MAX_WINDOWS * (WINDOW_CHARS - WINDOW_OVERLAP);
        assert!(windows(&"z".repeat(limit)).is_some());
        assert!(windows(&"z".repeat(limit + 1)).is_none(), "too long to check: the call asks");
    }

    #[test]
    fn concerns_name_the_risks() {
        assert_eq!(Assessment::Unchecked.concern(), None);
        assert_eq!(Assessment::Risks(vec![]).concern(), None);
        let risks = Assessment::Risks(vec![Risk { id: "destroys-data", probability: 0.91 }]);
        assert_eq!(risks.concern().unwrap(), "Jev flagged it: destroys-data 0.91");
        assert!(Assessment::Failed("offline".into()).concern().unwrap().contains("offline"));
    }
}
