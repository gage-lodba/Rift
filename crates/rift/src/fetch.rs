//! Playback audio fetching: resolution and streaming are native.
//!
//! Rift resolves a `YouTube` video ID to a directly downloadable audio URL with
//! the JS-less `VISIONOS` innertube client and streams it over ranged HTTP (the
//! crate-private `stream` module). There is no external downloader binary and no
//! fallback pipeline: the pre-native implementation shelled out to yt-dlp, and it
//! was removed once the native path shipped (`docs/native-streaming.md`, Phase 5).
//!
//! [`fetch_track`] is the playback entry point: it prefers an offline copy under
//! the downloads directory and otherwise fetches the stream over the network
//! through [`fetch_bytes`]. Offline files are named with [`safe_stem`]; the
//! pre-P5 `_`-folding is frozen in [`legacy_safe_stem`] so an old file stays
//! readable after the naming fix.

use anyhow::{bail, Context};
use tracing::{debug, info};

/// A byte written literally by [`safe_stem`]: lowercase ASCII letters, digits,
/// and the two separators real `YouTube` ids use. Everything else is escaped.
/// Uppercase is deliberately *not* literal: on a case-insensitive filesystem
/// `AbC` and `abc` would otherwise share one file.
fn is_plain_byte(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
}

const HEX_LOWER: [u8; 16] = *b"0123456789abcdef";
/// Deliberately uppercase: a prefix + `!` + these may never be decoded back, so
/// a capped name can never be mistaken for a different id's canonical encoding.
/// Uppercase is outside the literal alphabet above, so the whole suffix is
/// structurally undecodable (an all-digit hash would otherwise look like an
/// escape sequence followed by literal digits).
const HEX_UPPER: [u8; 16] = *b"0123456789ABCDEF";

/// Longest stem [`safe_stem`] emits. All three target filesystems accept longer
/// names (255 bytes), but staying at 120 leaves room for `.m4a.part` and keeps a
/// single component away from the limit; the budget below also leaves the
/// non-UTF-8/byte-length edge cases far behind.
const MAX_STEM_LEN: usize = 120;

/// `!` + 16 uppercase hex digits: the marker plus a 64-bit FNV-1a of the full
/// id. See [`hashed_stem`].
const HASH_SUFFIX_LEN: usize = 17;

/// Stand-in for an empty id. `~` alone is not a valid escape (there are not two
/// hex digits after it), so it cannot collide with any non-empty id's encoding;
/// [`decode_stem`] maps it back to the empty id explicitly.
const EMPTY_ID_STEM: &str = "~";

/// The downloads-directory filename stem for a video/track id.
///
/// The old implementation folded every disallowed character to `_`. That was
/// path-safe but not injective: `a/b` and `a_b` (or `..` and `__`) shared one
/// file, an old file could not be mapped back to its id, and case-only ids
/// collided on Windows/macOS. This encoding keeps the useful property — the
/// output alphabet is `[a-z0-9_-]` plus the `~` escape — while being injective,
/// case-insensitively unique and decodable:
///
/// - a byte in `[a-z0-9_-]` is literal; every other byte is `~` plus two
///   lowercase hex digits. `a/b` becomes `a~2fb`, `..` becomes `~2e~2e`, and a
///   non-ASCII id is escaped per UTF-8 byte, so the name stays valid UTF-8.
/// - `~` itself is escaped (`~7e`), which is what makes the mapping reversible.
/// - uppercase bytes are escaped too (`AbC` -> `~41b~43`), so two ids that
///   differ only by case get names that also differ after case folding. The
///   encoding is byte-exact, not case-folded.
/// - the alphabet has no separator, dot or space, so a hostile id from an
///   imported JSON cannot escape the downloads directory (`..` is escaped, not
///   copied) and no name can acquire a trailing dot/space that Windows strips.
///
/// Two additional cases are handled after encoding:
///
/// - Windows reserved device names (`con`, `nul`, `com1`, …) are neutralized by
///   escaping their *first* byte, e.g. `con` -> `~63on`. That spelling is not
///   the canonical encoding of any other id (`~63` decodes to `c`, whose normal
///   encoding is literal), so injectivity is preserved, and a name with an
///   extension cannot arise because `.` is always escaped.
/// - a stem longer than `MAX_STEM_LEN` is replaced by a truncated prefix plus
///   `HASH_SUFFIX_LEN` characters of stable FNV-1a hash (`!` + 16 uppercase
///   hex). The prefix keeps names recognizable in a file manager, and the hash
///   keeps overlong ids distinct. The `!` + uppercase form cannot be produced by
///   escaping, so `Downloads` never guesses an id from a capped name; it stores
///   the mapping in `downloads/.names.json`.
///
/// The only theoretical collision is the 64-bit hash of two overlong ids that
/// also share the kept prefix. Normal ids are exact.
#[must_use]
pub fn safe_stem(id: &str) -> String {
    if id.is_empty() {
        return EMPTY_ID_STEM.to_owned();
    }
    let mut encoded = String::with_capacity(id.len());
    for &byte in id.as_bytes() {
        push_encoded_byte(&mut encoded, byte);
    }
    if encoded.len() > MAX_STEM_LEN {
        return hashed_stem(id);
    }
    if is_windows_device_name(&encoded) {
        return escape_first_byte(&encoded);
    }
    encoded
}

