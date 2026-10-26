//! Cover-art decoding, downscaling and request de-duplication.
//!
//! Fetching is the backend's job: the controller calls
//! `rift::BackendHandle::fetch_image` (the backend's shared HTTP client, so the
//! app never builds a second one) and then hands the encoded bytes to
//! [`decode_cover`] on a blocking worker thread. This module owns everything
//! after the bytes arrive, plus the bookkeeping that keeps the UI from hammering
//! the network for a URL that is already loading or has already failed.
//!
//! Decoding and resizing are CPU work and must stay off the UI thread: the
//! controller runs [`decode_cover`] through `spawn_blocking`. The bytes are
//! untrusted network input, so decoding is bounded ([`MAX_SOURCE_EDGE`],
//! [`MAX_SOURCE_ALLOC`]) before the decoder can reserve a pixel buffer; a
//! decompression bomb fails as an ordinary "could not decode" error instead of
//! exhausting memory.

use std::collections::{HashSet, VecDeque};

/// Longest edge of a decoded cover thumbnail, in pixels.
///
/// The design's largest cover is the 170 logical px album art (the player-bar
/// cover is 44 px), so 256 covers it through 150% DPI scaling without keeping
/// multi-megapixel `YouTube` thumbnails in memory: a 256x256 RGBA8 cover is
/// 256 KiB, a 544x544 one is 1.1 MiB. At 200% scaling the GPU upscales the
/// texture slightly (softer art); that is the accepted trade-off for a bounded
/// 256-entry texture cache, see `app::MAX_CACHED_COVERS`.
pub const MAX_COVER_DIMENSION: u32 = 256;

/// How many failed URLs are remembered before the oldest are forgotten.
///
/// A session cannot reasonably produce more distinct broken covers; the bound
/// exists so a pathological feed (every row a different dead URL) cannot grow
/// the failure set without limit.
pub const MAX_REMEMBERED_FAILURES: usize = 1024;

/// Largest source edge, in pixels, that [`decode_cover`] will decode.
///
/// `YouTube` serves thumbnails at up to ~1280 px; a "cover" claiming more than
/// 4096 px is not artwork this UI can use and is far more likely to be a
/// decompression bomb. The limit is enforced by the decoder *before* it reserves
/// a pixel buffer, so a tiny crafted PNG cannot make the blocking pool allocate
/// gigabytes.
const MAX_SOURCE_EDGE: u32 = 4096;

/// Ceiling for the decoded source image's pixel buffer.
///
/// `image`'s default is 512 MiB, which a 16 MiB compressed upload (the backend's
/// fetch cap) could still reach. A 4096x4096 RGBA8 image is 64 MiB, and the
/// result is downscaled to at most [`MAX_COVER_DIMENSION`] immediately after
/// decode, so a larger source only wastes memory and CPU.
const MAX_SOURCE_ALLOC: u64 = 64 * 1024 * 1024;

/// A decoded cover image ready to be uploaded as a texture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoverImage {
    /// The URL this image was fetched from (the cover-cache key).
    pub url: String,
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGBA8, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

impl CoverImage {
    /// Expected byte length of [`Self::rgba`], for validation and tests.
    pub fn expected_len(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }
}

/// Decode encoded image bytes (PNG/JPEG as served) and downscale them to at most
/// [`MAX_COVER_DIMENSION`] on the longest edge, returning RGBA8 pixels.
///
/// Never upscales: a small source is returned at its own size. An empty source
/// or one whose dimensions are degenerate is an error rather than a panic.
///
/// # Errors
///
/// Returns `Err` with a human-readable message when the bytes are not a
/// supported image, when the source exceeds the decode limits
/// ([`MAX_SOURCE_EDGE`] / [`MAX_SOURCE_ALLOC`]), or when the decoded image has
/// no pixels.
pub fn decode_cover(url: &str, bytes: &[u8]) -> Result<CoverImage, String> {
    let image = decode_source(bytes)?;
    let image = downscale(image, MAX_COVER_DIMENSION)?;
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(CoverImage {
        url: url.to_string(),
        width,
        height,
        rgba: rgba.into_raw(),
    })
}

/// Decode untrusted image bytes with the resource limits applied *before* the
/// decoder reserves a pixel buffer.
///
/// Cover bytes arrive from the network (the backend caps the transfer at 16 MiB,
/// but not the decoded size), so a decompression bomb is the realistic attack:
/// a few KiB of PNG can declare a 50,000 x 50,000 image and make the
/// `spawn_blocking` worker try to allocate ~10 GB. Both a dimension cap and an
/// allocation cap keep that bounded; they are generous enough for every real
/// cover size.
fn decode_source(bytes: &[u8]) -> Result<image::DynamicImage, String> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| format!("could not decode cover art: {error}"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_EDGE);
    limits.max_image_height = Some(MAX_SOURCE_EDGE);
    limits.max_alloc = Some(MAX_SOURCE_ALLOC);
    reader.limits(limits);
    reader
        .decode()
        .map_err(|error| format!("could not decode cover art: {error}"))
}

