# Native audio streaming (clean-room) — design and phase plan

Status: **Phases 0–5 complete** (2026-10-26): the native resolver, ranged
downloader, progress plumbing and row bar are shipped; the Phase 4 live proof
passed; and **Phase 5 removed yt-dlp from the product** — the app resolves and
downloads audio natively, with no external media binary, installer/detection
surface or fallback path. This document is the design and verification record for
that pipeline. Fallback-era sections are kept as history because they explain why
the remaining single-client risk is documented rather than solved.

## 0. Implementation status (2026-10-26)

| Phase | Status | Evidence |
|---|---|---|
| 0 — spike | **done** | §2; recorded traffic/responses in `artifacts/native-stream/` |
| 1 — resolver `rift_ytmusic::player` | **done** | 71 lib + 15 fixture tests; live resolve + ranged GET `206 audio/mp4`; client table + chain (§3.1) |
| 2 — downloader + progress `rift` | **done** | native ranged downloader (range/resume/200-fallback/size/expiry), progress lifecycle, loopback tests |
| 3 — UI bar | **done** | `rift-desktop`: 314 lib + 13 ui_flow tests after the Phase 5 removal; bar geometry/no-bar tests; `ui-spec.md` §7.1.3 updated |
| 4 — live verification + default flip | **done** | §4.1: native-vs-yt-dlp parity, playback smoke, download worker (progress snapshots, restart/offline replay), UI capture, forced-fallback trigger, controller e2e |
| 5 — remove yt-dlp surface | **done** | §4.2: backend/desktop surface removed; `RIFT_STREAM_BACKEND`/`RIFT_YTDLP`/settings/installer gone; native-only probes + live e2e pass; full local gate green |

Resilience follow-up (2026-10-26): the resolver is a **client table plus
resolution chain** (`CHAIN` in `player.rs`) so another JS-less client can be added
as data; a 20-id live probe found one fully-downloadable client (`VISIONOS`) and
no usable second one yet — §3.1 records the probe and the maintenance procedure.

Phase 5 started on the automated proof rather than waiting for a release soak.
The human-only checks (listen to a track, watch the bar in the live window,
restart with the network actually off) cannot be asserted by automation; they are
recorded as post-removal release verification (§4.2, task-5).

## 1. Goal and non-goals

Goal: Rift resolves and downloads its own audio. No external media binary, one
HTTP streaming path shared by playback and offline downloads, and a real
byte-level progress fraction so the UI can draw a determinate download bar.

Non-goals (this effort): reimplementing YouTube's JS signature/`n` solving or
PO-token/BotGuard generation; reimplementing HLS/SABR delivery; touching the
metadata client. Those stay out of scope unless the JS-less path dies.

History: the rollout was phased with an automatic yt-dlp fallback — native first
behind a switch, then the default with fallback, then removal (Phase 5). The
fallback no longer exists; §4.2 records the removal and §7 the risk that remains.

## 2. What the Phase 0 spike proved

All evidence is in the git-ignored `artifacts/native-stream/` directory
(`traffic.log`, `visionos-player-request.txt`, `visionos-player-response.json`,
comparison downloads). Nothing below required JavaScript, a JS runtime, or a PO
token.

### 2.1 Baseline: what yt-dlp did (historical)

The installed yt-dlp (`%APPDATA%\dev.jerimiah.rift\bin\yt-dlp.exe`,
`2026.08.19`) resolves the test track in ~3 s with *no* JS runtime and *no* PO
token providers configured. Its verbose log names the client it fell back to:
`Downloading visionos player API JSON` — the JS-less `VISIONOS` innertube client.
That is the client this implementation reproduces.

### 2.2 The validated clean-room request

1. **Visitor data** comes from the existing visitor pool
   (`rift-ytmusic::identity`), which scrapes `music.youtube.com`'s home page. A
   visitor fetched this way was accepted by the player endpoint on
   `www.youtube.com` as well.
2. **Player request**:
   - `POST https://www.youtube.com/youtubei/v1/player?prettyPrint=false`
   - headers: `Content-Type: application/json`,
     `X-Youtube-Client-Name: 101`, `X-Youtube-Client-Version: 1.02`,
     `X-Goog-Visitor-Id: <visitor>`, `Origin: https://www.youtube.com`,
     and the client's `VISIONOS` user agent (see below).
   - body:

     ```json
     {
       "context": {
         "client": {
           "clientName": "VISIONOS",
           "clientVersion": "1.02",
           "deviceMake": "Apple",
           "deviceModel": "RealityDevice17,1",
           "userAgent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15",
           "osName": "visionOS",
           "osVersion": "26.5.23O471",
           "hl": "en",
           "timeZone": "UTC",
           "utcOffsetMinutes": 0
         }
       },
       "videoId": "<video id>",
       "contentCheckOk": true,
       "racyCheckOk": true
     }
     ```
   - `playbackContext`/`signatureTimestamp` is **not required**: omitting it
     still returns `playabilityStatus.status == "OK"` and the same formats.
