//! Update-check commands: query GitHub releases, toggle launch notifications,
//! and open the release page in the default browser.

use tauri::State;
use tracing::warn;

use crate::util::LockExt;
use crate::AppState;

/// GitHub repo (owner/name) whose releases back the update check.
const RELEASE_REPO: &str = "gage-lodba/Rift";

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

/// Check GitHub for a newer Rift release. Compares the running version against
/// the latest published release's tag; surfaced in Settings. Network/parse
/// failures degrade to "latest unknown" (still reporting the running version)
/// rather than erroring, so the UI can always show the current version.
#[tauri::command(rename_all = "snake_case")]
pub async fn check_update(state: State<'_, AppState>) -> Result<rift_types::UpdateStatus, String> {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let mut status = rift_types::UpdateStatus {
        current: current.clone(),
        ..Default::default()
    };

    let url = format!("https://api.github.com/repos/{RELEASE_REPO}/releases/latest");
    let json: Option<serde_json::Value> = async {
        state
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
            .map(|l| is_newer(l, &current))
            .unwrap_or(false);
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
#[tauri::command(rename_all = "snake_case")]
pub fn set_update_notifications(enabled: bool, state: State<'_, AppState>) {
    state.settings.lock_safe().set_update_notifications(enabled);
}

/// Open an http(s) URL in the user's default browser. Used by the "Download"
/// action on an available update (the webview itself shouldn't navigate away).
#[tauri::command(rename_all = "snake_case")]
pub fn open_url(url: String) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("refusing to open non-http(s) url".into());
    }
    #[cfg(target_os = "linux")]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(&url);
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(&url);
        c
    };
    #[cfg(windows)]
    let mut cmd = {
        // explorer.exe hands the URL straight to the default handler via
        // ShellExecute, so — unlike `cmd /C start` — shell metacharacters such
        // as `&` in the URL aren't re-parsed. It's a GUI process, so no console
        // window flashes.
        let mut c = std::process::Command::new("explorer.exe");
        c.arg(&url);
        c
    };
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("could not open browser: {e}"))
}

#[cfg(test)]
mod tests {
    use super::is_newer;

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
}