/// Downscale `image` so neither edge exceeds `max_edge`, preserving aspect ratio.
fn downscale(image: image::DynamicImage, max_edge: u32) -> Result<image::DynamicImage, String> {
    let (width, height) = (image.width(), image.height());
    if width == 0 || height == 0 {
        return Err("cover art has no pixels".into());
    }
    let longest = width.max(height);
    if max_edge == 0 || longest <= max_edge {
        return Ok(image);
    }
    let scale = max_edge as f64 / longest as f64;
    // Round to the nearest pixel but never to zero: a 2000x1 banner becomes
    // 256x1 rather than an empty image, and the constructor invariant holds.
    let target_width = ((width as f64 * scale).round() as u32).max(1);
    let target_height = ((height as f64 * scale).round() as u32).max(1);
    // `resize_exact`, not `resize`: the latter re-derives the dimensions from
    // the aspect ratio and can shave a pixel off the requested edge (a 300x900
    // cover asked for 85x256 would come back 85x255). The integer rounding above
    // is the aspect-preserving step; the resize should honour it exactly.
    Ok(image.resize_exact(
        target_width,
        target_height,
        image::imageops::FilterType::Lanczos3,
    ))
}

/// The decision for one [`CoverTracker::request`] call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoverRequest {
    /// The caller should start the fetch; the URL is now marked in flight.
    Fetch,
    /// A fetch for this URL is already in flight; do nothing.
    InFlight,
    /// This URL already failed this session; do not retry, render the fallback.
    Failed,
}

/// Tracks which cover URLs are loading and which have failed.
///
/// The UI's `State::covers` cache is the first line of de-duplication, but it is
/// UI state: screens can be rebuilt, textures dropped, and the same URL can be
/// requested from several places in one frame. This tracker is the controller's
/// own authoritative record so at most one fetch per URL is ever in flight and a
/// dead URL is not retried on every frame.
#[derive(Debug, Default)]
pub struct CoverTracker {
    in_flight: HashSet<String>,
    failed: HashSet<String>,
    /// Insertion order of `failed`, for the FIFO bound.
    failed_order: VecDeque<String>,
}

impl CoverTracker {
    /// Ask whether `url` should be fetched now, marking it in flight if so.
    pub fn request(&mut self, url: &str) -> CoverRequest {
        if self.failed.contains(url) {
            return CoverRequest::Failed;
        }
        if !self.in_flight.insert(url.to_string()) {
            return CoverRequest::InFlight;
        }
        CoverRequest::Fetch
    }

    /// Report the outcome of a fetch started by [`Self::request`].
    ///
    /// A failure is remembered (bounded by [`MAX_REMEMBERED_FAILURES`]) and the
    /// URL is never requested again in this session, so a broken or offline URL
    /// costs one attempt instead of one per frame. Success frees the URL for a
    /// future request but is not remembered: the UI's texture is the cache.
    pub fn finish(&mut self, url: &str, success: bool) {
        self.in_flight.remove(url);
        if !success {
            self.remember_failure(url);
        }
    }

    /// Whether a fetch for `url` is currently in flight.
    pub fn is_in_flight(&self, url: &str) -> bool {
        self.in_flight.contains(url)
    }

    /// Whether `url` is known to have failed this session.
    pub fn is_failed(&self, url: &str) -> bool {
        self.failed.contains(url)
    }

    /// Number of URLs currently being fetched (test/diagnostics helper).
    pub fn in_flight_len(&self) -> usize {
        self.in_flight.len()
    }