3. **Response**: `streamingData.adaptiveFormats` (~23 entries for a music
   video), each with a direct `url`, `itag`, `mimeType`, `contentLength`,
   `audioQuality`, `bitrate`. The `VISIONOS` response has **no
   `signatureCipher`, no `n` throttling parameter, and no `pot` parameter** — the
   URLs are directly downloadable.
4. **Format selection**: itag `140` (AAC 128 kbit/s, `audio/mp4`, the same
   format Rift already requests through yt-dlp) is present. So are `141`
   (256 kbit/s), `139` (48 kbit/s), and Opus/WebM `249`/`250`/`251`.
5. **Download**: a plain `GET` of the itag-140 URL returned `audio/mp4`,
   `Content-Length: 3911684`, and a file whose decoded duration (241.65 s,
   ffprobe) matches the yt-dlp output.

### 2.3 The one download quirk: Range requests are load-bearing

A plain `GET` of the resolved URL is throttled to ~32 KB/s (the whole 3.9 MB
track took 121 s). The same URL with a `Range: bytes=…` request returned `206`
and the full 3.9 MB in 0.18 s. yt-dlp's own downloader uses ranged HTTP for the
same reason. **The native downloader must issue ranged GETs** (8 MiB chunks),
accept `206`, and fall back gracefully if a server ignores `Range` and returns
`200`.

### 2.4 What is still unknown / risky

- The `VISIONOS` client identity and `clientVersion` are maintained by upstream
  yt-dlp and can churn (client reshuffles happen multiple times a year; the
  research notes in `docs/innertube-research.md` §3.1–3.3 track this).
- Some videos may not be available through this client (region/age/removed), or
  may only expose non-m4a formats.
- A signed URL expires; a long download needs a re-resolve path.
- YouTube could start gating this client with PO tokens.

That is why yt-dlp stayed the fallback through Phase 4; Phase 5 removed it from
the product (§4.2).

## 3. Architecture

### 3.1 Resolver — `rift_ytmusic::player`

```rust
/// One directly downloadable audio stream resolved from the player endpoint.
pub struct AudioStream {
    /// Direct googlevideo URL (already signed/authorized; expires).
    pub url: String,
    /// Format id, e.g. 140.
    pub itag: u32,
    /// MIME type as reported, e.g. "audio/mp4; codecs=\"mp4a.40.2\"".
    pub mime_type: String,
    /// Total bytes as reported by the format (`Content-Length` equivalent).
    pub content_length: Option<u64>,
    /// Name of the client configuration that served this stream, e.g. "VISIONOS".
    pub client: &'static str,
}

/// Resolve the preferred audio stream for a video id through the client chain.
/// # Errors
/// ... (documented per workspace `missing_errors_doc` lint)
pub async fn resolve(video_id: &str) -> Result<AudioStream>;
```

The resolver is a **client table plus a resolution chain**, not one hard-coded
request:

- `ClientConfig` rows carry the wire facts for one JS-less innertube client: the
  context `clientName`/`clientVersion`, the matching
  `X-YouTube-Client-Name`/`X-YouTube-Client-Version` headers, the user agent, and
  a builder for any client-specific `context.client` extras (device fields, ...).
- `CHAIN` lists the rows to try in preference order. Every attempt reuses the
  same request path (same tolerant parser, same format-selection rule, same
  one-rotation visitor rule); adding a client is a table row plus a loopback
  test, never a second code path.
- The chain stops at the first client that yields a stream. A client-specific
  failure (non-403 HTTP status, `Unavailable`, `NoAudioFormat`, `Parse`) falls
  through to the next row; a transport/bootstrap/size failure ends the chain
  because it says nothing about the client. When every row fails, the preferred
  (first) client's error is returned, since that is the most representative.
- Visitor rotation is **one budget for the whole resolve**: a 403 evicts the
  visitor via `identity::ban` and retries the same client once; `LOGIN_REQUIRED`
  rotates the same way before being believed. Later clients are tried with the
  rotated visitor and never start another rotation.
- `AudioStream::client` names the row that served the stream, and every failed
  attempt logs the client name, so a live report can tell which row is doing the
  work.

**Why the chain currently has one entry.** On 2026-10-26 the other JS-less
clients yt-dlp knows were probed live against the same player endpoint with a
clean-room request (no `playbackContext`, no JS runtime, no PO token); raw
evidence is in `artifacts/native-stream/client-probe/` and
`client-coverage.json`. Only `VISIONOS` returned audio URLs that are *fully
downloadable*:

