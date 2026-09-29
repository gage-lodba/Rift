//! SHA-256, so the one-click yt-dlp installer can verify what it downloads.
//!
//! Deliberately hand-rolled rather than pulling in `sha2`: this is the only
//! hash the app needs, FIPS 180-4 is short and fixed, and the alternative is a
//! new dependency in the trust path of a binary Rift later executes. The
//! implementation is checked against the published test vectors in this
//! module's tests, including the multi-block "abc…" case that exercises the
//! message schedule and padding.
//!
//! Not for password hashing or anything adversarial beyond integrity checking
//! against a checksum published in the same release.

/// Round constants: the first 32 bits of the fractional parts of the cube roots
/// of the first 64 primes (FIPS 180-4 §4.2.2).
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// Initial hash state: the first 32 bits of the fractional parts of the square
/// roots of the first 8 primes (FIPS 180-4 §5.3.3).
const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// Streaming SHA-256 state.
pub struct Sha256 {
    state: [u32; 8],
    /// Bytes not yet folded into a full 64-byte block.
    buffer: [u8; 64],
    buffered: usize,
    /// Total message length in bytes, for the padding's bit count.
    len: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    pub fn new() -> Self {
        Self {
            state: H0,
            buffer: [0; 64],
            buffered: 0,
            len: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.len = self.len.wrapping_add(data.len() as u64);
        // Top up a partial block first.
        if self.buffered > 0 {
            let take = (64 - self.buffered).min(data.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&data[..take]);
            self.buffered += take;
            data = &data[take..];
            if self.buffered == 64 {
                let block = self.buffer;
                self.compress(&block);
                self.buffered = 0;
            } else {
                // This input only partly filled the block; everything is now
                // buffered, so there is nothing left to fold in. Without this
                // return the loop below runs over an empty slice and resets
                // `buffered` to 0, silently discarding the partial block.
                return;
            }
        }
        // Whole blocks straight from the input. `chunks` (not `chunks_exact`)
        // yields empty slices for the 0-byte input, and the `copy_from_slice`
        // asserts the length — so a short final chunk can never be silently
        // compressed as a full block.
        let mut rest: &[u8] = &[];
        for block in data.chunks(64) {
            if block.len() == 64 {
                let mut b = [0u8; 64];
                b.copy_from_slice(block);
                self.compress(&b);
            } else {
                rest = block;
            }
        }
        self.buffer[..rest.len()].copy_from_slice(rest);
        self.buffered = rest.len();
    }

    /// Finish and return the 32-byte digest.
    pub fn finalize(mut self) -> [u8; 32] {
        // Padding: a single 1 bit, then zeros, then the length in bits as a
        // big-endian u64, so the total is a multiple of 64 bytes.
        let bit_len = self.len.wrapping_mul(8);
        self.buffer[self.buffered] = 0x80;
        self.buffered += 1;
        if self.buffered > 56 {
            self.buffer[self.buffered..].fill(0);
            let block = self.buffer;
            self.compress(&block);
            self.buffered = 0;
        }
        self.buffer[self.buffered..56].fill(0);
        self.buffer[56..64].copy_from_slice(&bit_len.to_be_bytes());
        let block = self.buffer;
        self.compress(&block);

        let mut out = [0u8; 32];
        for (i, word) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        for (slot, value) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
}

/// One-shot convenience wrapper.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize()
}

/// Lowercase hex of a digest, for comparison against a published checksum.
pub fn hex(digest: &[u8]) -> String {
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Look the checksum for `filename` out of a checksum manifest.
///
/// Handles the two shapes actually published in the wild — GNU coreutils
/// (`<hex>  <name>`, optionally `<hex> *<name>`) and BSD (`SHA256 (<name>) =
/// <hex>`) — because a silent parse failure here would look exactly like a
/// checksum mismatch, and the honest failure mode is "no entry for this asset".
/// Names are matched on their final path component so a manifest that lists
/// `./yt-dlp.exe` or `dist/yt-dlp.exe` still matches.
pub fn checksum_for(manifest: &str, filename: &str) -> Option<String> {
    for line in manifest.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // BSD style: `SHA256 (name) = hex`
        if let Some(rest) = line.strip_prefix("SHA256 (") {
            if let Some((name, hex)) = rest.split_once(") = ") {
                if base_name(name) == filename && is_hex_digest(hex) {
                    return Some(hex.to_ascii_lowercase());
                }
                continue;
            }
        }

        // GNU style: `hex  name` (two spaces) or `hex *name` (binary marker).
        let Some((hex, rest)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let name = rest.trim_start().trim_start_matches('*');
        if base_name(name) == filename && is_hex_digest(hex) {
            return Some(hex.to_ascii_lowercase());
        }
    }
    None
}

/// The final path component, so a manifest entry with directories still matches.
fn base_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Whether `s` is exactly 64 hex digits — a SHA-256 digest, not a truncated or
/// wrong-algorithm line.
fn is_hex_digest(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FIPS 180-4 / NIST published vectors. The two longer ones cross a block
    /// boundary, so they exercise the message schedule and the padding path.
    #[test]
    fn matches_the_published_test_vectors() {
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&sha256(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            hex(&sha256(&[b'a'; 1_000_000])),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    /// Streaming must agree with one-shot regardless of where the splits land,
    /// including inside the length-padding block (56 and 64 bytes).
    #[test]
    fn streaming_agrees_with_one_shot_at_block_boundaries() {
        let data: Vec<u8> = (0..200u32).map(|i| (i % 251) as u8).collect();
        let expected = hex(&sha256(&data));
        for split in [0, 1, 55, 56, 63, 64, 65, 127, 128, 199, 200] {
            let mut h = Sha256::new();
            h.update(&data[..split]);
            h.update(&data[split..]);
            assert_eq!(hex(&h.finalize()), expected, "split at {split}");
        }
        // Byte-at-a-time, the worst case for the buffering path.
        let mut h = Sha256::new();
        for byte in &data {
            h.update(&[*byte]);
        }
        assert_eq!(hex(&h.finalize()), expected, "byte at a time");
    }

    /// A partial `update` that does not reach a block boundary must survive
    /// until the next call. The first suite run of this module caught the
    /// opposite: a short top-up fell through to the block loop and reset
    /// `buffered` to 0, discarding the partial block and hashing a shorter
    /// message — which only shows up when total length stays a multiple of 64
    /// (199 + 1 bytes), because those inputs agree by luck otherwise.
    #[test]
    fn a_partial_update_survives_until_the_next_call() {
        // 199 + 1 = 200 bytes: the split leaves a partial block behind.
        let data: Vec<u8> = (0..200u32).map(|i| (i % 251) as u8).collect();
        let expected = hex(&sha256(&data));
        let mut h = Sha256::new();
        h.update(&data[..199]);
        h.update(&data[199..]);
        assert_eq!(hex(&h.finalize()), expected);

        // The same in three pieces, with the partial in the middle.
        let mut h = Sha256::new();
        h.update(&data[..10]);
        h.update(&data[10..100]);
        h.update(&data[100..]);
        assert_eq!(hex(&h.finalize()), expected);

        // And a partial update followed by a multiple-of-64 one.
        let mut h = Sha256::new();
        h.update(&data[..5]);
        h.update(&data[5..]);
        assert_eq!(hex(&h.finalize()), expected);
    }

    #[test]
    fn reads_gnu_style_checksum_manifests() {
        let manifest = "\
# SHA2-256SUMS from a yt-dlp release
6b0e1f0f0d4a5c8e1f2b3a4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f80  yt-dlp.exe
1111111111111111111111111111111111111111111111111111111111111111  yt-dlp_linux
";
        assert_eq!(
            checksum_for(manifest, "yt-dlp.exe").as_deref(),
            Some("6b0e1f0f0d4a5c8e1f2b3a4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f80")
        );
        assert_eq!(
            checksum_for(manifest, "yt-dlp_linux").as_deref(),
            Some("1111111111111111111111111111111111111111111111111111111111111111")
        );
        // An asset the manifest does not list yields nothing, rather than
        // falling back to some other entry's digest.
        assert_eq!(checksum_for(manifest, "yt-dlp_macos"), None);
    }

    #[test]
    fn reads_the_binary_marker_and_bsd_forms() {
        let gnu_binary =
            "2222222222222222222222222222222222222222222222222222222222222222 *yt-dlp.exe\n";
        assert!(checksum_for(gnu_binary, "yt-dlp.exe").is_some());

        let bsd = "SHA256 (yt-dlp.exe) = 3333333333333333333333333333333333333333333333333333333333333333\n";
        assert_eq!(
            checksum_for(bsd, "yt-dlp.exe").as_deref(),
            Some("3333333333333333333333333333333333333333333333333333333333333333")
        );

        // A digest is matched case-insensitively.
        let upper =
            "ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789  yt-dlp.exe\n";
        assert_eq!(
            checksum_for(upper, "yt-dlp.exe").as_deref(),
            Some("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789")
        );
    }

    #[test]
    fn ignores_malformed_lines_instead_of_matching_them() {
        // A short digest would be a wrong-algorithm or truncated line.
        let short = "abcd  yt-dlp.exe\n";
        assert_eq!(checksum_for(short, "yt-dlp.exe"), None);
        // A non-hex digest.
        let junk = "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz  yt-dlp.exe\n";
        assert_eq!(checksum_for(junk, "yt-dlp.exe"), None);
        // Blank lines and comments.
        assert_eq!(checksum_for("\n\n# comment\n", "yt-dlp.exe"), None);
        assert_eq!(checksum_for("", "yt-dlp.exe"), None);
    }

    #[test]
    fn matches_a_manifest_entry_that_carries_a_directory() {
        let manifest =
            "4444444444444444444444444444444444444444444444444444444444444444  ./dist/yt-dlp.exe\n";
        assert!(checksum_for(manifest, "yt-dlp.exe").is_some());
    }
}
