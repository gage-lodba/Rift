//! Guard for the Linux desktop launcher shipped with the app.
//!
//! `assets/linux/Rift.desktop` is plain data: nothing compiles it, so a hand
//! edit can silently break the launcher (a typo in a key, a CRLF ending, an
//! `Exec` that no longer names the binary, an app id the window never
//! advertises). These assertions hold the committed file to the same
//! "cannot rot silently" rule as the rest of the platform integration
//! (`docs/native-platform.md`), and — on Linux — exercise the installer end to
//! end against throwaway XDG directories and prefixes.
//!
//! The identity the entry has to agree with lives in
//! [`rift_desktop::APP_ID`] / [`rift_desktop::BINARY_NAME`] and is applied to
//! the window by `main.rs` on Linux.

use std::collections::HashMap;
use std::path::Path;

/// The committed entry, exactly as the installer copies it.
const ENTRY: &str = include_str!("../assets/linux/Rift.desktop");

/// Icon assets the installer maps into the hicolor theme, as
/// `(asset file, hicolor size directory)`. Keep in step with
/// `tools/install-linux-desktop.sh`.
const ICON_ASSETS: [(&str, &str); 5] = [
    ("32x32.png", "32x32"),
    ("64x64.png", "64x64"),
    ("128x128.png", "128x128"),
    ("128x128@2x.png", "256x256"),
    ("icon.png", "512x512"),
];

/// Read a PNG's pixel dimensions from its IHDR chunk (`PNG signature: 8 bytes,
/// IHDR length: 4, "IHDR": 4, width: 4, height: 4`).
fn png_dimensions(path: &Path) -> (u32, u32) {
    let bytes = std::fs::read(path)
        .unwrap_or_else(|error| panic!("cannot read PNG {}: {error}", path.display()));
    assert!(
        bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "{} is not a PNG",
        path.display()
    );
    assert!(
        bytes.len() >= 24,
        "{} is too short to hold a PNG IHDR chunk",
        path.display()
    );
    let width = u32::from_be_bytes(bytes[16..20].try_into().expect("width bytes"));
    let height = u32::from_be_bytes(bytes[20..24].try_into().expect("height bytes"));
    (width, height)
}

/// The square edge length of a hicolor theme directory such as `128x128`.
fn theme_size(size_dir: &str) -> u32 {
    let (width, height) = size_dir
        .split_once('x')
        .unwrap_or_else(|| panic!("theme size is a WxH directory name: {size_dir:?}"));
    assert_eq!(
        width, height,
        "the hicolor theme size directory must be square: {size_dir:?}"
    );
    width
        .parse()
        .unwrap_or_else(|error| panic!("theme size must be numeric, got {size_dir:?}: {error}"))
}

/// Parse a desktop entry into its `[Desktop Entry]` keys.
///
/// Deliberately strict: this is a committed data file, not user input, so any
/// structural surprise (a second group, a duplicate key, a key before the group
/// header, a line without `=`) is a defect rather than something to tolerate.
fn key_values(entry: &str) -> HashMap<&str, &str> {
    let mut fields = HashMap::new();
    let mut in_group = false;
    for (number, raw) in entry.lines().enumerate() {
        let line = raw.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            assert_eq!(
                line,
                "[Desktop Entry]",
                "line {}: only the [Desktop Entry] group belongs in this file",
                number + 1
            );
            assert!(!in_group, "line {}: duplicate group header", number + 1);
            in_group = true;
            continue;
        }
        assert!(
            in_group,
            "line {}: key before the [Desktop Entry] header",
            number + 1
        );
        let (key, value) = line.split_once('=').unwrap_or_else(|| {
            panic!(
                "line {}: expected a key=value line, got {line:?}",
                number + 1
            )
        });
        assert!(!key.is_empty(), "line {}: empty key", number + 1);
        assert!(
            !key.contains(char::is_whitespace),
            "line {}: key {key:?} contains whitespace",
            number + 1
        );
        assert!(
            fields.insert(key, value).is_none(),
            "line {}: duplicate key {key:?}",
            number + 1
        );
    }
    assert!(in_group, "the file has no [Desktop Entry] header");
    fields
}

/// Decode the installed `Exec` value back to the executable path.
///
/// The installer writes the value in the two layers the Desktop Entry
/// Specification defines, in order: first the Exec quoted-string layer (one
/// backslash before `"`, `` ` ``, `$` and `\`), then the general string layer
/// (every backslash doubled). This decoder accepts only that exact shape — it
/// rejects the single-backslash form GLib refuses — so it is a semantic guard
/// behind the round-trip test's raw-encoding assertion, not a permissive
/// inverse that would hide an under-escaped entry.
///
/// Only the Linux installer tests install an entry and decode it back; the
/// portable tests assert the raw `Exec` line. Gating the decoder avoids a
/// Windows dead-code warning under `-D warnings`.
#[cfg(target_os = "linux")]
fn decode_exec(value: &str) -> String {
    // General string layer: collapse `\\` pairs and reject a lone escape.
    let mut unescaped = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            assert_eq!(
                chars.next(),
                Some('\\'),
                "the entry string layer must escape every backslash: {value:?}"
            );
            unescaped.push('\\');
        } else {
            unescaped.push(ch);
        }
    }
    // Exec quoted-string layer: strip the quotes, then honour the four escapes
    // the specification allows inside them.
    assert!(
        unescaped.starts_with('"') && unescaped.ends_with('"') && unescaped.len() >= 2,
        "Exec must be one double-quoted argument: {value:?}"
    );
    let inner = &unescaped[1..unescaped.len() - 1];
    let mut decoded = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let escaped = chars.next().expect("dangling backslash in Exec");
            assert!(
                matches!(escaped, '"' | '`' | '$' | '\\'),
                "unexpected Exec escape \\{escaped} in {value:?}"
            );
            decoded.push(escaped);
        } else {
            assert_ne!(ch, '"', "unescaped double quote inside Exec: {value:?}");
            decoded.push(ch);
        }
    }
    decoded
}