| client | id / version | clean-room result (2026-10-26) |
|---|---|---|
| `VISIONOS` | 101 / 1.02 | **usable** — direct URLs; 20/20 ids resolved, 20/20 full ranged GETs `206 audio/mp4` with matching `contentLength` |
| `ANDROID_VR` | 28 / 1.65.10 | playable, direct URLs, but googlevideo serves only the first ~1 MiB of each URL and rejects offset/later ranges with 403 (GVS PO-token gate; yt-dlp warns the same) — not shippable as a fallback |
| `TVHTML5_SIMPLY` | 75 / 1.0 | unplayable without a `signatureTimestamp`; playable with one, but every audio format carries `signatureCipher` (needs JS solving) |
| `MWEB` | 2 / 2.20261022.04.00 | same shape as `TVHTML5_SIMPLY` |
| `TVHTML5` | 7 / 7.20261021.13.00 | `UNPLAYABLE` / "The page needs to be reloaded.", with or without a playback context |
| `WEB` (Safari UA) | 1 / 2.20261023.01.00 | `UNPLAYABLE` / "Video unavailable" |
| `WEB_EMBEDDED_PLAYER` | 56 / 2.20261023.01.00 | `ERROR` / "This video is unavailable"; yt-dlp only gets a playable response by generating JS `encryptedHostFlags` |
| `IOS` | 5 / 21.26.4 | `OK`, but SABR-only: audio entries carry no `url` |

Shipping an `ANDROID_VR` fallback today would turn "VISIONOS churned → immediate
yt-dlp fallback" into "VISIONOS churned → 1 MiB downloaded → 403 → yt-dlp
fallback": strictly worse. The table and chain exist so that a working client is
a data change rather than an outage. **Revisit trigger:** a player client returns
direct audio URLs *and* a full ranged GET of those URLs succeeds (`206`, byte
count == `contentLength`, no 403 on offset ranges — the check in
`crates/rift-ytmusic/examples/resolve_stream.rs`, extended with the candidate
config), or PO tokens become in scope. The maintenance procedure is:

1. Capture the candidate's request from yt-dlp's **wire traffic**
   (`--print-traffic` + `--extractor-args "youtube:player_client=<name>"`), not
   from its source.
2. Probe it clean-room with the minimal request and confirm direct audio URLs
   (`streamingData.adaptiveFormats[*].url`, no `signatureCipher`).
3. Confirm the URL is fully downloadable with a ranged GET (`206`, exact byte
   count, and no 403 on a non-zero offset range).
4. Add a `ClientConfig` row (plus a context builder if the client needs
   client-specific fields), append it to `CHAIN`, update the table above, extend
   the loopback tests (request shape for the row, chain order — the
   `the_shipped_chain_is_the_verified_visionos_client` guard fails until then),
   and sanitize one recorded response into a fixture.
5. Run `cargo run -p rift-ytmusic --example resolve_stream` over the 20-id
   sample, then `cargo run -p rift --example native_probe`.

Other implementation notes:

- Reuse `identity::visitor_data` for the visitor id and `identity::ban` for
  rotation, mirroring `client::post`'s metadata-403 handling. The visitor cookie
  bug fixed in §4.1 (an empty `__Secure-YEC` must fall through to the page's
  `visitorData`) matters to every row: an empty visitor id is answered with
  `LOGIN_REQUIRED`.
- The player base URL is a `cfg(test)`-overridable constant (`player_base`, with
  the test-only `PlayerBaseOverride` in `crates/rift-ytmusic/src/player.rs`) so
  transport tests run against loopback without touching `context::home()`; the
  chain tests inject synthetic rows the same way.
- Tolerant `serde_json::Value` parsing, in the style of `parse.rs`: never fail a
  whole response over an unknown field; only the fields the resolver consumes
  are interpreted.
- Format preference: `140` → `141` → `139` → any `audio/mp4` → any
  audio-only format; reject entries whose `url` is absent or that carry
  `signatureCipher` (that would need JS solving — such formats are rejected
  rather than downloaded, and the fetch surfaces the error).
- `playabilityStatus.status != "OK"` is a distinct error (`Unavailable`) so the
  UI/toast can surface YouTube's reason; the reason string is capped before it
  becomes a user-facing message.

### 3.2 Native downloader — `rift::stream` (crate-private module)

```rust
/// Resolve and stream the audio into memory (playback path).
pub async fn fetch_bytes(video_id: &str) -> anyhow::Result<Vec<u8>>;

/// Resolve and stream the audio to `dest` (offline path; caller passes the
/// temporary .part path and renames on success).
pub async fn fetch_to_path(
    video_id: &str,
    dest: &Path,
    progress: impl FnMut(u64, u64) + Send,
) -> anyhow::Result<()>;
```

Implementation notes:

- Own `reqwest::Client` with `connect_timeout(5s)`, a **read** timeout (not a
  30 s total timeout) and no fixed body-size cap: the backend's shared client is
   built for small JSON and must not be reused for audio. Automatic
   gzip/brotli decompression is **disabled** for this client: the transfer works
   in raw byte offsets/`Content-Range`, so transparent decoding would corrupt
   both (media responses are not compressed anyway).
- Range chunks of 8 MiB; each ranged request is bounded by its own timeout.
  Write chunks as they arrive and count bytes.
- **Completion is authoritative, not assumed:** the transfer requires a known
  total — the resolver's `content_length` or a total learned from a
  `Content-Range` — and verifies the written byte count against that effective
  total. A stream that ends without a verifiable length, or short of its learned
  total, is rejected rather than published (a cleanly truncated body cannot be
  told apart from a complete one without a total).
