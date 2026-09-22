//! Architecture boundary checks.
//!
//! Layer dependencies inside `dagos-core` are part of the DAGOS architecture: the provider layer
//! must never see the store, and the domain layer depends on nothing. These checks fail the build
//! when a module reaches across a boundary or the core crate grows a transport/provider-SDK
//! dependency.
//!
//! Convention that keeps this checkable: cross-layer references use `crate::<layer>` paths, one
//! layer per `use` (no `crate::{a, b}` groups), and never `super::super`.

use std::fs;
use std::path::{Path, PathBuf};

/// Each layer and the crate-internal layers it may reference.
const LAYERS: &[(&str, &[&str])] = &[
    ("domain", &[]),
    ("contracts", &[]),
    ("store", &["domain"]),
    ("context", &["domain", "contracts", "store"]),
    ("ir", &["domain", "contracts", "store"]),
    ("provider", &["domain"]),
    ("response", &["domain", "contracts"]),
    ("runtime", &["domain", "contracts", "store", "context", "ir", "provider", "response"]),
];

/// Crates that would pull transport or provider-SDK concerns into the core.
const FORBIDDEN_CORE_DEPENDENCIES: &[&str] = &[
    "reqwest",
    "hyper",
    "axum",
    "ureq",
    "isahc",
    "surf",
    "tonic",
    "async-openai",
    "openai",
    "anthropic",
    "rmcp",
];

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn layer_files(layer: &str) -> Vec<PathBuf> {
    let src = manifest_dir().join("src");
    let mut files = Vec::new();
    let root_file = src.join(format!("{layer}.rs"));
    if root_file.exists() {
        files.push(root_file);
    }
    collect_rust_files(&src.join(layer), &mut files);
    files
}

fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.expect("readable directory entry").path();
        if path.is_dir() {
            collect_rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// First path segment after every `crate::` in the code of `source` (comment lines, including doc
/// links, are not dependencies); `{` marks a grouped import.
fn crate_references(source: &str) -> Vec<String> {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .flat_map(|line| {
            line.match_indices("crate::").map(|(index, pattern)| {
                let rest = &line[index + pattern.len()..];
                if rest.starts_with('{') {
                    "{".to_string()
                } else {
                    rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect()
                }
            })
        })
        .collect()
}

#[test]
fn every_declared_module_has_a_boundary_rule() {
    let lib = fs::read_to_string(manifest_dir().join("src/lib.rs")).expect("read lib.rs");
    for line in lib.lines() {
        let Some(module) =
            line.trim().strip_prefix("pub mod ").and_then(|rest| rest.strip_suffix(';'))
        else {
            continue;
        };
        assert!(
            LAYERS.iter().any(|(name, _)| *name == module),
            "module `{module}` is declared in lib.rs but has no boundary rule in tests/boundaries.rs"
        );
    }
}

#[test]
fn layers_only_reference_allowed_layers() {
    let mut violations = Vec::new();
    for (layer, allowed) in LAYERS {
        for file in layer_files(layer) {
            let source = fs::read_to_string(&file).expect("read source file");
            let shown = file.display();
            if source.contains("super::super") {
                violations.push(format!("{shown}: uses `super::super`; use a `crate::` path"));
            }
            for referenced in crate_references(&source) {
                if referenced == "{" {
                    violations.push(format!(
                        "{shown}: grouped `crate::{{..}}` import; use one `use crate::<layer>` per layer"
                    ));
                } else if referenced != *layer
                    && LAYERS.iter().any(|(name, _)| *name == referenced)
                    && !allowed.contains(&referenced.as_str())
                {
                    violations.push(format!(
                        "{shown}: layer `{layer}` must not depend on `{referenced}`"
                    ));
                }
            }
        }
    }
    assert!(violations.is_empty(), "{}", violations.join("\n"));
}

#[test]
fn core_has_no_transport_or_provider_sdk_dependencies() {
    let manifest = fs::read_to_string(manifest_dir().join("Cargo.toml")).expect("read Cargo.toml");
    for line in manifest.lines().map(str::trim) {
        for forbidden in FORBIDDEN_CORE_DEPENDENCIES {
            let Some(rest) = line.strip_prefix(forbidden) else {
                continue;
            };
            let rest = rest.trim_start();
            assert!(
                !(rest.starts_with('=') || rest.starts_with('.')),
                "dagos-core must not depend on `{forbidden}`; keep it behind a provider/transport crate"
            );
        }
    }
}

#[test]
fn boundary_checker_detects_references() {
    let source = "use crate::store::Store;\n/// See [`crate::runtime`].\nuse crate::domain::DagNode;\nuse crate::{a, b};";
    assert_eq!(crate_references(source), vec!["store", "domain", "{"]);
}
