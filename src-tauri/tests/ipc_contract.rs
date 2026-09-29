//! The IPC contract, checked as a test.
//!
//! `AppState`-driven commands can't be invoked in a unit test, but `tauri::command`
//! generates a `__cmd__<name>` macro alongside each one, and the frontend names
//! its commands as string literals. Reading both sides as text and comparing them
//! turns "the frontend calls something the backend doesn't have, or with the wrong
//! argument keys" from a runtime `command not found` / `invalid args` into a
//! failing test.
//!
//! It is a text-level check, not a type-level one, and deliberately so: the exact
//! failure it exists to catch — this session removed `play_track`'s unused `radio`
//! parameter and left the frontend passing `"radio": false`, which would have
//! failed at runtime for every single-track play — leaves both sides compiling
//! perfectly. Nothing in the type system connects a `json!({...})` key to a
//! command's parameter name.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Arguments Tauri injects from the invocation itself; the frontend never sends
/// these, so they are excluded from the name comparison.
const INJECTED: &[&str] = &["app", "state", "window", "webview", "handle"];

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `<repo>/src-tauri` for this crate.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri has a parent")
        .to_path_buf()
}

fn rust_sources(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    out.push((path, text));
                }
            }
        }
    }
    out
}

/// Every command function in the backend, mapped to its parameter names.
///
/// Matched on the function definition rather than the `__cmd__<name>` marker
/// macro `tauri::command` generates: that marker is not emitted for a command
/// whose argument list names a type only the *library* crate has (the toolchain
/// then reports it once, in the binary that does have the type), so the marker
/// set is a strict subset. The definition is always there.
fn backend_commands() -> BTreeMap<String, BTreeSet<String>> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands");
    let mut found = BTreeMap::new();
    for (_, text) in rust_sources(&dir) {
        for (idx, _) in text.match_indices("#[tauri::command") {
            // Skip past the attribute's closing bracket, then to the `fn`.
            let after_attr = match text[idx..].find(']') {
                Some(rel) => &text[idx + rel..],
                None => continue,
            };
            let Some(fn_rel) = after_attr.find("fn ") else {
                continue;
            };
            let rest = &after_attr[fn_rel + 3..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if name.is_empty() {
                continue;
            }
            found
                .entry(name.clone())
                .or_insert_with(|| command_params(&after_attr[fn_rel..], &name));
        }
    }
    found
}

/// The parameter names of `fn <name>(...)`, minus the injected ones.
fn command_params(text: &str, name: &str) -> BTreeSet<String> {
    let Some(start) = text.find(&format!("fn {name}(")) else {
        return BTreeSet::new();
    };
    // Start at the opening paren, so the first chunk is a parameter rather than
    // `fn <name>(first_param`.
    let after = &text[start + format!("fn {name}").len()..];
    debug_assert!(
        after.starts_with('('),
        "expected the parameter list: {after}"
    );
    let Some(close) = after.find(')') else {
        return BTreeSet::new();
    };
    let params = &after[1..close];
    // `name: Type` pairs, skipping `::` inside a path type.
    let mut out = BTreeSet::new();
    for chunk in params.split(',') {
        let Some((lhs, rhs)) = chunk.split_once(':') else {
            continue;
        };
        if rhs.starts_with(':') {
            continue; // a `::` path, not a parameter
        }
        let key = lhs.trim().trim_start_matches("mut ").trim();
        if key.is_empty() || INJECTED.contains(&key) {
            continue;
        }
        out.insert(key.to_owned());
    }
    out
}