- A `403`/`410` (expired signed URL) triggers one re-resolve and resumes from the
  bytes already written, but **only when the re-resolved stream is provably the
  same media**: same itag, same MIME type, and (when both sides report one) the
  same total length. Splicing a different rendition's remainder onto the existing
  prefix would pass every length check and publish corrupt audio, so that case
  fails and the caller's retry restarts from zero.
- `fetch_to_path` writes to the caller's temp path (`.part`) so a crash cannot
  publish a truncated offline copy; the existing rename/cleanup rules in
  `commands/downloads.rs` stay authoritative.
- `fetch_bytes` for playback keeps the current in-memory contract (the decoder
  needs the bytes); progress is not surfaced for playback.

### 3.3 Backend integration (native only)

- `fetch::fetch_track` (playback) and `commands::downloads` (offline) call the
  native path directly: `rift_ytmusic::player::resolve` → `rift::stream`
  (`fetch_bytes` for playback, `fetch_to_path` for offline).
- There is **no fallback backend, no environment switch and no external
  process**. A native failure surfaces as the fetch/download error: playback
  toasts it, the download worker cleans the `.part` file and lets the existing
  kept-offline retry/backoff decide whether to try again.
- Phase 1–4 had one (`RIFT_STREAM_BACKEND` = `auto`/`native`/`ytdlp`, native
  first with the unchanged yt-dlp path behind it). It was removed in Phase 5
  (§4.2); the code no longer contains a second fetch path.

### 3.4 Progress plumbing

Contract (shared by backend and UI):

- `rift_types::DownloadState` gains:

  ```rust
  /// Completed fraction of each in-flight download, in permille (0..=1000).
  /// Only tracks currently in `downloading` may appear. Absent = unknown (the
  /// track is queued but not the one actively transferring); the UI then keeps
  /// its indeterminate spinner.
  #[serde(default)]
  pub progress: std::collections::HashMap<String, u16>,
  ```

  Permille (not `f32`) keeps `DownloadState: Eq`, which the UI tests and event
  comparisons rely on.
- `rift::downloads::Downloads` grows a progress map behind its existing lock
  discipline; `set_progress(id, received, total)` clamps and records, and every
  lifecycle transition (`begin`, `finish`, `fail`, `mark_failed`, `remove`)
  clears the entry so a stale bar can never outlive a claim.
- Emission: the download worker emits a `BackendEvent::Downloads` snapshot at
  most every 100 ms per active track (and on the transition, as today). Whole
  snapshots are already how the UI consumes this state; no new event variant is
  needed.
- "Downloading, no fraction" means the track is **claimed but queued** behind the
  one actively transferring (the worker claims a whole batch up front); the UI
  keeps the spinner for it. There is no second backend to report progress on.

### 3.5 UI

- `State::download_fraction(&self, id) -> Option<f32>` reads
  `downloads.progress` (permille → 0.0..=1.0).
- The 54 px track row paints a thin determinate bar under the title/artist
  block when `DownloadStatus::Downloading` **and** a fraction is known:
  `x` = meta-column x, `width` = meta-column width, `y` = artist-line bottom + 3
  (`row.y + 46.5`), 3 px tall, radius 1.5, `theme.bg_hover` track +
  `theme.accent` fill — bottom edge 4.5 px inside the row, so no row-height or
  virtualization changes.
- No fraction (a queued track behind the active transfer) keeps today's 14 px
  spinner in the download slot. The two are complementary: spinner = in flight,
  bar = how far.
- `list.rs` passes the fraction into `TrackRowOpts`; `ui-spec.md` §7.1.3 and the
  state table are updated with the new row behaviour.

## 4. Phase plan and gates

| Phase | Work | Gate |
|---|---|---|
| 0 | Spike: validate JS-less resolution, visitor reuse, ranged download | **Done** — §2 |
| 1 | `rift_ytmusic::player`: resolver module, fixture + loopback tests | **Done** — 71 lib + 15 fixture tests; client table/chain covered; live resolve on 2 ids with ranged `206`, plus a 20-id coverage sample (§3.1) |
| 2 | `rift::stream`: native ranged downloader + `Downloads` progress plumbing + fallback wiring | **Done** — native ranged downloader (range/resume/200-fallback/size/expiry), progress lifecycle, loopback tests; the fallback wiring added here was removed again in Phase 5 |
| 3 | UI: `DownloadState.progress`, row bar, list/state plumbing, tests | **Done** — 314 lib + 13 ui_flow `rift-desktop` tests after the Phase 5 removal; no-bar/bar geometry pinned; `ui-spec.md` updated |
| 4 | Live verification: mixed id sample, native-vs-yt-dlp comparison (bytes/duration/decodability), failure cases, flip `auto` to the default | **Done** — probe §4.1 passes 5/5; playback smoke, download worker (progress snapshots, restart/offline replay) and UI capture recorded; default flipped to `auto` (that switch and the comparison backend were removed in Phase 5) |
| 5 | Remove yt-dlp from the product: fetch path, installer/detection commands, Settings section, `RIFT_YTDLP`/override surface, README requirement, docs | **Done** — §4.2 records the removal and its native-only verification; full local CI gate green |