/// The exact filename stem Rift used before the P5 naming fix: every character
/// outside `[A-Za-z0-9_-]` folds to `_`, case is preserved, nothing is capped,
/// and reserved device names are not handled.
///
/// Frozen on purpose. Existing installs have `<legacy_safe_stem(id)>.m4a` files;
/// `Downloads::load` records every non-empty `.m4a` stem it cannot decode and
/// `Downloads::is_downloaded`/[`fetch_track`] consult this fallback, so an old
/// file remains playable without being renamed or deleted. New files always use
/// [`safe_stem`]. Do not use this to write.
#[must_use]
pub fn legacy_safe_stem(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "_".into()
    } else {
        cleaned
    }
}

/// Recover the id from an on-disk stem produced by [`safe_stem`], or `None` when
/// the stem is not a canonical current-scheme name.
///
/// The canonical check is what keeps this from "guessing": an over-escaped
/// spelling (`~61bc` for `abc`), a pre-P5 uppercase or `_`-folded name, an
/// arbitrary foreign file and a capped/hashed name (structurally undecodable,
/// mapped only through the downloads name index) all return `None`. Callers then
/// treat the file as a legacy copy instead of inventing an id for it.
pub(crate) fn stem_id(stem: &str) -> Option<String> {
    let id = decode_stem(stem)?;
    (safe_stem(&id) == stem).then_some(id)
}

/// Whether `stem` is a name the pre-P5 scheme could have written, i.e. one whose
/// id cannot be recovered from the name alone.
pub(crate) fn is_legacy_stem(stem: &str) -> bool {
    stem_id(stem).is_none()
}

/// Escape one encoded byte: literal for [`is_plain_byte`], otherwise `~` + two
/// lowercase hex digits.
fn push_encoded_byte(out: &mut String, byte: u8) {
    if is_plain_byte(byte) {
        out.push(char::from(byte));
    } else {
        push_escaped_byte(out, byte);
    }
}

/// Force the `~xx` escaped spelling even for a byte that is normally literal.
/// Used only for the first byte of a reserved device name.
fn push_escaped_byte(out: &mut String, byte: u8) {
    out.push('~');
    out.push(char::from(HEX_LOWER[usize::from(byte >> 4)]));
    out.push(char::from(HEX_LOWER[usize::from(byte & 0x0f)]));
}

