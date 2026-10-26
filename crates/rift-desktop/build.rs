//! Embed the Rift icon in `rift-music.exe` as a Windows PE resource.
//!
//! Two different icons are involved, and they are easy to confuse:
//!
//! * the *runtime window icon* — set from `assets/icons/128x128.png` by
//!   `main.rs::window_icon()`, it only affects the running window and its
//!   taskbar button;
//! * the *file icon* — a resource inside the executable that Explorer, the
//!   Start menu, shortcuts and taskbar pinning read without ever running the
//!   app. That is the one this script embeds.
//!
//! The legacy Tauri shell got the file icon for free from
//! `tauri-build`/`tauri-winres`, which compiled `src-tauri/icons/icon.ico`.
//! Replacing Tauri with a plain `cargo build` dropped that step, and the
//! executable silently fell back to the generic Windows application icon. This
//! script restores it, so the icon is defined in exactly one place again.
//!
//! The resource compiler (`rc.exe`) ships with the Windows SDK, which the MSVC
//! build already needs (see "Windows" in README's build requirements). It is
//! only invoked when both the host (for the build dependency) and the target (for
//! the PE resource) are Windows; nothing here runs for any other target.
//!
//! No VERSIONINFO/metadata belongs here: this script exists to restore the file
//! icon, nothing else.

fn main() {
    // Rebuild when the embedded inputs change. Without these, Cargo would only
    // notice a modified .rc/.ico on a clean build and the executable could keep
    // a stale icon indefinitely.
    println!("cargo:rerun-if-changed=assets/icons/rift.rc");
    println!("cargo:rerun-if-changed=assets/icons/icon.ico");

    // Build scripts are compiled for the *host*: `cfg(windows)` below only says
    // that Windows SDK tooling may exist. It does not say the artifact is a PE
    // file — a Windows host cross-compiling to Linux/macOS would otherwise run
    // `rc.exe` and emit an MSVC `.lib` link argument into a non-Windows link.
    // Cargo exposes the real target through `CARGO_CFG_TARGET_OS`; skip the
    // resource step for every non-Windows target.
    let windows_target = std::env::var("CARGO_CFG_TARGET_OS")
        .map(|os| os == "windows")
        .unwrap_or(false);
    if !windows_target {
        return;
    }

    // `embed-resource` is a host-gated build dependency (see Cargo.toml), so
    // this code only exists when the build starts on Windows. A non-Windows host
    // cross-compiling *to* Windows has no resource compiler dependency and ships
    // without the embedded icon (documented: other hosts have no resource
    // compiler to run).
    //
    // `manifest_required()`, not `manifest_optional()`: an icon is cosmetic at
    // runtime, but silently shipping an icon-less executable is exactly the
    // regression this script prevents. If the resource compiler cannot be
    // found, the build must fail loudly so CI/release never publish a binary
    // without the icon. The result is `#[must_use]`, and the workspace denies
    // `unused_must_use`, so consuming it here is also what keeps lint-clean.
    #[cfg(windows)]
    embed_resource::compile_for("assets/icons/rift.rc", ["rift-music"], embed_resource::NONE)
        .manifest_required()
        .expect(
            "failed to embed the Windows icon resource: is the Windows SDK \
         resource compiler (rc.exe) installed?",
        );
}
