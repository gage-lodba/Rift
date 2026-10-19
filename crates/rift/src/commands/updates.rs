//! Update-check commands: query GitHub releases, toggle launch notifications,
//! and open the release page in the default browser.

use tracing::warn;

use crate::backend::BackendHandle;
use crate::util::LockExt;

/// GitHub repo (owner/name) whose releases back the update check.
const RELEASE_REPO: &str = "gage-lodba/Rift";

impl BackendHandle {
    /// Check GitHub for a newer Rift release. Compares the running version
    /// against the latest published release's tag; surfaced in Settings.
    /// Network/parse failures degrade to "latest unknown" (still reporting the
    /// running version) rather than erroring, so the UI can always show the
    /// current version.
    ///
    /// Network I/O: run the future from a spawned task.
    pub async fn check_update(&self) -> Result<rift_types::UpdateStatus, String> {
        let current = env!("CARGO_PKG_VERSION").to_string();
        let mut status = rift_types::UpdateStatus {
            current: current.clone(),
            ..Default::default()
        };

        let url = format!("https://api.github.com/repos/{RELEASE_REPO}/releases/latest");
        let json: Option<serde_json::Value> = async {
            self.inner
                .player
                .http
                .get(&url)
                // GitHub's API rejects requests without a User-Agent.
                .header(reqwest::header::USER_AGENT, "rift-update-check")
                .header(reqwest::header::ACCEPT, "application/vnd.github+json")
                .send()
                .await
                .ok()?
                .error_for_status()
                .ok()?
                .json()
                .await
                .ok()
        }
        .await;

        if let Some(json) = json {
            status.latest = json
                .get("tag_name")
                .and_then(|v| v.as_str())
                .map(|s| s.trim_start_matches('v').to_string());
            status.update_available = status
                .latest
                .as_deref()
                .is_some_and(|l| is_newer(l, &current));
            status.url = json
                .get("html_url")
                .and_then(|v| v.as_str())
                .map(str::to_string);
        } else {
            warn!("update check failed to reach GitHub");
        }
        Ok(status)
    }

    /// Enable or disable the launch-time update check/notification and persist it.
    pub fn set_update_notifications(&self, enabled: bool) {
        self.inner
            .settings
            .lock_safe()
            .set_update_notifications(enabled);
    }

    /// Open an http(s) URL in the user's default browser.
    ///
    /// Used by the "Download" action on an available update. Validates the
    /// scheme first (only `http`/`https` are opened, never an arbitrary file or
    /// custom protocol), then hands it to the platform opener.
    pub fn open_url(&self, url: String) -> Result<(), String> {
        if !is_openable_url(&url) {
            return Err("refusing to open non-http(s) url".into());
        }
        open_external(&url)
    }
}

/// Whether `url` is safe to hand to the OS opener: only `http` and `https`.
///
/// This is what keeps a backend-driven `open_url` from being a generic
/// "launch anything" primitive (`file:`, `ms-settings:`, a bare executable path,
/// a `javascript:` payload).
fn is_openable_url(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

/// Hand `url` to the platform's default browser.
///
/// Split out (and pure of backend state) so the scheme gate above is the only
/// thing an external URL decision depends on.
fn open_external(url: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("could not open browser: {e}"))
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("could not open browser: {e}"))
    }
    #[cfg(windows)]
    {
        // explorer.exe hands the URL straight to the default handler via
        // ShellExecute, so — unlike `cmd /C start` — shell metacharacters such
        // as `&` in the URL aren't re-parsed. It's a GUI process, so no console
        // window flashes.
        match std::process::Command::new("explorer.exe").arg(url).spawn() {
            Ok(_) => Ok(()),
            Err(first) => {
                // Extremely unusual (explorer.exe missing or blocked); fall back
                // to the documented `start` form. The empty argument is `start`'s
                // window-title slot, so a URL that begins with `"` can't be
                // confused for the title.
                std::process::Command::new("cmd")
                    .args(["/C", "start", "", url])
                    .spawn()
                    .map(|_| ())
                    .map_err(|e| format!("could not open browser: {first}; fallback failed: {e}"))
            }
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = url;
        Err("opening a browser is not supported on this platform".into())
    }
}

/// Compare two dotted version strings (e.g. "0.2.0" vs "0.1.3"), returning true
/// if `latest` is strictly newer. Non-numeric/missing components count as 0, so
/// pre-release suffixes are treated leniently rather than crashing the check.
fn is_newer(latest: &str, current: &str) -> bool {
    fn parts(v: &str) -> Vec<u32> {
        v.trim_start_matches('v')
            .split('.')
            .map(|p| {
                p.split(|c: char| !c.is_ascii_digit())
                    .next()
                    .unwrap_or("")
                    .parse()
                    .unwrap_or(0)
            })
            .collect()
    }
    let (l, c) = (parts(latest), parts(current));
    for i in 0..l.len().max(c.len()) {
        let (a, b) = (
            l.get(i).copied().unwrap_or(0),
            c.get(i).copied().unwrap_or(0),
        );
        if a != b {
            return a > b;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{is_newer, is_openable_url};

    #[test]
    fn detects_newer_versions() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("0.1.1", "0.1.0"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(is_newer("v0.2.0", "0.1.0")); // leading v tolerated
        assert!(is_newer("0.2", "0.1.9")); // uneven lengths
    }

    #[test]
    fn ignores_same_or_older_versions() {
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(!is_newer("0.1.0", "0.1.0")); // identical
        assert!(!is_newer("0.1.0-rc1", "0.1.0")); // pre-release suffix -> 0
    }

    /// The scheme gate is what keeps a backend-driven `open_url` from being a
    /// generic "launch anything" primitive. `file:`, custom protocols and shell
    /// metacharacter payloads must be refused before any process is spawned.
    #[test]
    fn only_http_and_https_urls_are_opened() {
        assert!(is_openable_url(
            "https://github.com/gage-lodba/Rift/releases"
        ));
        assert!(is_openable_url("http://example.com"));
        for refused in [
            "file:///C:/Windows/System32/calc.exe",
            "javascript:alert(1)",
            "ms-settings:",
            "C:\\Windows\\System32\\calc.exe",
            "",
            " https://example.com",
        ] {
            assert!(!is_openable_url(refused), "must refuse {refused:?}");
        }
    }
}
