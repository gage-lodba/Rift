//! The ACL actually grants the commands the frontend calls.
//!
//! Declaring an app manifest (`build.rs`) puts Rift's own commands under the
//! ACL — Tauri checks it for app-defined commands only when the app has one. The
//! cost is that a command which is not *granted* stops working at runtime, and
//! nothing about that is visible to the compiler: the command is registered, the
//! frontend calls it, the capability names a permission, and the app still
//! fails with "command not allowed".
//!
//! That is not hypothetical. `permissions/default.toml` was first written as
//! `[[set]] identifier = "default"`, which puts the permissions in
//! `Manifest::permission_sets`. Tauri resolves the literal identifier `default`
//! by consulting *only* `Manifest::default_permission`
//! (`tauri-utils/src/acl/resolved.rs`, `get_permissions`), so `"default"` in the
//! capability resolved to **zero** permissions and every command was rejected —
//! an empty library on launch, with no error the user could act on beyond
//! "bootstrap not allowed".
//!
//! So this test resolves the ACL exactly as Tauri does at startup, from the same
//! generated artifacts, and asserts the result against `invoke_handler`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use tauri::utils::acl::capability::Capability;
use tauri::utils::acl::manifest::Manifest;
use tauri::utils::acl::resolved::Resolved;
use tauri::utils::platform::Target;

/// The generated ACL artifacts, as `tauri_build` wrote them.
///
/// `gen/schemas` is a copy of what the build script emits (it is gitignored, as
/// build output). Locating them through `OUT_DIR` is not possible from an
/// integration test, so the committed-to-disk copy is used and its freshness is
/// asserted below rather than assumed.
fn generated_dir() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("gen/schemas");
    assert!(
        dir.join("acl-manifests.json").exists(),
        "{} is missing — run a build first (build.rs generates it)",
        dir.display()
    );
    dir
}

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

fn resolved_acl() -> Resolved {
    let dir = generated_dir();
    let manifests: BTreeMap<String, Manifest> = serde_json::from_str(
        &std::fs::read_to_string(dir.join("acl-manifests.json")).expect("read acl-manifests.json"),
    )
    .expect("parse acl-manifests.json");
    let capabilities: BTreeMap<String, Capability> = serde_json::from_str(
        &std::fs::read_to_string(dir.join("capabilities.json")).expect("read capabilities.json"),
    )
    .expect("parse capabilities.json");

    let target = Target::from_triple(
        &std::env::var("TARGET").unwrap_or_else(|_| "x86_64-pc-windows-msvc".to_owned()),
    );
    Resolved::resolve(&manifests, capabilities, target).expect("resolve the ACL")
}

/// The app manifest must exist at all: without it Tauri does not check the ACL
/// for app commands, which is the opposite of what `build.rs` intends, and every
/// command is reachable from any webview script.
#[test]
fn the_app_manifest_is_present_so_app_commands_are_checked() {
    let acl = resolved_acl();
    assert!(
        acl.has_app_acl,
        "no app ACL manifest: Rift's own commands are outside the ACL entirely"
    );
}

/// The regression guard for the `permissions/default.toml` format bug. Every
/// command the backend registers must resolve to an allowed command, or the
/// frontend's call to it is rejected at runtime.
#[test]
fn every_registered_command_is_granted_by_the_acl() {
    let acl = resolved_acl();
    let registered = registered_commands();
    assert!(
        registered.len() > 30,
        "parsed only {} commands out of invoke_handler — this test needs updating",
        registered.len()
    );

    let missing: Vec<&String> = registered
        .iter()
        .filter(|name| !acl.allowed_commands.contains_key(name.as_str()))
        .collect();

    assert!(
        missing.is_empty(),
        "these commands are registered but not granted by the ACL, so the webview \
         cannot call them ({} allowed): {missing:?}",
        acl.allowed_commands.len()
    );
}

/// And nothing more: the grant should be exactly the registered set, so a
/// command removed from `invoke_handler` or from `permissions/default.toml` is
/// noticed rather than lingering as dead access.
#[test]
fn the_acl_grants_nothing_beyond_the_registered_commands() {
    let acl = resolved_acl();
    let registered = registered_commands();

    let extra: Vec<&String> = acl
        .allowed_commands
        .keys()
        .filter(|name| !name.starts_with("plugin:") && !registered.contains(name.as_str()))
        .collect();

    assert!(
        extra.is_empty(),
        "the ACL grants app commands that are not registered: {extra:?}"
    );
}