/// The entry carries every field a KDE/GNOME launcher needs, with sane values.
#[test]
fn the_desktop_entry_has_the_required_launcher_fields() {
    let fields = key_values(ENTRY);

    let expected = [
        ("Type", "Application"),
        ("Version", "1.5"),
        ("Name", "Rift"),
        ("Exec", rift_desktop::BINARY_NAME),
        ("Icon", rift_desktop::BINARY_NAME),
        ("Terminal", "false"),
        ("StartupWMClass", rift_desktop::APP_ID),
        ("SingleMainWindow", "true"),
        ("StartupNotify", "false"),
    ];
    for (key, value) in expected {
        assert_eq!(
            fields.get(key).copied(),
            Some(value),
            "{key} must be {value:?} in the committed Rift.desktop"
        );
    }
    for key in ["GenericName", "Comment"] {
        let value = fields
            .get(key)
            .unwrap_or_else(|| panic!("{key} is missing"));
        assert!(!value.trim().is_empty(), "{key} must not be empty");
    }
}

/// `Exec` is the bare binary name (the installer makes it absolute), with no
/// field codes: Rift's argument parser rejects positional arguments, so a
/// `%U`/`%F` here would make every launcher click exit with an error.
#[test]
fn exec_invokes_the_binary_without_field_codes() {
    let fields = key_values(ENTRY);
    let exec = fields["Exec"];
    assert_eq!(exec, rift_desktop::BINARY_NAME);
    assert!(
        !exec.contains('%') && !exec.contains(char::is_whitespace),
        "Exec must stay a bare binary name for the installer to rebase: {exec:?}"
    );
}

/// The launcher's categories are a well-formed freedesktop category list, and
/// they put Rift where a music player belongs.
#[test]
fn categories_are_a_well_formed_category_list() {
    let fields = key_values(ENTRY);
    let categories = fields["Categories"];
    assert!(
        categories.ends_with(';'),
        "a Categories list must end with ';': {categories:?}"
    );
    let items: Vec<&str> = categories.trim_end_matches(';').split(';').collect();
    assert!(
        items.iter().all(|item| !item.is_empty()),
        "Categories has an empty element: {categories:?}"
    );
    assert_eq!(items.first().copied(), Some("AudioVideo"));
    assert!(
        items.contains(&"Player"),
        "Rift is an audio player: {items:?}"
    );
}

/// The committed file is plain UTF-8 with LF endings and a final newline, so it
/// stays diffable (`.gitattributes` normalises it) and installs cleanly.
#[test]
fn the_entry_is_plain_utf8_with_lf_endings() {
    assert!(
        !ENTRY.contains('\r'),
        "the desktop entry must use LF endings"
    );
    assert_eq!(
        ENTRY.as_bytes().last().copied(),
        Some(b'\n'),
        "the desktop entry must end with a newline"
    );
    assert!(ENTRY.is_ascii(), "the desktop entry should stay ASCII");
}

/// Every icon the installer maps into the hicolor theme exists and really is a
/// square PNG of the advertised size — a renamed or re-exported asset must not
/// leave the menu entry without an icon.
#[test]
fn installer_icon_assets_are_valid_pngs_of_their_theme_size() {
    let icon_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icons");
    for (file, size_dir) in ICON_ASSETS {
        let path = icon_dir.join(file);
        let (width, height) = png_dimensions(&path);
        let expected = theme_size(size_dir);
        assert_eq!(
            (width, height),
            (expected, expected),
            "{} is {width}x{height}, but the hicolor theme slot is {size_dir}",
            path.display()
        );
    }
}

/// End-to-end checks of `tools/install-linux-desktop.sh`, Linux-only because the
/// script is the Linux launcher installer (and uses GNU `install`, `readlink`,
/// `mktemp` and `mv -T`).
#[cfg(target_os = "linux")]
mod installer {
    use super::{decode_exec, key_values, png_dimensions, theme_size};
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    /// The test-built executable, used as the install source.
    const BINARY: &str = env!("CARGO_BIN_EXE_rift-music");

    /// Keep in step with `ICON_ASSETS` above (size directory names).
    const ICON_SIZES: [&str; 5] = ["32x32", "64x64", "128x128", "256x256", "512x512"];

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn script() -> PathBuf {
        let script = repo_root().join("tools/install-linux-desktop.sh");
        assert!(script.is_file(), "missing {}", script.display());
        script
    }