Phase 5 was originally planned for after a release soak. It started immediately
after the Phase 4/controller-level proof instead: every automatable path (resolve,
playback fetch, download worker, restart/offline replay, UI capture, controller
e2e) was green, and the remaining human-only checks cannot be performed by the
implementation environment. Those checks are release-verification items (§4.2).

### 4.1 Phase 4 verification report (2026-10-26, pre-removal)

> Historical snapshot: this report was captured while the native path was the
> default with yt-dlp behind it. The yt-dlp columns, the `RIFT_STREAM_BACKEND`
> references and §4.1.4/§4.1.5's "fallback" framing describe that state; they are
> kept as the verification record for the switch that Phase 5 then removed.

`cargo run -p rift --example native_probe` over five real ids (mixed ATV/OMV
uploads, 3.4–7.0 MB). Each row is one backend; "decoded" is the playback
decoder's total duration, which is the contract — the byte counts differ
slightly between sessions because the same itag can be remuxed under different
URL instances.

| id | native | yt-dlp | decoded |
|---|---|---|---|
| `K0HSD_i2DvA` | 3,911,684 B / 873 ms | 3,908,920 B / 2869 ms | 241.65 s both |
| `Jb6gcoR266U` | 6,953,417 B / 634 ms | 6,948,393 B / 2564 ms | 429.53 / 429.57 s |
| `dQw4w9WgXcQ` | 3,449,447 B / 356 ms | 3,447,031 B / 2427 ms | 213.09 s both |
| `fJ9rUzIMcZQ` | 5,818,745 B / 597 ms | 5,814,688 B / 2692 ms | 359.49 s both |
| `hTWKbfoikeg` | 4,506,479 B / 652 ms | 4,503,371 B / 2494 ms | 278.41 s both |

The invalid id `xXxXxXxXxXx` fails on both backends and the native error is
`video unavailable: This video is unavailable` (the exact reason string is
pinned by a fixture test), so the UI/toast gets a useful message.

One pre-existing bug surfaced during the live verification and was fixed in
`identity.rs`: YouTube now sets `__Secure-YEC` as an empty cookie, and the
visitor bootstrap preferred the cookie over the page's `visitorData`, so every
request carried an empty `X-Goog-Visitor-Id` and the player/metadata endpoints
answered `LOGIN_REQUIRED`. An empty cookie now falls through to the page value
(non-empty still wins; regression test added). This also restored live metadata
calls, which had the same exposure.

#### 4.1.1 Playback path under default `auto`

`cargo run -p rift --example smoke -- "daft punk get lucky"` drove the real
backend through search → native resolve → ranged fetch → decode. yt-dlp was
detected but not used:

- `rift_ytmusic::player: resolved native audio stream … itag=140 content_length=5983881`
- `rift::stream: native audio range request range=bytes=0-8388607`
- `rift::fetch: 4D7u5KF7SP8: native stream served playback`
- 5,983,881 B fetched, decoded 369.63 s, `SMOKE PASS: search -> fetch -> decode`.

#### 4.1.2 Download worker, progress and restart/offline replay

`cargo run -p rift --example download_probe` drives the real `RiftBackend` with a
throwaway data dir (default `auto`):

- The worker emitted **2 progress snapshots** on a fast link (`0‰`, `85‰` for the
  single in-flight track) as `Downloads` events; the offline file is
  3,449,447 B and decodes to 213.09 s.
- A **second backend on the same data dir** discovers the offline copy and
  `fetch::fetch_track` replays it from disk — "restart detects the offline copy
  and fetch_track replays it without the network". PASS.
- Same probe with `RIFT_STREAM_BACKEND=ytdlp`: **0 progress snapshots**,
  3,447,031 B, decodes 213.09 s — the spinner-only (indeterminate) fallback
  semantics are intact.

The probe example initializes `RUST_LOG` now, so the backend-path logs above are
part of its output.

#### 4.1.3 UI visual capture