/// Decode a stem back to its id. Strict by design: it accepts only the literal
/// alphabet and `~` + two *lowercase* hex digits, so anything the old scheme
/// could have written (uppercase, dots, spaces, unescaped non-ASCII) is rejected
/// rather than silently mapped to a wrong id. [`stem_id`] additionally requires
/// the result to re-encode to exactly the same string.
fn decode_stem(stem: &str) -> Option<String> {
    if stem == EMPTY_ID_STEM {
        return Some(String::new());
    }
    let bytes = stem.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        if byte == b'~' {
            let high = hex_value(*bytes.get(i + 1)?)?;
            let low = hex_value(*bytes.get(i + 2)?)?;
            decoded.push((high << 4) | low);
            i += 3;
        } else if is_plain_byte(byte) {
            decoded.push(byte);
            i += 1;
        } else {
            return None;
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

/// Escape the first byte of an otherwise canonical encoding, neutralizing a
/// Windows device name without losing decodability. The first encoded byte is
/// always ASCII, so byte iteration (not slicing) is enough.
fn escape_first_byte(stem: &str) -> String {
    let mut out = String::with_capacity(stem.len() + 2);
    let mut bytes = stem.bytes();
    let Some(first) = bytes.next() else {
        return out;
    };
    push_escaped_byte(&mut out, first);
    for byte in bytes {
        out.push(char::from(byte));
    }
    out
}

/// The capped name for an id whose encoding is longer than [`MAX_STEM_LEN`].
///
/// The prefix stops on an escape-unit boundary (never mid-`~xx`) and the hash is
/// over the *full* id bytes, so two overlong ids that differ only past the kept
/// prefix still get different names unless the 64-bit hash collides. `!` and the
/// uppercase hex are outside the encoding grammar on purpose: a capped stem is
/// never decoded, only looked up in `downloads/.names.json`.
fn hashed_stem(id: &str) -> String {
    let budget = MAX_STEM_LEN - HASH_SUFFIX_LEN;
    let mut prefix = String::with_capacity(budget);
    for &byte in id.as_bytes() {
        let unit = if is_plain_byte(byte) { 1 } else { 3 };
        if prefix.len() + unit > budget {
            break;
        }
        push_encoded_byte(&mut prefix, byte);
    }
    let hash = fnv1a(id.as_bytes());
    let mut stem = String::with_capacity(MAX_STEM_LEN);
    stem.push_str(&prefix);
    stem.push('!');
    for shift in (0..16).rev() {
        let digit = ((hash >> (shift * 4)) & 0x0f) as usize;
        stem.push(char::from(HEX_UPPER[digit]));
    }
    stem
}

/// FNV-1a (64-bit) over the full id. Chosen because it is a handful of lines
/// with no dependency and a stable, documented output — the name must not change
/// across builds or the manifest mapping stops applying. This is a filename
/// disambiguator, not a security hash.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Windows device names are reserved case-insensitively and even with an
/// extension (`CON.m4a`). Our encodings escape every dot, so only the exact base
/// name can occur, but the helper still applies Windows' own rules (first dot,
/// trailing dots/spaces) so the contract is explicit and the test can pin it.
/// `COM0`/`LPT0` are included although the classic list starts at 1: some
/// Windows versions reserve them too, and escaping is harmless.
fn is_windows_device_name(stem: &str) -> bool {
    let base = stem.split_once('.').map_or(stem, |(base, _)| base);
    let base = base.trim_end_matches([' ', '.']);
    let lower = base.to_ascii_lowercase();
    if matches!(lower.as_str(), "con" | "prn" | "aux" | "nul") {
        return true;
    }
    let bytes = lower.as_bytes();
    bytes.len() == 4
        && (lower.starts_with("com") || lower.starts_with("lpt"))
        && bytes[3].is_ascii_digit()
}

/// Load a track for playback, preferring an offline copy under `downloads_dir`
/// and otherwise fetching it natively over the network. Returns the audio bytes
/// and duration in seconds (0.0 if unknown — the decoder reports the real
/// length).
///
/// The lookup tries the current name first and then the pre-P5
/// [`legacy_safe_stem`] name, because an existing install's `<id>.m4a` files are
/// not renamed at startup. A legacy `_`-folded name is inherently ambiguous (it
/// cannot say which of several ids it was written for); serving it keeps the
/// old behaviour and is no worse than the pre-fix lookup, while every new file
/// uses the injective name.
///
/// # Errors
///
/// Returns the native resolution/download failure when there is no usable
/// offline copy: the resolver reports the video unavailable or offers no
/// directly downloadable audio format, a network request fails, or the transfer
/// cannot be completed. The error names that single cause — there is no fallback
/// backend whose failure could mask it. A local file that cannot be read is
/// treated as "not downloaded" and falls back to the network.
pub async fn fetch_track(
    downloads_dir: &std::path::Path,
    video_id: &str,
) -> anyhow::Result<(Vec<u8>, f64)> {
    let stem = safe_stem(video_id);
    let local = downloads_dir.join(format!("{stem}.m4a"));
    if let Some(data) = read_offline(&local).await {
        // An empty file is not an offline copy (it can only come from a
        // failed/aborted write that predates the temp-file-then-rename path).
        // Fall through to the network instead of handing the decoder zero bytes.
        debug!(
            "playing {video_id} from offline download {}",
            local.display()
        );
        return Ok((data, 0.0));
    }

    // Old installs: `<legacy_safe_stem(id)>.m4a`. Only stat/read it when the
    // current name is absent, and only once when both names coincide.
    let legacy = legacy_safe_stem(video_id);
    if legacy != stem
        && !crate::downloads::legacy_name_owned_by_other(downloads_dir, &legacy, video_id)
    {
        let legacy_path = downloads_dir.join(format!("{legacy}.m4a"));
        if let Some(data) = read_offline(&legacy_path).await {
            debug!(
                "playing {video_id} from pre-P5 offline download {}",
                legacy_path.display()
            );
            return Ok((data, 0.0));
        }
    }
    fetch_bytes(video_id).await
}

/// Read an offline copy, or `None` when the path is missing, unreadable or
/// empty. `None` means "ask the network", never an error: a corrupt local file
/// must not block playback.
async fn read_offline(path: &std::path::Path) -> Option<Vec<u8>> {
    if !path.is_file() {
        return None;
    }
    match tokio::fs::read(path).await {
        Ok(data) if !data.is_empty() => Some(data),
        Ok(_) => {
            debug!(
                "ignoring the empty offline file {}; fetching from the network",
                path.display()
            );
            None
        }
        Err(e) => {
            debug!(
                "could not read offline file {}: {e}; fetching from the network",
                path.display()
            );
            None
        }
    }
}

/// Resolve a `YouTube` video ID to its audio stream and download it fully into
/// memory over the native ranged-HTTP pipeline.
///
/// Returns the audio data and duration in seconds (0.0 if unknown — the stream
/// is fetched as audio only, so the decoder reports the real length).
///
/// # Errors
///
/// Errors when the native resolver cannot produce a direct audio URL for
/// `video_id` (unavailable or removed video, a response with no directly
/// downloadable audio format, or a failure while resolving), or when the ranged
/// download fails (an HTTP error, a URL that is still rejected after one
/// re-resolve, a broken body, or a stream that does not match its reported
/// length).
pub async fn fetch_bytes(video_id: &str) -> anyhow::Result<(Vec<u8>, f64)> {
    let data = crate::stream::fetch_bytes(video_id)
        .await
        .with_context(|| format!("could not fetch the native audio stream for {video_id}"))?;
    // `stream` already rejects an empty stream; this guard keeps a future
    // regression there from serving silence as playback.
    if data.is_empty() {
        bail!("the native audio stream for {video_id} was empty");
    }
    info!("{video_id}: native stream served playback");
    Ok((data, 0.0))
}

#[cfg(test)]
mod tests {
    use super::{fetch_track, legacy_safe_stem, safe_stem, stem_id, EMPTY_ID_STEM, MAX_STEM_LEN};
    use crate::util::testing::TempDir;
    use std::collections::HashSet;

    #[test]
    fn safe_stem_preserves_plain_lowercase_ids_and_neutralizes_traversal() {
        // Real lowercase YouTube ids are already a safe single component and
        // pass through unchanged: existing plain files keep their names.
        assert_eq!(safe_stem("dqw4w9wgxcq"), "dqw4w9wgxcq");
        assert_eq!(safe_stem("a-b_c"), "a-b_c");
        assert_eq!(safe_stem("0123456789"), "0123456789");
        // Separators, `..`, whitespace and dots are escaped, not folded, so the
        // stem says exactly which id it is.
        assert_eq!(safe_stem(".."), "~2e~2e");
        assert_eq!(safe_stem("../etc/passwd"), "~2e~2e~2fetc~2fpasswd");
        assert_eq!(safe_stem("a/b"), "a~2fb");
        assert_eq!(safe_stem("a\\b"), "a~5cb");
        assert_eq!(safe_stem("a b"), "a~20b");
        assert_eq!(safe_stem("a.b"), "a~2eb");
        assert_eq!(safe_stem("~"), "~7e");
        // The empty id still yields a safe, non-reserved, decodable name.
        assert_eq!(safe_stem(""), EMPTY_ID_STEM);
        assert!(!super::is_windows_device_name(EMPTY_ID_STEM));
        assert_eq!(stem_id(EMPTY_ID_STEM).as_deref(), Some(""));
    }

    /// The old scheme was many-to-one (`a/b` and `a_b` both became `a_b`). The
    /// new stem must be injective for a representative set of hostile ids.
    #[test]
    fn safe_stem_is_injective_for_tricky_ids() {
        let long = "a".repeat(200);
        let ids = [
            "",
            "a",
            "a/b",
            "a_b",
            "a?b",
            "a:b",
            "a b",
            "a.b",
            "..",
            "__",
            "../etc/passwd",
            "a\\b",
            "~",
            "~2f",
            "AbC",
            "abc",
            "ABC",
            "dQw4w9WgXcQ",
            "con",
            "CON",
            "nul",
            "com1.txt",
            "lpt9",
            "ünïcode",
            "🎵",
            "\u{0}",
            "line\nbreak",
            long.as_str(),
        ];
        let mut stems = HashSet::new();
        for id in ids {
            let stem = safe_stem(id);
            assert!(
                stems.insert(stem.clone()),
                "safe_stem({id:?}) collided at {stem:?}"
            );
        }
        // The pair the old scheme actually shared a file for.
        assert_ne!(safe_stem("a/b"), safe_stem("a_b"));
        assert_ne!(safe_stem(".."), safe_stem("__"));
    }

    /// On Windows/macOS the filesystem folds case, so case-only ids need names
    /// that stay distinct after `to_ascii_lowercase`.
    #[test]
    fn safe_stem_maps_case_only_ids_to_case_insensitively_distinct_names() {
        let ids = ["AbC", "abc", "ABC", "aBc", "Abc"];
        let mut folded = HashSet::new();
        for id in ids {
            let stem = safe_stem(id);
            assert!(
                folded.insert(stem.to_ascii_lowercase()),
                "{id:?} fold-collided at {stem:?}"
            );
        }
        assert_ne!(safe_stem("AbC"), safe_stem("abc"));
        assert_eq!(safe_stem("AbC"), "~41b~43");
        assert_eq!(safe_stem("abc"), "abc");
    }

    /// The two collisions that motivated the fix must stay apart, and ids that
    /// were merely `_`-folded must no longer share a file.
    #[test]
    fn safe_stem_keeps_ids_that_used_to_collide_apart() {
        assert_ne!(safe_stem("a/b"), safe_stem("a_b"));
        assert_ne!(safe_stem(".."), safe_stem("__"));
        assert_ne!(safe_stem("a?b"), safe_stem("a_b"));
        assert_ne!(safe_stem("a:b"), safe_stem("a_b"));
        // ... while the legacy helper still reproduces the old shared name, so
        // an existing file remains findable through the fallback.
        assert_eq!(legacy_safe_stem("a/b"), legacy_safe_stem("a_b"));
    }

    /// Windows reserves device names case-insensitively, with or without an
    /// extension. Every stem we emit for such an id must not be one.
    #[test]
    fn safe_stem_neutralizes_windows_reserved_device_names() {
        for id in [
            "con",
            "CON",
            "Con",
            "prn",
            "aux",
            "nul",
            "com1",
            "COM9",
            "lpt1",
            "LPT9",
            "com0",
            "lpt0",
            "con.txt",
            "COM1.txt",
            "nul.tar.gz",
            "lpt9",
        ] {
            let stem = safe_stem(id);
            assert!(
                !super::is_windows_device_name(&stem),
                "safe_stem({id:?}) = {stem:?} is still a Windows device name"
            );
            assert_ne!(stem.to_ascii_lowercase(), id.to_ascii_lowercase());
            // Neutralizing escapes the first byte instead of dropping it, so the
            // name is still decodable.
            assert_eq!(stem_id(&stem).as_deref(), Some(id));
        }
        // Normal names that merely start like a device name are untouched.
        assert_eq!(safe_stem("console"), "console");
        assert_eq!(safe_stem("community"), "community");
        assert!(!super::is_windows_device_name("console"));
        assert!(super::is_windows_device_name("con.m4a"));
        assert!(super::is_windows_device_name("COM1.txt"));
    }

    /// Overlong ids get a capped prefix + stable hash: distinct, deterministic,
    /// and structurally undecodable (the manifest maps them, not the name).
    #[test]
    fn safe_stem_caps_overlong_ids_and_keeps_them_distinct() {
        let long_a = "a".repeat(400);
        let long_b = "a".repeat(400) + "b";
        let long_c = "b".repeat(400);
        let stem_a = safe_stem(&long_a);
        let stem_b = safe_stem(&long_b);
        let stem_c = safe_stem(&long_c);
        assert!(stem_a.len() <= MAX_STEM_LEN);
        assert!(stem_b.len() <= MAX_STEM_LEN);
        assert!(stem_c.len() <= MAX_STEM_LEN);
        assert_ne!(stem_a, stem_b, "a suffix difference must change the hash");
        assert_ne!(stem_a, stem_c);
        assert_eq!(stem_a, safe_stem(&long_a), "the hash is stable");
        assert!(
            stem_a.contains('!'),
            "the capped form is marked: {stem_a:?}"
        );
        let (_, hash) = stem_a
            .rsplit_once('!')
            .expect("the capped form has a marker");
        assert!(
            hash.len() == 16 && hash.chars().all(|c| c.is_ascii_hexdigit()),
            "the suffix is 16 hex digits: {stem_a:?}"
        );
        assert_eq!(
            stem_id(&stem_a),
            None,
            "a capped name must not be guessed back to an id"
        );
        // Case-insensitive filesystems still see distinct names.
        assert_ne!(stem_a.to_ascii_lowercase(), stem_b.to_ascii_lowercase());
        // A long id whose encoding is exactly at the cap is still decodable.
        let at_cap = "a".repeat(MAX_STEM_LEN);
        assert_eq!(safe_stem(&at_cap), at_cap);
        assert_eq!(stem_id(&at_cap).as_deref(), Some(at_cap.as_str()));
    }

    /// The encoding is reversible for every name under the cap, including the
    /// neutralized reserved spellings — `Downloads::load` relies on it.
    #[test]
    fn stem_id_round_trips_every_decodable_stem() {
        for id in [
            "",
            "abc-123_x",
            "AbC",
            "a/b",
            "..",
            "__",
            "con",
            "COM1.txt",
            "lpt9",
            "ünïcode 🎵",
            "~7e",
        ] {
            let stem = safe_stem(id);
            assert_eq!(
                stem_id(&stem).as_deref(),
                Some(id),
                "round-trip for {id:?} via {stem:?}"
            );
        }
        // Non-canonical or pre-P5 spellings are not guessed.
        assert_eq!(stem_id("a_b"), Some("a_b".to_owned()));
        assert_eq!(stem_id("AbC"), None, "old uppercase id file");
        assert_eq!(stem_id("~61bc"), None, "over-escaped spelling");
        assert_eq!(stem_id("plain~"), None, "dangling escape");
        assert_eq!(stem_id(""), None, "an empty stem is not the empty id");
    }

    /// Freeze the pre-P5 function: it is the only way an old file is found after
    /// the naming change, so its output must never drift.
    #[test]
    fn legacy_safe_stem_keeps_the_pre_p5_folding() {
        assert_eq!(legacy_safe_stem("dQw4w9WgXcQ"), "dQw4w9WgXcQ");
        assert_eq!(legacy_safe_stem("a-b_c"), "a-b_c");
        assert_eq!(legacy_safe_stem("a/b"), "a_b");
        assert_eq!(legacy_safe_stem(".."), "__");
        assert_eq!(legacy_safe_stem("../etc/passwd"), "___etc_passwd");
        assert_eq!(legacy_safe_stem(""), "_");
        assert!(super::is_legacy_stem("dQw4w9WgXcQ"));
        assert!(!super::is_legacy_stem("abc"));
    }

    /// The playback promise for an offline track: the local copy is served
    /// without any network access. The fixture is the only possible source of
    /// these bytes, so this cannot pass by fetching them.
    #[tokio::test]
    async fn fetch_track_serves_an_offline_copy_before_touching_the_network() {
        let dir = TempDir::new("fetch-offline-first");
        let bytes = b"offline audio bytes".to_vec();
        std::fs::write(dir.path().join("track-id.m4a"), &bytes).expect("write the offline copy");

        let (data, duration) = fetch_track(dir.path(), "track-id")
            .await
            .expect("an offline copy needs no network");
        assert_eq!(data, bytes);
        assert_eq!(duration, 0.0, "the decoder reports the real length");
    }

    /// A sanitising id is stored under its encoded name; playback must find it
    /// there without falling back to the network.
    #[tokio::test]
    async fn fetch_track_serves_a_current_encoded_name_offline() {
        let dir = TempDir::new("fetch-encoded-offline");
        let bytes = b"encoded offline audio".to_vec();
        std::fs::write(dir.path().join("a~2fb.m4a"), &bytes).expect("write the encoded copy");

        let (data, _) = fetch_track(dir.path(), "a/b")
            .await
            .expect("an encoded offline copy needs no network");
        assert_eq!(data, bytes);
    }

    /// An install upgraded from before the naming fix has the old `_`-folded
    /// file, not the new name. It must still play without the network.
    #[tokio::test]
    async fn fetch_track_serves_a_pre_p5_folded_name_offline() {
        let dir = TempDir::new("fetch-legacy-offline");
        let bytes = b"legacy offline audio".to_vec();
        std::fs::write(dir.path().join("a_b.m4a"), &bytes).expect("write the legacy copy");

        let (data, duration) = fetch_track(dir.path(), "a/b")
            .await
            .expect("a legacy offline copy needs no network");
        assert_eq!(data, bytes);
        assert_eq!(duration, 0.0);
    }

    /// An old uppercase id file is legacy too (the new encoding escapes
    /// uppercase); serving it keeps upgraded installs offline.
    #[tokio::test]
    async fn fetch_track_serves_a_pre_p5_uppercase_id_offline() {
        let dir = TempDir::new("fetch-legacy-uppercase");
        let bytes = b"legacy uppercase audio".to_vec();
        std::fs::write(dir.path().join("dQw4w9WgXcQ.m4a"), &bytes).expect("write the legacy copy");

        let (data, _) = fetch_track(dir.path(), "dQw4w9WgXcQ")
            .await
            .expect("a legacy offline copy needs no network");
        assert_eq!(data, bytes);
    }

    /// When both names exist (e.g. a re-download after an upgrade), the current
    /// name wins deterministically, so playback never depends on directory
    /// enumeration order.
    #[tokio::test]
    async fn fetch_track_prefers_the_current_name_over_a_legacy_copy() {
        let dir = TempDir::new("fetch-prefer-current");
        std::fs::write(dir.path().join("a~2fb.m4a"), b"new audio").expect("write the new copy");
        std::fs::write(dir.path().join("a_b.m4a"), b"old audio").expect("write the legacy copy");

        let (data, _) = fetch_track(dir.path(), "a/b")
            .await
            .expect("an offline copy needs no network");
        assert_eq!(data, b"new audio");
    }
}