    /// A fresh, unique directory under a canonical temp root. Canonicalising the
    /// root keeps the expected paths equal to what the installer's `readlink -m`
    /// produces, even when the system temp directory is a symlink.
    fn temp_root(name: &str) -> PathBuf {
        let base = std::fs::canonicalize(std::env::temp_dir())
            .unwrap_or_else(|error| panic!("canonicalise the temp directory: {error}"));
        let root = base.join(format!("rift-desktop-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create the throwaway test directory");
        root
    }

    fn script_command() -> Command {
        let mut command = Command::new("bash");
        command.arg(script());
        command
    }

    fn install_command(prefix: &Path, binary: &Path) -> Command {
        let mut command = script_command();
        command
            .args(["--no-cache-refresh", "--prefix"])
            .arg(prefix)
            .arg("--binary")
            .arg(binary);
        command
    }

    fn uninstall_command(prefix: &Path) -> Command {
        let mut command = script_command();
        command
            .args(["--uninstall", "--no-cache-refresh", "--prefix"])
            .arg(prefix);
        command
    }

    fn run_ok(mut command: Command, what: &str) -> Output {
        let output = command
            .output()
            .unwrap_or_else(|error| panic!("run the {what}: {error}"));
        assert!(
            output.status.success(),
            "{what} failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn run_err(mut command: Command, what: &str) -> Output {
        let output = command
            .output()
            .unwrap_or_else(|error| panic!("run the {what}: {error}"));
        assert!(
            !output.status.success(),
            "{what} unexpectedly succeeded:\nstdout: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        output
    }

    /// Put `stub_dir` first on the child's `PATH`, so a stub tool shadows the
    /// real one (and exists even where the real tool is not installed).
    fn with_stub_path(mut command: Command, stub_dir: &Path) -> Command {
        let path = std::env::var_os("PATH").expect("PATH is set");
        let mut search = vec![stub_dir.to_path_buf()];
        search.extend(std::env::split_paths(&path));
        command.env(
            "PATH",
            std::env::join_paths(search).expect("join the test PATH"),
        );
        command
    }

    /// A directory must hold no hidden file: the installer's staging and backup
    /// names all start with a dot, so anything hidden is a transaction leftover.
    fn assert_no_staged_files(dir: &Path) {
        for entry in
            std::fs::read_dir(dir).unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
        {
            let name = entry.expect("read a directory entry").file_name();
            assert!(
                !name.to_string_lossy().starts_with('.'),
                "the installer must remove its staged temp files, found {} in {}",
                name.to_string_lossy(),
                dir.display()
            );
        }
    }

    /// The exact raw `Exec=` line the two-stage Desktop Entry escaping must
    /// produce: four backslashes for a literal `\`, and two for `"`, `` ` ``
    /// and `$` (the Exec layer adds one, the general string layer doubles it).
    fn expected_exec_line(binary: &Path) -> String {
        let path = binary.to_str().expect("the test path is UTF-8");
        let mut value = String::from("\"");
        for ch in path.chars() {
            match ch {
                '\\' => value.push_str(r"\\\\"),
                '"' => value.push_str(r#"\\""#),
                '`' => value.push_str(r"\\`"),
                '$' => value.push_str(r"\\$"),
                other => value.push(other),
            }
        }
        value.push('"');
        format!("Exec={value}")
    }

    /// The fixed installed paths of a prefix: binary, five icons, entry and
    /// manifest. Keep in step with `tools/install-linux-desktop.sh`.
    fn fixed_install_paths(prefix: &Path) -> (PathBuf, Vec<PathBuf>, PathBuf, PathBuf) {
        let binary = prefix.join("bin/rift-music");
        let icons = ICON_SIZES
            .iter()
            .map(|size| {
                prefix.join(format!(
                    "share/icons/hicolor/{size}/apps/{}.png",
                    rift_desktop::BINARY_NAME
                ))
            })
            .collect();
        let entry = prefix.join("share/applications/Rift.desktop");
        let manifest = prefix.join("share/rift-desktop/install-manifest");
        (binary, icons, entry, manifest)
    }

    /// Read `(contents, mode)` for every path, so a test can prove a rollback
    /// restored the previous install byte-for-byte and mode-for-mode rather than
    /// merely recreating files with the right names.
    fn snapshot(paths: &[PathBuf]) -> Vec<(Vec<u8>, u32)> {
        paths
            .iter()
            .map(|path| (file_bytes(path), file_mode(path)))
            .collect()
    }

    fn file_bytes(path: &Path) -> Vec<u8> {
        std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
    }

    fn file_mode(path: &Path) -> u32 {
        std::fs::metadata(path)
            .unwrap_or_else(|error| panic!("stat {}: {error}", path.display()))
            .permissions()
            .mode()
            & 0o777
    }

    fn assert_snapshot_unchanged(paths: &[PathBuf], expected: &[(Vec<u8>, u32)], what: &str) {
        assert_eq!(paths.len(), expected.len(), "{what}: incomplete snapshot");
        for (path, (bytes, mode)) in paths.iter().zip(expected) {
            assert_eq!(
                file_bytes(path),
                *bytes,
                "{what}: {} must be restored byte-for-byte",
                path.display()
            );
            assert_eq!(
                file_mode(path),
                *mode,
                "{what}: {} must keep its previous mode",
                path.display()
            );
        }
    }

    /// No hidden staging or backup file may survive in any destination
    /// directory, whether the transaction committed or rolled back.
    fn assert_no_staging_or_backup_files(prefix: &Path) {
        let (binary, icons, entry, manifest) = fixed_install_paths(prefix);
        let mut dirs = vec![
            binary.parent().expect("binary parent").to_path_buf(),
            entry.parent().expect("entry parent").to_path_buf(),
            manifest.parent().expect("manifest parent").to_path_buf(),
        ];
        for icon in &icons {
            dirs.push(icon.parent().expect("icon parent").to_path_buf());
        }
        dirs.sort();
        dirs.dedup();
        for dir in dirs {
            if dir.is_dir() {
                assert_no_staged_files(&dir);
            }
        }
    }

    /// End-to-end check against a throwaway prefix: the installed entry's `Exec`
    /// has the exact two-stage encoding, the icons, binary and manifest land
    /// where expected, and `--uninstall` removes only its own files.
    #[test]
    fn the_installer_round_trips_through_a_throwaway_prefix() {
        let root = temp_root("roundtrip");
        // Every reserved character is deliberate: `$`, the backtick, the double
        // quote and the backslash exercise both escaping layers, and the space
        // checks the unconditional double-quoting. `%` is absent because the
        // installer rejects it (see the dedicated test below).
        let prefix = root.join("with space $dollar `tick` \"quote\" back\\slash");
        let binary = Path::new(BINARY);

        // `--dry-run` must be a genuine no-op, and the round-trip below skips the
        // desktop/icon cache refresh so the test cannot rebuild a developer's real
        // KDE service cache or GTK icon cache.
        let dry_run = run_ok(
            {
                let mut command = script_command();
                command
                    .args(["--dry-run", "--no-cache-refresh", "--prefix"])
                    .arg(&prefix)
                    .arg("--binary")
                    .arg(binary);
                command
            },
            "installer dry-run",
        );
        assert!(
            !prefix.exists(),
            "--dry-run must not create the install prefix"
        );
        assert!(
            String::from_utf8_lossy(&dry_run.stdout).contains("would:"),
            "--dry-run must report the operations it would perform"
        );

        run_ok(install_command(&prefix, binary), "installer");

        let installed_binary = prefix.join("bin/rift-music");
        assert!(
            installed_binary.is_file(),
            "the installer must install the binary"
        );
        let entry_path = prefix.join("share/applications/Rift.desktop");
        let installed_entry = std::fs::read_to_string(&entry_path)
            .expect("the installer must write the desktop entry");
        let fields = key_values(&installed_entry);
        let exec = fields
            .get("Exec")
            .copied()
            .expect("installed entry has Exec");
        let exec_line = installed_entry
            .lines()
            .find(|line| line.starts_with("Exec="))
            .expect("installed entry has an Exec line");
        assert_eq!(
            exec_line,
            expected_exec_line(&installed_binary),
            "the raw Exec encoding must be the exact two-stage Desktop Entry escaping"
        );
        assert_eq!(
            decode_exec(exec),
            installed_binary.to_str().expect("temp path is UTF-8"),
            "the installed Exec must decode back to the installed binary"
        );
        // The third identity name: the desktop-file ID is the entry's file
        // stem, and the compositor matches it against the window's
        // app_id/WM_CLASS, so a drift here would detach the window from the
        // launcher.
        assert_eq!(
            entry_path.file_stem().and_then(|stem| stem.to_str()),
            Some(rift_desktop::APP_ID),
            "the installed entry must be named <APP_ID>.desktop"
        );
        assert_eq!(
            entry_path
                .extension()
                .and_then(|extension| extension.to_str()),
            Some("desktop"),
            "the installed entry must keep its .desktop extension"
        );
        // Derive the installed icon name from the entry instead of hardcoding it:
        // an `Icon=`/asset-name drift must fail the round-trip, not just the field
        // check above.
        let icon_name = fields
            .get("Icon")
            .copied()
            .expect("installed entry has Icon");
        assert_eq!(icon_name, rift_desktop::BINARY_NAME);
        for size in ICON_SIZES {
            let icon = prefix.join(format!("share/icons/hicolor/{size}/apps/{icon_name}.png"));
            assert!(icon.is_file(), "missing installed icon {}", icon.display());
            let (width, height) = png_dimensions(&icon);
            let expected = theme_size(size);
            assert_eq!(
                (width, height),
                (expected, expected),
                "installed icon {} is {width}x{height}, but its hicolor slot is {size}",
                icon.display()
            );
        }

        // The manifest must list exactly the files the install created (the
        // binary, the entry and the five icons), and nothing else.
        let manifest_path = prefix.join("share/rift-desktop/install-manifest");
        let manifest = std::fs::read_to_string(&manifest_path)
            .expect("the installer must write the install manifest");
        let mut listed: Vec<PathBuf> = manifest
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(PathBuf::from)
            .collect();
        let mut expected = vec![installed_binary.clone(), entry_path.clone()];
        for size in ICON_SIZES {
            expected.push(prefix.join(format!("share/icons/hicolor/{size}/apps/{icon_name}.png")));
        }
        listed.sort();
        expected.sort();
        assert_eq!(
            listed, expected,
            "the manifest must list exactly the installed files"
        );

        // Files the installer did not create must survive the uninstall.
        let decoy_binary = prefix.join("bin/other-tool");
        let decoy_entry = prefix.join("share/applications/Other.desktop");
        std::fs::write(&decoy_binary, b"not rift\n").expect("write the decoy binary");
        std::fs::write(&decoy_entry, b"[Desktop Entry]\nName=Other\n")
            .expect("write the decoy entry");
        // A tampered or stale manifest must not be able to reach outside the
        // installer's bin/data roots, even when the line is syntactically valid.
        let outside = root.join("outside-the-prefix");
        std::fs::write(&outside, b"keep\n").expect("write a file outside the prefix");
        let mut tampered = manifest;
        tampered.push_str(&format!("{}\n", outside.display()));
        std::fs::write(&manifest_path, tampered).expect("append a path outside the install roots");

        // An uninstall dry-run is also side-effect free.
        let uninstall_dry_run = run_ok(
            {
                let mut command = script_command();
                command
                    .args(["--uninstall", "--dry-run", "--no-cache-refresh", "--prefix"])
                    .arg(&prefix);
                command
            },
            "uninstaller dry-run",
        );
        assert!(
            String::from_utf8_lossy(&uninstall_dry_run.stdout).contains("would:"),
            "the uninstall dry-run must report the operations it would perform"
        );
        assert!(
            installed_binary.is_file() && entry_path.is_file() && manifest_path.is_file(),
            "an uninstall dry-run must not remove anything"
        );

        let uninstall = run_ok(uninstall_command(&prefix), "uninstaller");
        assert!(
            String::from_utf8_lossy(&uninstall.stderr)
                .contains("skipping manifest path outside the install roots"),
            "uninstall must warn about manifest paths outside its install roots"
        );
        assert!(
            !installed_binary.exists(),
            "uninstall must remove the binary it installed"
        );
        assert!(
            !entry_path.exists(),
            "uninstall must remove the entry it installed"
        );
        for size in ICON_SIZES {
            let icon = prefix.join(format!("share/icons/hicolor/{size}/apps/{icon_name}.png"));
            assert!(!icon.exists(), "uninstall must remove {}", icon.display());
        }
        assert!(
            !manifest_path.exists(),
            "uninstall must remove the manifest last, after the files it lists"
        );
        assert_eq!(
            std::fs::read(&decoy_binary).expect("read the decoy binary"),
            b"not rift\n".to_vec(),
            "uninstall must not remove files it did not install"
        );
        assert!(
            decoy_entry.is_file(),
            "uninstall must not remove an unrelated entry"
        );
        assert_eq!(
            std::fs::read(&outside).expect("read the outside file"),
            b"keep\n".to_vec(),
            "uninstall must ignore manifest paths outside its install roots"
        );

        // A second uninstall has no manifest and nothing left to remove: it is a
        // successful no-op, not a refusal.
        run_ok(
            uninstall_command(&prefix),
            "second (idempotent) uninstaller",
        );
        assert!(
            decoy_binary.is_file() && decoy_entry.is_file(),
            "a repeated uninstall must still leave unrelated files alone"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A `desktop-file-validate` rejection must not publish anything: the
    /// installer stages the binary, icons and entry under temporary names in
    /// their destination directories, validates the staged entry, and only then
    /// renames the files into place and writes the manifest. A failing stub
    /// validator on `PATH` (shadowing the real tool, if installed) proves a
    /// failed install leaves a previous install byte-for-byte intact, publishes
    /// no icon and removes its staging files, and that a fresh prefix receives
    /// no entry, manifest or binary at all.
    #[test]
    fn rejected_entry_leaves_the_previous_install_untouched() {
        let root = temp_root("validation-rollback");
        let prefix = root.join("prefix");
        let sentinel_binary = prefix.join("bin/rift-music");
        let sentinel_entry = prefix.join("share/applications/Rift.desktop");
        let sentinel_manifest = prefix.join("share/rift-desktop/install-manifest");
        let previous = [
            (sentinel_binary.clone(), "previous binary\n"),
            (sentinel_entry.clone(), "previous entry\n"),
            (sentinel_manifest.clone(), "previous manifest\n"),
        ];
        for (path, contents) in &previous {
            std::fs::create_dir_all(path.parent().expect("sentinel parent"))
                .expect("create the sentinel directory");
            std::fs::write(path, *contents).expect("write the previous install file");
        }

        // Shadow the real validator (whether or not it is installed) with one
        // that rejects every entry.
        let stub_dir = root.join("stub-bin");
        std::fs::create_dir_all(&stub_dir).expect("create the stub directory");
        let stub = stub_dir.join("desktop-file-validate");
        std::fs::write(&stub, b"#!/bin/sh\nexit 1\n").expect("write the stub validator");
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .expect("make the stub validator executable");

        let rejected = run_err(
            with_stub_path(install_command(&prefix, Path::new(BINARY)), &stub_dir),
            "install with a rejecting desktop-file-validate",
        );
        let stderr = String::from_utf8_lossy(&rejected.stderr);
        assert!(
            stderr.contains("desktop-file-validate rejected"),
            "the installer must surface the validation failure:\n{stderr}"
        );

        // The previous binary, entry and manifest are exactly as they were:
        // validation runs before anything is published, so nothing is replaced
        // and the manifest never claims the rejected entry.
        for (path, contents) in &previous {
            assert_eq!(
                std::fs::read_to_string(path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", path.display())),
                *contents,
                "a rejected entry must not replace {}",
                path.display()
            );
        }
        // The previously absent icons stay absent.
        for size in ICON_SIZES {
            let icon = prefix.join(format!(
                "share/icons/hicolor/{size}/apps/{}.png",
                rift_desktop::BINARY_NAME
            ));
            assert!(
                !icon.exists(),
                "a rejected entry must not publish the icon {}",
                icon.display()
            );
        }
        // The EXIT trap removes the staged temp files, so a retry (or a plain
        // direct run) does not trip over a leftover binary/entry/icon copy.
        for dir in [
            prefix.join("bin"),
            prefix.join("share/applications"),
            prefix.join("share/icons/hicolor/32x32/apps"),
            prefix.join("share/rift-desktop"),
        ] {
            assert_no_staged_files(&dir);
        }

        // In a fresh prefix a failed install must not leave a new entry, a
        // manifest, an icon or any unowned binary behind either.
        let fresh = root.join("fresh-prefix");
        run_err(
            with_stub_path(install_command(&fresh, Path::new(BINARY)), &stub_dir),
            "install into a fresh prefix with a rejecting desktop-file-validate",
        );
        assert!(
            !fresh.join("share/applications/Rift.desktop").exists(),
            "a rejected entry must not be published"
        );
        assert!(
            !fresh.join("share/rift-desktop/install-manifest").exists(),
            "a rejected install must not write a manifest"
        );
        assert!(
            !fresh.join("bin/rift-music").exists(),
            "a rejected install must not publish the binary"
        );
        for size in ICON_SIZES {
            let icon = fresh.join(format!(
                "share/icons/hicolor/{size}/apps/{}.png",
                rift_desktop::BINARY_NAME
            ));
            assert!(
                !icon.exists(),
                "a rejected entry must not publish the icon {}",
                icon.display()
            );
        }
        for dir in [
            fresh.join("bin"),
            fresh.join("share/applications"),
            fresh.join("share/icons/hicolor/32x32/apps"),
        ] {
            assert_no_staged_files(&dir);
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A failure after validation but midway through publication must restore
    /// the previous install exactly. The entry destination is replaced with a
    /// non-empty directory after a v1 install: the binary and icons are
    /// published before the entry, so the entry publish fails only after they
    /// were already renamed into place. The rollback must remove the v2 files,
    /// restore the v1 binary/icons (bytes and modes), leave the injected entry
    /// path alone, keep the v1 manifest byte-identical, and remove every
    /// staging/backup file.
    #[test]
    fn failed_publication_restores_the_previous_install() {
        let root = temp_root("publication-rollback");
        let prefix = root.join("prefix");
        let (installed_binary, icon_paths, entry_path, manifest_path) =
            fixed_install_paths(&prefix);

        let v1_binary = root.join("v1-rift-music");
        std::fs::write(&v1_binary, b"v1 binary payload\n").expect("write the v1 binary");
        run_ok(install_command(&prefix, &v1_binary), "v1 install");

        // The entry is deliberately excluded from the snapshot: the injection
        // below replaces it. Everything else must come back byte- and
        // mode-identical.
        let mut previous_files = vec![installed_binary.clone()];
        previous_files.extend(icon_paths.iter().cloned());
        previous_files.push(manifest_path.clone());
        let previous = snapshot(&previous_files);

        // Deterministic failure without root: the entry destination is now a
        // non-empty directory, which a file publish must never replace (and
        // which the installer did not create, so it must survive untouched).
        std::fs::remove_file(&entry_path).expect("remove the v1 entry");
        std::fs::create_dir_all(entry_path.join("keep")).expect("create the injected directory");
        std::fs::write(entry_path.join("keep/marker"), b"injected directory\n")
            .expect("write the injected directory marker");

        let v2_binary = root.join("v2-rift-music");
        std::fs::write(&v2_binary, b"v2 binary payload\n").expect("write the v2 binary");
        let failed = run_err(
            install_command(&prefix, &v2_binary),
            "v2 install with a directory at the entry destination",
        );
        let stderr = String::from_utf8_lossy(&failed.stderr);
        assert!(
            stderr.contains("the destination is a directory"),
            "the failure must name the foreign directory:\n{stderr}"
        );
        assert!(
            stderr.contains("rolled back"),
            "the failure must report the rollback:\n{stderr}"
        );

        assert_snapshot_unchanged(&previous_files, &previous, "a failed reinstall");
        assert!(
            entry_path.is_dir(),
            "the injected non-empty entry directory must be left in place"
        );
        assert_eq!(
            std::fs::read(entry_path.join("keep/marker")).expect("read the injected marker"),
            b"injected directory\n".to_vec(),
            "the rollback must not delete the foreign entry directory"
        );
        assert_ne!(
            file_bytes(&installed_binary),
            b"v2 binary payload\n".to_vec(),
            "the failed v2 binary must not stay published"
        );
        assert_no_staging_or_backup_files(&prefix);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A first-time install that fails after some files were published must
    /// leave no binary, icon or manifest behind. The only things that may
    /// remain are the injected foreign directory and empty directories the
    /// staging created, so a retry starts from a clean prefix.
    #[test]
    fn failed_first_install_publishes_nothing() {
        let root = temp_root("first-install-rollback");
        let prefix = root.join("prefix");
        let (installed_binary, icon_paths, entry_path, manifest_path) =
            fixed_install_paths(&prefix);

        std::fs::create_dir_all(&entry_path).expect("create the injected entry directory");
        std::fs::write(entry_path.join("keep"), b"foreign directory\n")
            .expect("write the injected directory marker");

        let binary = root.join("first-install-rift-music");
        std::fs::write(&binary, b"first install payload\n").expect("write the install source");
        let failed = run_err(
            install_command(&prefix, &binary),
            "first install with a directory at the entry destination",
        );
        let stderr = String::from_utf8_lossy(&failed.stderr);
        assert!(
            stderr.contains("rolled back"),
            "the failure must report the rollback:\n{stderr}"
        );

        assert!(
            !installed_binary.exists(),
            "a failed first install must not publish the binary"
        );
        for icon in &icon_paths {
            assert!(
                !icon.exists(),
                "a failed first install must not publish {}",
                icon.display()
            );
        }
        assert!(
            !manifest_path.exists(),
            "a failed first install must not write a manifest"
        );
        assert!(
            !manifest_path.parent().expect("manifest parent").exists(),
            "a failed first install must not claim a manifest directory"
        );
        assert!(
            entry_path.is_dir() && entry_path.join("keep").is_file(),
            "the injected foreign entry directory must survive"
        );
        assert_no_staging_or_backup_files(&prefix);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The manifest rename is the commit point: until it succeeds the install is
    /// not valid, so a failure there must roll every already-published file back
    /// too. A stub `mv` on `PATH` that delegates everything except the final
    /// manifest rename injects that failure deterministically without root; the
    /// previous binary, icons, entry and manifest must all survive untouched.
    #[test]
    fn failed_manifest_write_rolls_back_published_files() {
        let root = temp_root("manifest-rollback");
        let prefix = root.join("prefix");
        let (installed_binary, icon_paths, entry_path, manifest_path) =
            fixed_install_paths(&prefix);

        let v1_binary = root.join("v1-rift-music");
        std::fs::write(&v1_binary, b"v1 binary payload\n").expect("write the v1 binary");
        run_ok(install_command(&prefix, &v1_binary), "v1 install");

        let mut previous_files = vec![
            installed_binary.clone(),
            entry_path.clone(),
            manifest_path.clone(),
        ];
        previous_files.extend(icon_paths.iter().cloned());
        let previous = snapshot(&previous_files);

        // Resolve the real mv before the stub shadows it on the child's PATH;
        // `command -v mv` inside the stub would find the stub itself.
        let real_mv = {
            let output = Command::new("sh")
                .args(["-c", "command -v mv"])
                .output()
                .expect("resolve mv through the shell");
            assert!(output.status.success(), "mv must be available on PATH");
            let path = String::from_utf8(output.stdout).expect("mv path is UTF-8");
            let path = PathBuf::from(path.trim());
            assert!(
                path.is_absolute(),
                "expected an absolute mv path, got {}",
                path.display()
            );
            path
        };
        let stub_dir = root.join("stub-bin");
        std::fs::create_dir_all(&stub_dir).expect("create the stub directory");
        let stub = stub_dir.join("mv");
        std::fs::write(
            &stub,
            br#"#!/bin/sh
target=
for argument in "$@"; do target=$argument; done
case "$target" in
    */rift-desktop/install-manifest) exit 1 ;;
esac
exec "$RIFT_TEST_REAL_MV" "$@"
"#,
        )
        .expect("write the mv stub");
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .expect("make the mv stub executable");

        let v2_binary = root.join("v2-rift-music");
        std::fs::write(&v2_binary, b"v2 binary payload\n").expect("write the v2 binary");
        let mut command = with_stub_path(install_command(&prefix, &v2_binary), &stub_dir);
        command.env("RIFT_TEST_REAL_MV", &real_mv);
        let failed = run_err(command, "v2 install with a failing manifest rename");
        let stderr = String::from_utf8_lossy(&failed.stderr);
        assert!(
            stderr.contains("could not write the install manifest"),
            "the failure must name the manifest write:\n{stderr}"
        );
        assert!(
            stderr.contains("rolled back"),
            "the failure must report the rollback:\n{stderr}"
        );

        assert_snapshot_unchanged(&previous_files, &previous, "a failed manifest write");
        assert_no_staging_or_backup_files(&prefix);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Without a manifest, a prefix containing files at Rift's fixed paths is
    /// ambiguous: those may be a hand-installed copy, not this installer's. The
    /// uninstall must refuse (preserving them) unless `--force` explicitly opts
    /// into touching only the fixed known paths.
    #[test]
    fn uninstall_without_a_manifest_refuses_to_remove_foreign_files() {
        let root = temp_root("uninstall-refusal");
        let prefix = root.join("prefix");
        let fixed_binary = prefix.join("bin/rift-music");
        let fixed_entry = prefix.join("share/applications/Rift.desktop");
        std::fs::create_dir_all(fixed_binary.parent().expect("binary parent"))
            .expect("create the bin directory");
        std::fs::create_dir_all(fixed_entry.parent().expect("entry parent"))
            .expect("create the applications directory");
        std::fs::write(&fixed_binary, b"#!/bin/sh\necho not rift\n")
            .expect("write the foreign binary");
        std::fs::write(&fixed_entry, b"[Desktop Entry]\nName=Not Rift\n")
            .expect("write the foreign entry");

        let refused = run_err(uninstall_command(&prefix), "uninstall without a manifest");
        let stderr = String::from_utf8_lossy(&refused.stderr);
        assert!(
            stderr.contains("--force"),
            "the refusal must tell the user how to proceed:\n{stderr}"
        );
        assert_eq!(
            std::fs::read(&fixed_binary).expect("read the foreign binary"),
            b"#!/bin/sh\necho not rift\n".to_vec(),
            "a refused uninstall must preserve the foreign binary"
        );
        assert!(
            fixed_entry.is_file(),
            "a refused uninstall must preserve the foreign entry"
        );

        // Even with --force, --dry-run must change nothing.
        let forced_dry_run = run_ok(
            {
                let mut command = script_command();
                command
                    .args([
                        "--uninstall",
                        "--dry-run",
                        "--force",
                        "--no-cache-refresh",
                        "--prefix",
                    ])
                    .arg(&prefix);
                command
            },
            "forced dry-run uninstall",
        );
        assert!(
            String::from_utf8_lossy(&forced_dry_run.stdout).contains("would:"),
            "a forced dry-run must report the fixed paths it would remove"
        );
        assert!(
            fixed_binary.is_file() && fixed_entry.is_file(),
            "a forced dry-run must not remove anything"
        );

        // --force is the recovery path: it removes only the fixed paths, never
        // arbitrary files next to them.
        let unrelated = prefix.join("bin/other-tool");
        std::fs::write(&unrelated, b"keep\n").expect("write an unrelated tool");
        let forced = run_ok(
            {
                let mut command = script_command();
                command
                    .args(["--uninstall", "--force", "--no-cache-refresh", "--prefix"])
                    .arg(&prefix);
                command
            },
            "forced uninstall",
        );
        assert!(
            String::from_utf8_lossy(&forced.stderr).contains("warning:"),
            "a forced uninstall without a manifest must print a warning"
        );
        assert!(
            !fixed_binary.exists() && !fixed_entry.exists(),
            "--force must remove the fixed install paths"
        );
        assert_eq!(
            std::fs::read(&unrelated).expect("read the unrelated tool"),
            b"keep\n".to_vec(),
            "--force must not remove anything outside the fixed paths"
        );

        // And the uninstall is idempotent afterwards.
        run_ok(uninstall_command(&prefix), "uninstall after --force");
        assert!(
            unrelated.is_file(),
            "a repeated uninstall must not touch unrelated files"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The installer deliberately does not implement Exec's `%%` escape, so a
    /// `%` in the install path cannot be
    /// represented by the launcher. Fail before writing anything, with an error
    /// that names the problem, instead of installing an entry that never opens.
    #[test]
    fn installer_rejects_percent_in_the_install_path() {
        let root = temp_root("percent");
        let prefix = root.join("100% rift");
        let output = run_err(
            install_command(&prefix, Path::new(BINARY)),
            "install into a path containing %",
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("contains '%'"),
            "the error must name the offending '%':\n{stderr}"
        );
        assert!(
            !prefix.exists(),
            "a rejected install path must not be created"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A downloaded release asset is commonly mode 0644. An explicit --binary
    /// only has to be a readable regular file; `install -Dm755` sets the mode,
    /// just as for a local build output.
    #[test]
    fn explicit_binary_does_not_need_the_execute_bit() {
        let root = temp_root("non-executable-binary");
        let source = root.join("downloaded-rift-music");
        std::fs::copy(BINARY, &source).expect("copy the test binary");
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o644))
            .expect("make the source non-executable");
        assert_eq!(
            std::fs::metadata(&source)
                .expect("stat the source")
                .permissions()
                .mode()
                & 0o111,
            0,
            "the test source must start without an execute bit"
        );

        let prefix = root.join("prefix");
        run_ok(
            install_command(&prefix, &source),
            "installer with a non-executable source",
        );
        let installed = prefix.join("bin/rift-music");
        assert!(
            installed.is_file(),
            "the installer must install the non-executable source"
        );
        assert_eq!(
            std::fs::metadata(&installed)
                .expect("stat the installed binary")
                .permissions()
                .mode()
                & 0o777,
            0o755,
            "install -Dm755 must set the installed mode"
        );
        assert_eq!(
            std::fs::metadata(&source)
                .expect("stat the source after install")
                .permissions()
                .mode()
                & 0o111,
            0,
            "the source mode must be left alone"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// With no --prefix the XDG base directories must be honoured (binary in
    /// XDG_BIN_HOME, entry/icons/manifest in XDG_DATA_HOME), falling back to
    /// $HOME/.local when they are unset; an explicit --prefix still means
    /// <prefix>/bin + <prefix>/share and wins over the XDG variables.
    #[test]
    fn default_install_uses_the_xdg_dirs_and_prefix_still_wins() {
        let root = temp_root("xdg");
        let home = root.join("home");
        let data = root.join("data");
        let bin = root.join("bin");
        for dir in [&home, &data, &bin] {
            std::fs::create_dir_all(dir).expect("create the XDG directory");
        }

        let xdg_install = run_ok(
            {
                let mut command = script_command();
                command
                    .env("HOME", &home)
                    .env("XDG_DATA_HOME", &data)
                    .env("XDG_BIN_HOME", &bin)
                    .args(["--no-cache-refresh", "--binary"])
                    .arg(BINARY);
                command
            },
            "XDG install",
        );
        assert!(
            String::from_utf8_lossy(&xdg_install.stdout).contains("Installed:"),
            "the install must report what it wrote"
        );

        let installed_binary = bin.join("rift-music");
        let entry_path = data.join("applications/Rift.desktop");
        let manifest_path = data.join("rift-desktop/install-manifest");
        assert!(
            installed_binary.is_file(),
            "XDG_BIN_HOME must receive the binary"
        );
        assert!(
            entry_path.is_file(),
            "XDG_DATA_HOME must receive the desktop entry"
        );
        assert!(
            manifest_path.is_file(),
            "XDG_DATA_HOME must receive the install manifest"
        );
        for size in ICON_SIZES {
            let icon = data.join(format!(
                "icons/hicolor/{size}/apps/{}.png",
                rift_desktop::BINARY_NAME
            ));
            assert!(
                icon.is_file(),
                "XDG_DATA_HOME must receive {}",
                icon.display()
            );
        }
        assert!(
            !home.join(".local").exists(),
            "setting XDG_DATA_HOME/XDG_BIN_HOME must bypass $HOME/.local"
        );
        let installed_entry =
            std::fs::read_to_string(&entry_path).expect("read the XDG-installed entry");
        let exec_line = installed_entry
            .lines()
            .find(|line| line.starts_with("Exec="))
            .expect("the XDG-installed entry has an Exec line");
        assert_eq!(
            exec_line,
            expected_exec_line(&installed_binary),
            "an XDG install still writes the exact Exec encoding"
        );

        run_ok(
            {
                let mut command = script_command();
                command
                    .env("HOME", &home)
                    .env("XDG_DATA_HOME", &data)
                    .env("XDG_BIN_HOME", &bin)
                    .args(["--uninstall", "--no-cache-refresh"]);
                command
            },
            "XDG uninstall",
        );
        assert!(
            !installed_binary.exists(),
            "an XDG uninstall must remove the binary from XDG_BIN_HOME"
        );
        assert!(
            !entry_path.exists() && !manifest_path.exists(),
            "an XDG uninstall must remove the entry and manifest from XDG_DATA_HOME"
        );

        // An explicit --prefix wins over both XDG variables, for install and
        // uninstall alike.
        let prefix = root.join("explicit-prefix");
        run_ok(
            {
                let mut command = script_command();
                command
                    .env("HOME", &home)
                    .env("XDG_DATA_HOME", &data)
                    .env("XDG_BIN_HOME", &bin)
                    .args(["--no-cache-refresh", "--prefix"])
                    .arg(&prefix)
                    .arg("--binary")
                    .arg(BINARY);
                command
            },
            "explicit-prefix install",
        );
        let explicit_binary = prefix.join("bin/rift-music");
        let explicit_entry = prefix.join("share/applications/Rift.desktop");
        let explicit_manifest = prefix.join("share/rift-desktop/install-manifest");
        assert!(
            explicit_binary.is_file() && explicit_entry.is_file() && explicit_manifest.is_file(),
            "an explicit --prefix must use <prefix>/bin and <prefix>/share"
        );
        assert!(
            !bin.join("rift-music").exists(),
            "an explicit --prefix must not write the XDG bin directory"
        );
        assert!(
            !data.join("applications/Rift.desktop").exists(),
            "an explicit --prefix must not write the XDG data directory"
        );

        run_ok(
            {
                let mut command = script_command();
                command
                    .env("HOME", &home)
                    .env("XDG_DATA_HOME", &data)
                    .env("XDG_BIN_HOME", &bin)
                    .args(["--uninstall", "--no-cache-refresh", "--prefix"])
                    .arg(&prefix);
                command
            },
            "explicit-prefix uninstall",
        );
        assert!(
            !explicit_binary.exists() && !explicit_entry.exists() && !explicit_manifest.exists(),
            "an explicit-prefix uninstall must remove exactly the prefixed install"
        );

        // With the XDG variables unset, the documented $HOME/.local fallbacks
        // must be used (and uninstall must resolve the same paths).
        let home_only = root.join("home-only");
        std::fs::create_dir_all(&home_only).expect("create the fallback home");
        run_ok(
            {
                let mut command = script_command();
                command
                    .env("HOME", &home_only)
                    .env_remove("XDG_DATA_HOME")
                    .env_remove("XDG_BIN_HOME")
                    .args(["--no-cache-refresh", "--binary"])
                    .arg(BINARY);
                command
            },
            "HOME-fallback install",
        );
        assert!(
            home_only.join(".local/bin/rift-music").is_file(),
            "the default binary location must be $HOME/.local/bin"
        );
        assert!(
            home_only
                .join(".local/share/applications/Rift.desktop")
                .is_file(),
            "the default entry location must be $HOME/.local/share/applications"
        );
        assert!(
            home_only
                .join(".local/share/rift-desktop/install-manifest")
                .is_file(),
            "the default manifest location must be $HOME/.local/share/rift-desktop"
        );
        run_ok(
            {
                let mut command = script_command();
                command
                    .env("HOME", &home_only)
                    .env_remove("XDG_DATA_HOME")
                    .env_remove("XDG_BIN_HOME")
                    .args(["--uninstall", "--no-cache-refresh"]);
                command
            },
            "HOME-fallback uninstall",
        );
        assert!(
            !home_only.join(".local/bin/rift-music").exists(),
            "the HOME-fallback uninstall must resolve the same paths"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