A dev-only `RIFT_PREVIEW_DOWNLOAD=<permille>` environment variable in preview
state (documented in `docs/architecture.md`'s preview section) produced
`artifacts/native-stream/download-bar-liked.png` via
`cargo run -p rift-desktop -- --preview --screenshot … --view liked`: it shows
the determinate bar and in-flight spinner on "Song Title 2", the header's
"Downloading 1/8", and spinner-only on the queued "Song Title 3".

With the playback path, the download worker (including progress and
restart/offline replay), the UI capture and the fallback trigger covered by
automated evidence at the time, the remaining checks are the human-only release
items (task-5): launch the app, play a track, download one, watch the bar, restart
with the network off, and confirm a native failure produces an actionable error
(there is no fallback to observe any more).

#### 4.1.4 Automatic fallback trigger (historical — fallback removed in Phase 5)

The safety property behind keeping yt-dlp was verified directly, not inferred:
a `cfg(test)`-only failure injection in `fetch::fetch_native` makes the native
fetch fail, and the real default-`auto` dispatch must then serve playback through
yt-dlp. The ignored live test

```text
cargo test -p rift --lib -- --ignored --nocapture auto_mode_falls_back
```

passed (1 passed, 2.85 s) on 2026-10-26 with the app's installed yt-dlp: the
forced failure produced >100 KB of decoded audio via the legacy path. The hook is
compiled out of release builds, and the test is `#[ignore]`d in CI. Artifact:
`artifacts/native-stream/fallback-live-test.txt`.

#### 4.1.5 Controller-level end-to-end (live)

The last code-level gap between "backend works" and "the app does it" is the
controller/event path the UI drives. A new ignored live test in
`crates/rift-desktop/tests/live_backend.rs`
(`live_native_playback_download_progress_and_offline_restart`) walks one track
through the real `Controller` + `App` state machine on a throwaway data dir
(default `auto`):

1. `Action::PlayTrack` → `ControllerEvent::Track` + `PlaybackState::Playing`, and
   playback position ticks (`Progress.position > 0.5 s`).
2. `Action::DownloadTracks` → at least one `Downloads` snapshot carrying the
   in-flight `DownloadState.progress` entry before the completed state; offline
   file on disk >100 KB.
3. Backend shutdown + fresh backend/controller on the same store → `bootstrap`
   rediscovers `downloads.downloaded`, and a second `Action::PlayTrack` reaches
   `Playing` again from the offline copy.

Run:

```text
cargo test -p rift-desktop --features backend --test live_backend -- \
  --ignored --nocapture --test-threads=1 live_native_playback_download_progress_and_offline_restart
```

Passed (1.75 s) on 2026-10-26; artifact `artifacts/native-stream/live-controller-e2e.txt`.
What remains for the human pass (task-5) is only what automation cannot assert:
it sounds right, the bar looks right in the live window, and offline playback
works with the network actually disconnected.

### 4.2 Phase 5 removal record (2026-10-26)

Removal started after the controller-level proof above instead of waiting for a
release soak (§0); it is recorded here because the earlier sections describe a
fallback that no longer exists.

Removed:

- `rift::fetch`'s entire yt-dlp half: `fetch_via_ytdlp`, the installer
  (`download_ytdlp` + checksum verification and the `sha256` module),
  detection/override (`RIFT_YTDLP`, `ytdlp_path`, `set_ytdlp_override`,
  `detect_ytdlp`, `validate_ytdlp_override`), the `YTDLP_*` ceilings, the
  `StreamBackend` enum and the `RIFT_STREAM_BACKEND` switch.
  `fetch_bytes` is now a thin native call; `fetch_track` keeps its
  offline-copy-first behaviour.
- `commands/ytdlp.rs` (check / set-path / install commands), the settings
  `yt_dlp_path` field, `Bootstrap.yt_dlp_path`, `rift_types::YtDlpStatus`, and
  the backend-startup override.
- The download worker's fallback branch (native-only now; `.part` + rename +
  progress + kept-offline retry semantics unchanged) and the fallback's
  indeterminate-spinner special case (only queued tracks remain fraction-less).
- The desktop Settings "yt-dlp" section, the controller actions/events/state it
  drove, the live tests that seeded a yt-dlp path, and the examples/tooling/doc
  references.
- Backward compatibility: an existing `<data dir>/bin/yt-dlp[.exe]` and a stale
  `yt_dlp_path` key in an old `settings.json` are ignored and left untouched
  (never delete user data); a load test pins the legacy-settings case.

Verification after removal (all recorded in `artifacts/native-stream/phase5-*`):

- **The installed yt-dlp binary was hidden for the whole run** (moved aside and
  restored afterwards), so nothing could silently shell out to it:
  - `cargo run -p rift --example native_probe` → 5/5 ids resolved, downloaded
    (3,449,447–6,953,417 B) and decoded (213.09–429.53 s); the invalid id failed
    with `video unavailable: This video is unavailable`.
  - `cargo run -p rift --example download_probe` → native progress snapshots,
    saved file, restart discovery + offline replay PASS, including the seeded
    legacy install (`settings.json` with `yt_dlp_path`, stale `bin/yt-dlp.exe`):
    settings loaded, binary left untouched.
  - `cargo run -p rift --example smoke` → native `fetch: 5,983,881 B`, decode
    369.63 s, `SMOKE PASS`.
  - the ignored controller live test
    (`live_native_playback_download_progress_and_offline_restart`) → PASS (2.0 s).
- `cargo test -p rift` 210 passed; `cargo test -p rift-ytmusic` 71 + 15 fixture
  passed; `cargo test -p rift-desktop` 314 lib + 13 ui_flow passed; clippy /
  fmt / rustdoc clean; the full `pwsh tools/ci-local.ps1` gate passed
  (`artifacts/ci-local-*`).
