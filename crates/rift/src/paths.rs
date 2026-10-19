//! Native application-data directory resolution.
//!
//! Rift used to ask the desktop shell for `app.path().app_data_dir()`. This
//! module is the framework-free replacement: it resolves the same per-user data
//! directory the app has always used, so an existing installation keeps its
//! library, offline downloads and settings after the conversion.
//!
//! The layout is the reverse-DNS application identifier as a single directory
//! component, which is exactly what the old shell resolved:
//!
//! | Platform | Directory |
//! |---|---|
//! | Windows | `%APPDATA%\dev.jerimiah.rift` (i.e. `…\AppData\Roaming\dev.jerimiah.rift`) |
//! | Linux | `$XDG_DATA_HOME/dev.jerimiah.rift`, defaulting to `~/.local/share/dev.jerimiah.rift` |
//! | macOS | `~/Library/Application Support/dev.jerimiah.rift` |
//!
//! `RIFT_DATA_DIR` overrides the whole path (tests, preview runs, portable
//! installations). [`BackendConfig::data_dir`](crate::BackendConfig) is the
//! programmatic equivalent and takes precedence over the environment variable.

use std::path::{Path, PathBuf};

/// Reverse-DNS identifier of the application; also the data-directory name.
///
/// This was the old shell config's `identifier` and must not change: it is where
/// every existing installation's `library.json`, `settings.json` and
/// `downloads/` live.
pub const APP_IDENTIFIER: &str = "dev.jerimiah.rift";

/// Environment variable that overrides the data directory entirely.
pub const DATA_DIR_ENV: &str = "RIFT_DATA_DIR";

/// Why the platform data directory could not be determined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataDirError {
    /// Human-readable explanation, surfaced through [`Display`](std::fmt::Display).
    message: String,
}

impl std::fmt::Display for DataDirError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DataDirError {}

/// The application's data directory, honouring `RIFT_DATA_DIR`.
///
/// Nothing is created here; callers create it (the backend constructor does).
pub fn native_data_dir() -> Result<PathBuf, DataDirError> {
    // The override does not need the platform base directory at all, so check it
    // before paying for (or failing on) the known-folder lookup.
    if let Some(value) = std::env::var_os(DATA_DIR_ENV).filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(value));
    }
    let base = base_data_dir()?;
    resolve_data_dir(None, None, Some(&base))
}

/// Pure resolver behind [`native_data_dir`]: explicit config wins, then the
/// environment override, then the platform data directory joined with
/// [`APP_IDENTIFIER`]. Split out so precedence is testable without touching the
/// process environment or the real user profile.
fn resolve_data_dir(
    configured: Option<&Path>,
    env_override: Option<&std::ffi::OsStr>,
    base: Option<&Path>,
) -> Result<PathBuf, DataDirError> {
    if let Some(dir) = configured {
        return Ok(dir.to_path_buf());
    }
    if let Some(value) = env_override.filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(value));
    }
    let base = base.ok_or_else(|| DataDirError {
        message: "could not determine the OS data directory; set RIFT_DATA_DIR to choose one"
            .to_owned(),
    })?;
    Ok(data_dir_under(base))
}

/// The identifier directory as it sits under a platform data directory.
fn data_dir_under(base: &Path) -> PathBuf {
    base.join(APP_IDENTIFIER)
}

/// The platform base data directory (`%APPDATA%`, `$XDG_DATA_HOME`,
/// `~/Library/Application Support`).
///
/// `directories::BaseDirs` is the primary source; the environment fallbacks
/// exist so a stripped/odd environment (a service launcher, a sandbox without
/// the known-folder API) still gets the historical path rather than a panic.
fn base_data_dir() -> Result<PathBuf, DataDirError> {
    if let Some(dirs) = directories::BaseDirs::new() {
        return Ok(dirs.data_dir().to_path_buf());
    }
    env_base_data_dir().ok_or_else(|| DataDirError {
        message: "could not determine the OS data directory; set RIFT_DATA_DIR to choose one"
            .to_owned(),
    })
}

/// Fallback platform data directory computed from environment variables only.
fn env_base_data_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .filter(|v| !v.is_empty())
            .map(|home| {
                PathBuf::from(home)
                    .join("Library")
                    .join("Application Support")
            })
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|v| !v.is_empty())
                    .map(|home| PathBuf::from(home).join(".local").join("share"))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> PathBuf {
        PathBuf::from("C:\\Users\\test\\AppData\\Roaming")
    }

    #[test]
    fn the_default_directory_is_the_identifier_under_the_platform_data_dir() {
        let resolved =
            resolve_data_dir(None, None, Some(&base())).expect("a base directory resolves");
        assert_eq!(
            resolved,
            base().join(APP_IDENTIFIER),
            "the layout must stay %APPDATA%\\dev.jerimiah.rift"
        );
    }

    #[test]
    fn a_configured_data_dir_wins_over_the_environment() {
        let configured = PathBuf::from("D:\\rift-data");
        let env = std::ffi::OsStr::new("C:\\env-rift");
        let resolved = resolve_data_dir(Some(&configured), Some(env), Some(&base()))
            .expect("configured directory resolves");
        assert_eq!(resolved, configured);
    }

    #[test]
    fn the_environment_override_wins_over_the_platform_default() {
        let env = std::ffi::OsStr::new("C:\\env-rift");
        let resolved = resolve_data_dir(None, Some(env), Some(&base())).expect("override resolves");
        assert_eq!(resolved, PathBuf::from(env));
    }

    #[test]
    fn an_empty_override_is_ignored_rather_than_meaning_the_empty_path() {
        let empty = std::ffi::OsStr::new("");
        let resolved =
            resolve_data_dir(None, Some(empty), Some(&base())).expect("default resolves");
        assert_eq!(resolved, base().join(APP_IDENTIFIER));
    }

    #[test]
    fn no_base_directory_is_an_error_that_names_the_override() {
        let err = resolve_data_dir(None, None, None).expect_err("no base directory");
        assert!(
            err.to_string().contains(DATA_DIR_ENV),
            "the error should tell the user how to fix it: {err}"
        );
    }

    /// `native_data_dir` must never panic; in a normal test environment it
    /// resolves to an absolute path ending in the identifier.
    #[test]
    fn the_native_environment_resolves_to_the_identifier_directory() {
        if std::env::var_os(DATA_DIR_ENV).is_some() {
            // The override deliberately replaces the default layout, so this
            // assertion only describes the un-overridden environment.
            return;
        }
        let dir = native_data_dir().expect("the test environment has a data directory");
        assert!(
            dir.ends_with(APP_IDENTIFIER),
            "expected a path ending in {}: {}",
            APP_IDENTIFIER,
            dir.display()
        );
    }
}