/// Every command name the frontend passes to `invoke`/`fire`, with the argument
/// keys it sends for each.
fn frontend_calls() -> BTreeMap<String, BTreeSet<BTreeSet<String>>> {
    let dir = repo_root().join("ui/src");
    let mut found: BTreeMap<String, BTreeSet<BTreeSet<String>>> = BTreeMap::new();
    for (_, text) in rust_sources(&dir) {
        for call in text.match_indices("json!(") {
            let before = &text[..call.0];
            // Walk back to the start of this statement and look for the command
            // literal, e.g. `api::fire("play_track", json!({ ... }))`.
            let stmt_start = before.rfind(';').map(|i| i + 1).unwrap_or(0);
            let stmt = &before[stmt_start..];
            let Some(open) = stmt.rfind('"') else {
                continue;
            };
            let Some(close) = stmt[..open].rfind('"') else {
                continue;
            };
            let cmd = &stmt[close + 1..open];
            if cmd.is_empty() || !cmd.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                continue;
            }
            if !stmt.contains("invoke") && !stmt.contains("fire") {
                continue;
            }
            // The keys are the `"key":` pairs inside the json! object.
            let body = &text[call.0..];
            let Some(end) = body.find("})").or_else(|| body.find('}')) else {
                continue;
            };
            let object = &body[..end];
            let mut keys = BTreeSet::new();
            let bytes = object.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == b'"' {
                    if let Some(rel) = object[i + 1..].find('"') {
                        let key = &object[i + 1..i + 1 + rel];
                        let after = object[i + 1 + rel + 1..].trim_start();
                        if after.starts_with(':') && !key.is_empty() {
                            keys.insert(key.to_owned());
                        }
                        i += rel + 2;
                        continue;
                    }
                }
                i += 1;
            }
            found.entry(cmd.to_owned()).or_default().insert(keys);
        }
    }
    found
}

/// Every command name registered in `invoke_handler`.
fn registered_commands() -> BTreeSet<String> {
    let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"))
        .expect("read main.rs");
    let Some(start) = text.find("generate_handler![") else {
        panic!("main.rs has no invoke_handler");
    };
    let body = &text[start..];
    let end = body.find(']').unwrap_or(body.len());
    body[..end]
        .split("commands::")
        .skip(1)
        .filter_map(|chunk| {
            let name: String = chunk
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            (!name.is_empty()).then_some(name)
        })
        .collect()
}

#[test]
fn every_registered_command_exists() {
    let registered = registered_commands();
    let backend = backend_commands();
    assert!(
        !registered.is_empty(),
        "parsed no commands out of invoke_handler — the test needs updating"
    );
    for name in &registered {
        assert!(
            backend.contains_key(name),
            "`{name}` is registered in main.rs but no `#[tauri::command]` defines it"
        );
    }
}

#[test]
fn every_command_the_frontend_calls_is_registered() {
    let registered = registered_commands();
    let calls = frontend_calls();
    assert!(
        calls.len() > 30,
        "parsed only {} frontend calls — the test needs updating",
        calls.len()
    );
    let mut unknown = Vec::new();
    for name in calls.keys() {
        if !registered.contains(name) {
            unknown.push(name.clone());
        }
    }
    assert!(
        unknown.is_empty(),
        "the frontend calls commands that are not in invoke_handler: {unknown:?}"
    );
}

/// The regression guard. `play_track`'s `radio` parameter was removed as dead
/// code (L4) while the call site still sent `"radio": false`; both sides
/// compiled and the command failed at runtime. Every frontend argument key must
/// name a real parameter, and every parameter must be supplied.
#[test]
fn every_frontend_argument_key_matches_a_backend_parameter() {
    let backend = backend_commands();
    let calls = frontend_calls();
    let mut problems = Vec::new();

    for (name, variants) in &calls {
        let Some(params) = backend.get(name) else {
            continue; // reported by the registration test
        };
        for keys in variants {
            for key in keys {
                if !params.contains(key) {
                    problems.push(format!(
                        "{name}: frontend sends `{key}`, backend takes {params:?}"
                    ));
                }
            }
            for param in params {
                if !keys.contains(param) {
                    problems.push(format!(
                        "{name}: backend wants `{param}`, frontend sends {keys:?}"
                    ));
                }
            }
        }
    }

    assert!(
        problems.is_empty(),
        "IPC argument mismatch:\n  {}",
        problems.join("\n  ")
    );
}