- Symbol audit: zero `yt-dlp`/`yt_dlp`/`YtDlp`/`YTDLP`/`RIFT_STREAM_BACKEND`/
  `sha256` matches in `crates/rift-desktop/**` or `.github/**`. The only
  examples-level mentions are `download_probe.rs`'s intentional legacy-install
  compatibility seed (it must name the stale `yt_dlp_path` key/bin); the other
  examples are clean. `crates/rift/src` keeps only the required legacy-settings
  load test (which must name the removed JSON key) and a one-line
  removal-history comment.

**Post-removal review hardening (same day).** An adversarial review of the new
audio path found and fixed these before hand-off (each has a regression test):

- re-resolve after URL expiry now refuses to splice a different itag/MIME/total
  onto the partially written file (§3.2);
- completion verifies against the total learned during the transfer, and rejects
  streams with no verifiable length (§3.2);
- startup ignores/removes a zero-byte `.m4a` instead of showing a phantom
  offline copy;
- the audio HTTP client opts out of transparent gzip/brotli;
- `identity.rs` bounds the visitor-bootstrap body and reports non-2xx responses;
- the resolver only bans the rejected visitor while a rotation is still
  available, and caps a YouTube "unavailable" reason before it becomes a toast;
- `download_probe` now fails if a completed download never reported a progress
  fraction, and `resolve_stream` also checks a mid-offset range (the
  `ANDROID_VR`-style offset gate).

The remaining review items are recorded in §8 as hardening backlog, not release
blockers except where noted.

Human-only release checks remain (task-5): listen to playback, watch the bar fill
in the live window, restart with networking actually disconnected, and observe a
native-failure error/toast.

## 5. Clean-room and licensing policy

- The resolver is implemented from **observed HTTP behaviour**: recorded
  request/response traffic and live responses captured in
  `artifacts/native-stream/`. The request constants above are protocol facts
  (client name/version/headers), not code.
- No source is copied from `rustypipe` (GPL-3.0) — the crate stays Apache-2.0.
  yt-dlp is Unlicense/public domain; it was consulted as a behavioural oracle
  during Phases 0–4, but the implementation is written from the wire contract,
  and the `--print-traffic` capture was taken from the installed binary without
  decompiling it.
- yt-dlp is **no longer part of the shipped app**: it was only the Phase 1–4
  fallback and the manual verification oracle, both removed in Phase 5. Its
  installed binary under `%APPDATA%\dev.jerimiah.rift\bin` is left in place on
  developer machines but not called.

## 6. Test strategy

- **Unit/fixture tests** (committed, offline): format selection and player
  response parsing against a sanitized recorded fixture (no signed URLs, no
  visitor data committed); error mapping for `playabilityStatus`.
- **Loopback transport tests** (committed, offline): a `std::net::TcpListener`
  server, as `client.rs` already does, exercising the request we build (per
  client-table row), header values, visitor rotation on `403`, chain order and
  fall-through (a synthetic second row, plus a client-specific failure that must
  not rotate twice), the `LOGIN_REQUIRED` path, and the `.part` downloader's
  range/offset accounting.
- **Manual live probes** (not in CI): `crates/rift-ytmusic/examples/resolve_stream.rs`
  resolves the 20-id coverage sample (or explicit ids) and ranged-GETs each
  (whole-file 206 + length check + a mid-offset 64 KiB range, printing the
  serving client); `crates/rift/examples/native_probe.rs` fetches ids through the
  playback path (`fetch::fetch_bytes`) and checks bytes + decoded duration;
  `crates/rift/examples/download_probe.rs` drives the real backend's download
  worker through progress + restart/offline replay and fails if a completed
  transfer never reported a progress fraction; and the ignored controller live
  test in `crates/rift-desktop/tests/live_backend.rs` covers the app-level
  play/download/restart path. All run on a networked machine; results go to
  `artifacts/native-stream/`.
- No committed test may contact `youtube.com`/`googlevideo.com`; the CI
  environment has no network.

## 7. Risks and mitigations

**The primary risk is now structural:** the native path depends on one JS-less
client family (`VISIONOS` today), and since Phase 5 there is no yt-dlp fallback.
This was a deliberate, documented trade-off (see §0/§8), not an oversight.