    fn remember_failure(&mut self, url: &str) {
        if !self.failed.insert(url.to_string()) {
            return;
        }
        self.failed_order.push_back(url.to_string());
        while self.failed_order.len() > MAX_REMEMBERED_FAILURES {
            if let Some(oldest) = self.failed_order.pop_front() {
                self.failed.remove(&oldest);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageEncoder, Rgba, RgbaImage};

    /// Encode a solid-colour RGBA PNG, so decode tests need no fixture files.
    fn png(width: u32, height: u32, pixel: [u8; 4]) -> Vec<u8> {
        let image = RgbaImage::from_pixel(width, height, Rgba(pixel));
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(
                image.as_raw(),
                width,
                height,
                image::ExtendedColorType::Rgba8,
            )
            .expect("encode the test PNG");
        bytes
    }

    #[test]
    fn the_longest_edge_is_capped_at_the_maximum() {
        let image = decode_cover(
            "https://example.invalid/a.jpg",
            &png(640, 480, [1, 2, 3, 255]),
        )
        .expect("decode");
        assert_eq!(image.url, "https://example.invalid/a.jpg");
        assert_eq!((image.width, image.height), (256, 192));
        assert_eq!(image.rgba.len(), image.expected_len());
        assert_eq!(image.rgba.len(), 256 * 192 * 4);
    }

    #[test]
    fn a_portrait_cover_is_capped_on_its_height() {
        let image = decode_cover("u", &png(300, 900, [9, 8, 7, 255])).expect("decode");
        assert_eq!((image.width, image.height), (85, 256));
        assert_eq!(image.rgba.len(), 85 * 256 * 4);
    }

    #[test]
    fn a_small_cover_is_not_upscaled_and_keeps_its_alpha() {
        let image = decode_cover("u", &png(64, 32, [10, 20, 30, 128])).expect("decode");
        assert_eq!((image.width, image.height), (64, 32));
        assert_eq!(&image.rgba[..4], &[10, 20, 30, 128], "RGBA must survive");
    }

    #[test]
    fn a_wide_panorama_keeps_at_least_one_pixel_per_edge() {
        let image = decode_cover("u", &png(2000, 2, [0, 0, 0, 255])).expect("decode");
        assert_eq!((image.width, image.height), (256, 1));
    }

    #[test]
    fn undecodable_bytes_are_an_error_not_a_panic() {
        let error = decode_cover("u", b"this is not an image").expect_err("must fail");
        assert!(error.contains("decode"), "unhelpful error: {error}");
        assert!(decode_cover("u", &[]).is_err());
    }

    /// A source exactly at the decode limit is still accepted: the cap exists to
    /// stop bombs, not to reject legitimate large artwork. Downscaling still
    /// applies.
    #[test]
    fn a_source_at_the_decode_limit_is_accepted() {
        let image = decode_cover("u", &png(MAX_SOURCE_EDGE, 4, [3, 4, 5, 255])).expect("decode");
        assert_eq!((image.width, image.height), (256, 1));
    }

    /// A decompression bomb (a tiny file declaring dimensions above the limit) is
    /// rejected by the decode limits before any pixel buffer is allocated, so the
    /// process never attempts a multi-gigabyte allocation.
    #[test]
    fn a_source_above_the_decode_limit_is_rejected() {
        let error = decode_cover("u", &png(MAX_SOURCE_EDGE + 1, 8, [0, 0, 0, 255]))
            .expect_err("oversized source must be rejected");
        assert!(error.contains("decode"), "unhelpful error: {error}");
    }

    #[test]
    fn the_tracker_starts_and_ends_one_fetch_per_url() {
        let mut tracker = CoverTracker::default();
        assert_eq!(tracker.request("a"), CoverRequest::Fetch);
        assert!(tracker.is_in_flight("a"));
        assert_eq!(
            tracker.request("a"),
            CoverRequest::InFlight,
            "no second fetch"
        );
        assert_eq!(tracker.in_flight_len(), 1);
        tracker.finish("a", true);
        assert!(!tracker.is_in_flight("a"));
        assert!(!tracker.is_failed("a"));
        // A later request is allowed again: the UI's texture is the cache, and a
        // dropped/evicted texture may legitimately ask once more.
        assert_eq!(tracker.request("a"), CoverRequest::Fetch);
    }

    #[test]
    fn a_failed_url_is_not_retried() {
        let mut tracker = CoverTracker::default();
        assert_eq!(tracker.request("bad"), CoverRequest::Fetch);
        tracker.finish("bad", false);
        assert!(tracker.is_failed("bad"));
        assert_eq!(tracker.request("bad"), CoverRequest::Failed);
        assert_eq!(tracker.request("bad"), CoverRequest::Failed);
    }

    #[test]
    fn remembered_failures_are_bounded() {
        let mut tracker = CoverTracker::default();
        for i in 0..(MAX_REMEMBERED_FAILURES + 10) {
            let url = format!("bad-{i}");
            tracker.finish(&url, false);
        }
        assert_eq!(tracker.failed.len(), MAX_REMEMBERED_FAILURES);
        // The oldest were forgotten (their fallback tile will retry once), the
        // newest are still suppressed.
        assert!(!tracker.is_failed("bad-0"));
        assert!(tracker.is_failed(&format!("bad-{}", MAX_REMEMBERED_FAILURES + 9)));
    }
}