| Risk | Mitigation |
|---|---|
| `VISIONOS` client/config churn | The resolver is a client table + chain (`CHAIN` in `player.rs`): a new client is one row, a context builder and a loopback test (§3.1). `examples/resolve_stream.rs` over the 20-id sample is the re-verification step after any resolver change. Until a second fully-downloadable JS-less client exists, a VISIONOS gate is a native-path outage the release must react to (revisit trigger in §3.1). |
| Another client's URLs look usable but are PO-token gated | Never add a row without a full ranged GET: `ANDROID_VR` resolves with direct URLs but only the first ~1 MiB is served without a GVS PO token (§3.1) — a fallback that starts a transfer and 403s is worse than none. |
| PO-token gating arrives for `VISIONOS` | There is no fallback; the resolver's revisit trigger (§3.1) and the "add a client" procedure are the response. If no JS-less client survives, PO-token/BotGuard scope returns to the roadmap (previously out of scope, §1). |
| Signed URL expiry mid-download | re-resolve once on 403/410, identity-checked before resuming, then surface the error (retries/backoff for kept-offline downloads) |
| Throttled/plain GET performance | ranged requests are mandatory (§2.3) |
| Empty visitor cookie (observed 2026-10-26: YouTube sets `__Secure-YEC` empty) | fixed in `identity.rs`: an empty cookie falls through to the home page's `visitorData`; pinned by a regression test (§4.1) |
| Non-m4a-only videos | prefer m4a; a video without a directly downloadable m4a/audio format surfaces a fetch error (no remux backend anymore) |
| Progress events flooding the UI | 100 ms throttle; permille granularity |
| Arbitrary URL in a player response / redirect | fixed 2026-10-26: before any bytes are requested the signed URL must be `https` on `googlevideo.com`/`*.googlevideo.com`, with no embedded credentials; the audio client only follows redirects that satisfy the same rule (`rift::stream` regression tests, §8). |
| Unbounded body from a server that ignores `Range` | the `200`-instead-of-`206` fallback is capped at 256 MiB for the in-memory playback sink and the transfer fails with a clear error instead of growing without bound; offline file sinks are bounded by the filesystem (§8). |
| A slow-dripping server keeps a download claim alive | each read is read-timeout-bounded, but there is no total/no-progress deadline yet; backlog (§8) |

## 8. Open questions / later

- Adding a second fully-downloadable JS-less client is now the highest-value
  resilience work (it is no longer optional polish): re-test the candidates when
  YouTube's client experiments change or when PO tokens come into scope.
  `ANDROID_VR` is the closest candidate (direct URLs, GVS-gated past ~1 MiB
  today); `IOS` is SABR-only and the TV/web clients need JS signatures. Use the
  §3.1 maintenance procedure; never ship a row whose URLs are not fully
  downloadable.
- HLS (`m3u8`) delivery appears in the player response as an alternative
  manifest; not needed while progressive itag 140 is offered.
- Whether playback should prefer 141 (higher bitrate) now that the native decoder
  path is stable — out of scope; parity with the historical `140` choice.

### Hardening backlog from the post-removal review (2026-10-26)

None of these is a known live bug; they are follow-ups the review surfaced. The
items marked *fixed* are already in the tree with regression tests (§4.2).

- **URL trust boundary:** *fixed 2026-10-26.* The signed URL is validated before
  any request (`https` only, host must be `googlevideo.com` or a
  `*.googlevideo.com` subdomain, no embedded credentials), and the audio client's
  redirect policy follows only redirects that satisfy the same rule
  (`rift::stream::validate_stream_url` / `allowed_stream_url`, unit-tested). A
  hostile/compromised player response can no longer aim the fetch at an arbitrary
  host.
- **Unbounded `200` body:** *fixed 2026-10-26.* If an edge ignores `Range` and
  answers `200`, the playback (in-memory) sink is capped at 256 MiB and the
  transfer fails with an explicit error instead of growing until the process is
  OOM-killed (`rift::stream::check_memory_cap`, unit-tested); offline downloads
  are bounded by the filesystem.
- **Per-resolve HTTP client:** *fixed 2026-10-26.* `rift_ytmusic::player` builds
  one process-wide `reqwest::Client` and clones it per call, so the
  resolve→re-resolve path keeps its connection pool instead of paying a new
  TCP+TLS handshake each time.
- **No-progress deadline:** only a per-read timeout exists; a server sending one
  byte every 29 s keeps a download claim/UI row alive indefinitely (playback has
  no cancel). Suggested: bytes/`no-progress` budget per transfer.
- **Remove while downloading:** `Downloads::remove` neither cancels nor clears
  the in-flight claim; the worker can finish afterwards and re-publish the file.
  The UI does not offer remove for an in-flight row today, but the backend
  invariant is loose.
- **Toast flood:** a fully failed fresh batch emits one `Error` per track
  (kept-offline retries are coalesced). Suggested: coalesce per pass.
- **Progress event cost:** every snapshot clones the whole `DownloadState`
  (including large downloaded sets) at ≤10 Hz; a dedicated lightweight progress
  event or change-gated emission would scale better with huge offline libraries.
- **`safe_stem` edge cases:** case-insensitive filesystems can collide for ids
  differing only by case, and Windows reserved names (`CON`, `NUL`, …) are not
  neutralized; an imported playlist can carry such ids. Fixing needs offline-file
  migration, so it is deferred.
- **Last-resort non-m4a formats:** if 140/141/139/`audio/mp4` are all absent the
  resolver may pick Opus/WebM, which the decoder may or may not accept; verify
  the decoder path before relying on it (or return `NoAudioFormat` instead).
- **Test gaps:** no unit test drives the download worker itself (rename, `.part`
  cleanup, progress emission are only covered by the live probes); `identity.rs`
  has no loopback test for the empty-`__Secure-YEC` → outgoing-header path; the
  player's body-size ceiling is not `cfg(test)`-overridable, so oversized-body
  responses are untested; `stream.rs` does not test a `200` in response to a
  mid-download range, an overrun body, or a `416` at offset.
