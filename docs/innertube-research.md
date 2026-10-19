# innertube / YouTube Music client — research notes

Status: **complete**. Part 1 (rustypipe contract) is sourced from the local crate
source at `~/.cargo/registry/src/.../rustypipe-0.11.4`. Part 2 (yt-dlp), Part 3
(anti-bot + alternatives), and Part 4 (design/plan) are sourced from the yt-dlp
repo/docs and crates.io/lib.rs research.

> **Licensing note.** rustypipe is **GPL-3.0**. These notes describe the
> *observable* innertube API surface (endpoint paths, header names, field names,
> magic strings) in our own words — not rustypipe code. Any reimplementation
> that keeps Rift Apache-2.0 must be clean-room: do not copy or translate
> rustypipe source.

---

## Part 1 — the innertube request contract (rustypipe side)

### 1.1 Endpoints

All requests are `POST …/youtubei/v1/{endpoint}?prettyPrint=false` (JSON body,
`Content-Type: application/json`, gzip/brotli accepted). There are three base
hosts:

| Base host | Used by client type |
|---|---|
| `https://www.youtube.com/youtubei/v1/` | Desktop (`WEB`), TV |
| `https://music.youtube.com/youtubei/v1/` | Desktop Music (`WEB_REMIX`) |
| `https://m.youtube.com/youtubei/v1/` | Mobile web (`MWEB`) |
| `https://youtubei.googleapis.com/youtubei/v1/` | Android, iOS |

No `key=` API key is sent (rustypipe dropped it in v0.2.0); only
`?prettyPrint=false` is appended.

Endpoint → feature map (the only ones Rift needs):

| Feature | endpoint | body |
|---|---|---|
| music search (tracks/artists/albums) | `search` | `{ context, query, params? }` |
| artist page | `browse` | `{ context, browseId }` |
| album page | `browse` | `{ context, browseId }` |
| playlist | `browse` (+ `browse` continuation) | `{ context, browseId }` / `{ context, continuation }` |
| track details | `next` | `{ context, videoId, … }` |
| radio | `next` | `{ context, playlistId, params, … }` |
| stream resolution | `player` | `{ context, videoId, … }` |

Identifier prefixes used in `browseId`/`playlistId` (needed to build requests):
albums are `MPREb_…`, the artist album-tab is `MPAD` + artist id, playlists are
`VL` + id, radio seeds are `RDAMVM{videoId}` / `RDAMPL{playlistId}`.

All bodies are wrapped as `{ "context": <context>, ...<body> }`.

### 1.2 Client identity

Per-request headers:

| Client | `X-YouTube-Client-Name` | context `clientName` | platform |
|---|---|---|---|
| Desktop web | `1` | `WEB` | DESKTOP |
| **Desktop Music** | `67` | `WEB_REMIX` | DESKTOP |
| Mobile web | `2` | `MWEB` | MOBILE |
| Android | `3` | `ANDROID` | ANDROID |
| iOS | `5` | `IOS` | IOS |
| TV | `7` | `TVHTML5` | TV |

Also sent: `X-YouTube-Client-Version` (the client version, fetched dynamically),
`X-Goog-Api-Format-Version: 2`, `Origin`/`Referer` (the matching home URL),
a consent cookie, and — when a visitor id is known —
`X-Goog-EOM-Visitor-Id: <visitor data>`.

The **client version is not hardcoded**: it is scraped from the site's own
`INNERTUBE_CONTEXT_CLIENT_VERSION` (regex over the home-page HTML/JS) and
cached. A reimplementation must do the same, or requests start 404ing the
moment the real client versions roll forward.

### 1.3 Context object

Every body embeds a `context` of roughly:

```json
{
  "client": {
    "clientName": "WEB_REMIX",
    "clientVersion": "<scraped>",
    "platform": "DESKTOP",
    "originalUrl": "https://music.youtube.com",
    "visitorData": "<id>",
    "hl": "en", "gl": "US",
    "timeZone": "UTC", "utcOffsetMinutes": 0
  },
  "request": { "internalExperimentFlags": [], "useSsl": true },
  "user": {}
}
```

(`request` is desktop-only; `user` is empty for anonymous use.)

### 1.4 Visitor data

Anonymous identity is a "visitor data" id (base64, starts `Cg…`). It is **not
client-random**: rustypipe fetches it from YouTube's consent session — a GET to
`music.youtube.com` with `Cookie: SOCS=CAISAiAD`, reading the `__Secure-YEC`
`Set-Cookie` (falling back to a `"visitorData":"…"` regex over the HTML).
rustypipe keeps a small pool (~50, hard cap 20) and rotates an id out when a
response is flagged/403. It's sent both in `context.client.visitorData` and the
`X-Goog-EOM-Visitor-Id` header. A reimplementation needs the same
fetch → use → rotate-on-failure loop.

### 1.5 Auth (only relevant for logged-in features)

- Web clients: `Authorization: SAPISIDHASH {unix}_{sha1hex(unix + " " + sapisid + " " + origin)}`
  where `sapisid` comes from the account cookie and `origin` is the home URL
  for that client. Plus `X-Goog-AuthUser` / `X-Goog-PageId` and the cookie.
- TV client: `Authorization: Bearer <access_token>`.

Rift is anonymous today, so auth is out of scope unless private/age-restricted
content becomes a goal.

### 1.6 Search filters are protobuf

The `search` body's optional `params` field encodes filters (type = artists /
albums / songs, etc.) as a **base64url+urlencoded protobuf blob**. rustypipe
hand-writes the wire format (`util/protobuf.rs`, `param/search_filter.rs`). The
precomputed Music filter blobs are:

- Tracks: `EgWKAQIIAWoQEAMQBBAJEAoQBRAREBAQFQ%3D%3D`
- Albums / Artists: same shape, different type-tag byte
  (`EgWKAQIYAWoQ…` / `EgWKAQIgAWoQ…`) — capture the exact bytes from
  `param/search_filter.rs` (`MusicSearchFilter::params`) or by recording one
  request.

A reimplementation can hardcode the three blobs Rift needs (tracks/albums/
artists) and skip a general protobuf encoder.

Note the exact bytes differ between implementations: yt-dlp's music sections use
`EgWKAQIIAWoKEAoQAxAEEAkQBQ==` (songs) / `EgWKAQIYAWoKEAoQAxAEEAkQBQ==`
(albums) / `EgWKAQIgAWoKEAoQAxAEEAkQBQ==` (artists), while rustypipe's tracks
blob is `EgWKAQIIAWoQEAMQBBAJEAoQBRAREBAQFQ%3D%3D`. Either form works — capture
one by recording a live request rather than trusting a single source.

### 1.7 Stream resolution (the hard part)

`player` body:

```json
{
  "context": { ... },
  "videoId": "<id>",
  "contentCheckOk": true,
  "racyCheckOk": true,
  "playbackContext": {
    "contentPlaybackContext": {
      "signatureTimestamp": "<from player.js>",
      "referer": "https://www.youtube.com"
    }
  },
  "serviceIntegrityDimensions": { "poToken": "<...>" }
}
```

Two load-bearing, moving pieces:

1. **Signature / n-parameter deobfuscation.** The stream URLs in the response are
   signed and must be deobfuscated by running the video player's JavaScript.
   rustypipe embeds a JS interpreter (QuickJS via `rquickjs`), caches the player
   JS, extracts the `signatureTimestamp`, and executes the deobfuscation
   function. This is a large, churn-prone subsystem (deobfuscate.rs + a 438 KB
   dictionary + a JS parser).
2. **PO tokens.** Since ~Aug 2024, web clients need a "proof of origin" token to
   stream. rustypipe does **not** generate these itself — it shells out to a
   separate `rustypipe-botguard` binary (a browser-simulated BotGuard), passing
   `--snapshot-file … -- <ids>` and reading the token JSON back. The iOS client
   used to be exempt; per Rift's own notes it now only serves ~1 MB and no
   longer avoids this.

**Conclusion for streaming:** the signature interpreter and PO-token machinery
are the two things that keep breaking. This is exactly where yt-dlp concentrates
its maintenance effort. A from-scratch client should **not** take this on;
stream audio via yt-dlp (or an equivalent maintained binary) instead.

### 1.8 Response parsing

Responses are parsed with serde into typed structs, but with a custom
`MapResult<T>` wrapper that downgrades "this one item failed to parse" to a
warning instead of failing the whole request, and uses `serde(other,
deserialize_with = "deserialize_ignore_any")` so unknown/new response shapes
don't hard-fail. A reimplementation should copy this *philosophy* (tolerant,
partial-failure parsing) because innertube responses are large, nested, and
frequently add/rename fields.

### 1.9 One recording, two video ids

A music item states its kind in
`watchEndpoint.watchEndpointMusicSupportedConfigs.watchEndpointMusicConfig`
`.musicVideoType` (`…_ATV` = art track, the topic channel's album audio; `…_OMV`
= official music video; `…_UGC` = user upload). YouTube Music describes one
*recording* with one video per upload, and different surfaces pick different
uploads — measured 2026-10-10 and re-checked 2026-10-18 against Daft Punk's
*Homework* (`MPREb_dMHztPsrDgN`) and artist page (`UCRr1xG_2WIDs18a6cIiCxeA`):

| row | id | type | carries |
|---|---|---|---|
| album page, "Around the World" | `K0HSD_i2DvA` | `…_OMV` on 2026-10-18; **absent** on 2026-10-10 | duration 430 s, **no** artist credit |
| artist page → Top songs, same song | `Jb6gcoR266U` | `…_ATV` | `artist` = Daft Punk, **no** duration |
| artist page → Videos carousel | `K0HSD_i2DvA` | `…_OMV` | 135M views |

The type is not reliably present on a row — the first measurement found none on
the album row and the second found `…_OMV` — so nothing may depend on it being
there. (Rift's duplicate-song rule does not.)

Both ids resolve to different YouTube uploads ("…(Official Music Video
Remastered)" and the topic channel's "Around the World"), and the album page's
row names **only** `K0HSD_i2DvA` — on the inline play button's `watchEndpoint`,
on `playlistItemData.videoId` *and* on the title run's `watchEndpoint`. Neither
row names the other's id, so no choice of pointer in `parse::video_id` can
reconcile them, and the two surfaces disagree about which fields they carry:
album rows omit the byline (the page header has it; the row's own column is the
play count), Top songs rows omit the duration.

Two consequences for a client:

- **Do not key a song on the video id alone.** Rift's likes, recently played,
  playlist duplicate check and credit backfill compare songs through
  `Track::same_song` (`crates/rift-types/src/lib.rs`), and the UI asks a `SongIndex` built
  from the liked list, so the heart agrees with the library whichever surface
  rendered the row. The rule uses the title plus whichever of compatible
  credits, known durations (a couple of seconds apart is one recording) and
  album id both rows carry.
- **The two ids are still two streams.** Downloads and the playback queue stay
  keyed on the video id: an offline copy is one upload's audio file, and the
  other id is a different upload to play.

---

## Part 2 — yt-dlp internals

> **License: Unlicense (public domain).** Its logic can be reimplemented in Rust
> with no copyleft concern — the opposite of rustypipe's GPL-3.0.

### 2.1 Architecture

yt-dlp is a Python CLI/library in three layers:

- **Extractors** (`yt_dlp/extractor/`) — turn a URL/query into an "info dict"
  (metadata + a list of formats). The YouTube extractors live in the package
  `yt_dlp/extractor/youtube/` (`_base.py` = shared client/auth plumbing,
  `_video.py` = `YoutubeIE`, `_tab.py`/`_search.py` = browse/search, plus `jsc/`
  for the n/sig solver and `pot/` for PO tokens). There is **no `YoutubeMusicIE`**
  — music is folded into `YoutubeIE` / `YoutubeTabIE` / `YoutubeMusicSearchURLIE`
  via the `web_music` (`WEB_REMIX`) client.
- **Downloader** (`yt_dlp/downloader/`) — fetches the chosen format.
- **Postprocessors** (`yt_dlp/postprocessor/`) — remux/embed, etc.

Rift uses only the extractor+downloader half, via
`yt-dlp -f 140/bestaudio[ext=m4a] -o <tmp> <url>` — i.e. "resolve the best m4a
audio (itag 140 = AAC ~128 kbit/s) and write it to a file."

### 2.2 Format resolution

The extractor POSTs the innertube `/player` endpoint (same contract as §1.7)
and reads `streamingData.formats` (progressive) and
`streamingData.adaptiveFormats` (separate DASH video-only / audio-only). It
builds each format dict **directly from the API fields — there is no hardcoded
itag table**: `format_id` = `itag`, `ext`/`acodec`/`vcodec` from `mimeType`,
`abr` from `bitrate`/`averageBitrate`, `asr` = `audioSampleRate`, channels =
`audioChannels`, size = `contentLength`, and a `quality` rank from `audioQuality`
(ladder `tiny < audio_quality_ultralow < low < medium < high < …`). Reference
audio itags: 139 (AAC 48k), **140 (AAC 128k, m4a, `audio_quality_medium`)**,
141 (AAC 256k), 249/250/251 (Opus). `-f 140/bestaudio[ext=m4a]` = "use 140 if
present, else the best m4a audio-only" — `bestaudio[ext=m4a]` alone would
normally pick 141 (256k), which is why Rift prefixes `140/`.

### 2.3 Signature / n deobfuscation (jsinterp)

Stream URLs carry a `signatureCipher` (an `s` field + `sp` param name) and/or an
`n` throttle param. Solving them means executing YouTube's real obfuscated player
`base.js`. yt-dlp's old pure-Python `jsinterp.py` interpreter is **no longer used
for YouTube**; the current "JS Challenge Provider" framework (`jsc/`) runs two
hash-verified solver scripts (`yt.solver.lib.js` + `yt.solver.core.js` from the
`yt-dlp/ejs` project) inside a **real external JS runtime** — deno ≥2.3 (default),
node ≥22, bun, or QuickJS — and reads the solved `sig` (a permutation) and `n`
back as JSON. This is the same churn that broke rustypipe; without a solver,
yt-dlp falls back to the JS-less `visionos` client.

### 2.4 Client selection

`INNERTUBE_CLIENTS` maps client names to contexts + per-protocol PO-token
policies (same names as §1.2; `web_music` = WEB_REMIX, name 67, host
`music.youtube.com`). Default sets: `('visionos','web')`, with `visionos` as the
JS-less fallback (no sig/n solving needed). There is **no hardcoded API key** in
current yt-dlp either. yt-dlp solves sig/n externally and pulls PO tokens from a
pluggable provider only when needed; client choices churn frequently (tv vs mweb,
iOS formats dropped Jan 2025, `tv_downgraded`/`web_embedded`/`visionos` added
2026).

### 2.5 Relevance to Rift's reimplementation

Rift shells out to yt-dlp purely as a **stream resolver/downloader** (the
`fetch_via_ytdlp` path in `fetch.rs`); it does not use yt-dlp for metadata. So
the reimplementation only needs yt-dlp's *resolution* behavior — which is exactly
the part we are **not** rebuilding (we keep yt-dlp). The metadata client in
Part 4 needs none of the jsinterp / PO-token / format machinery.

## Part 3 — anti-bot measures & alternative clients

### 3.1 PO tokens (proof-of-origin)

- A PO token is a ~110–128-byte base64url blob proving the request passed
  Google's **BotGuard** attestation. Enforced for web clients (WEB/MWEB) since
  **Aug 2024**; the iOS/Android app clients gate their audio-only formats behind
  it too (iOS 139 AAC, 249/599/600 Opus — gated ~Jan 2025).
- Two bindings: `serviceIntegrityDimensions.poToken` in the `/player` body
  (content-bound) and a `pot=` query param on the resolved stream URL
  (session-bound).
- **Why ~1 MB:** the player response's StreamProtectionStatus (`sps`) — status 2
  allows ~1–2 MB before interruption; status 3 blocks. Un-tokened clients hit
  this.
- **Generating one** requires running YouTube's proprietary BotGuard JS VM +
  WAA attestation + a WebPO minter (BgUtils is the reference impl). Even yt-dlp
  doesn't do this in-tree — it uses pluggable providers
  (`bgutil-ytdlp-pot-provider`); rustypipe shells out to `rustypipe-botguard`.

### 3.2 Signature / n-parameter deobfuscation

- Resolved URLs still need `sig` and `n` solved against the player's `base.js`.
- **Sep 2025**: YouTube re-obfuscated the player enough to break in-tree JS
  interpreters; **yt-dlp 2025.11.12 now requires an external JS runtime** for
  full support (Node fallback / external n-sig solver).
- This is the same churn that broke rustypipe's stream path.

### 3.3 How yt-dlp survives

- Defaults to innertube clients that don't need a PO token for basic streams,
  solves sig/n with an external JS runtime, and pulls PO tokens from providers
  only when needed. Constant client reshuffles: `tv` replaced `mweb` as default
  fallback (2024), iOS formats dropped (Jan 2025), `tv_downgraded`/`web_embedded`/
  `visionos` added and `android_vr` dropped (2026).

### 3.4 Maintained Rust alternatives to rustypipe

- **`innertube-rs`** — MIT, "fast lightweight Rust port of YouTube.js", embedded
  QuickJS decipher for sig/n, stream URL resolution, YouTube Music endpoints;
  active Aug–Sep 2026 (0.5.0→0.9.0), ~39K SLoC, **single maintainer**.
- **`ytmusic-api`** (`ytmusic-tui`) — MIT, active Jun 2026; a browse/auth
  innertube client (search/library/playlist, SAPISIDHASH cookie auth) but **no
  stream resolution**.
- Others (`youtubei-rs`, etc.) are dormant.

### 3.5 Verdict

- **Metadata/browse client**: defensible to build or adopt (stable; no sig/n/PO).
- **Audio/stream client**: outsource. Owning it means maintaining the BotGuard VM
  + external JS runtime + sig/n + PO tokens, which churns multiple times/year.
  Keep yt-dlp as the audio backend.

## Part 4 — reimplementation design & plan

### 4.1 The core scope decision

- **Build or adopt a metadata/catalog client** covering the ~7 endpoints Rift
  uses. This removes the dormant GPL-3.0 `rustypipe` dependency and puts us in
  control of search/browse breakage.
- **Adopt before building**: `innertube-rs` (MIT, active 2026) already does
  Music endpoints + stream resolution with an embedded QuickJS decipher, and
  `ytmusic-api` (MIT) does Music browse/auth. Evaluate `innertube-rs` first — if
  it's viable it saves the whole build. If it's too new/risky (single
  maintainer), fall back to a small in-house client, or adopt `ytmusic-api` for
  browse and keep yt-dlp for audio.
- **Do not build** stream resolution (signature deobfuscation + PO tokens).
  Keep yt-dlp as the audio backend. This is the churn-prone part that even
  rustypipe delegates (to `rustypipe-botguard`) and yt-dlp maintains full-time.
- **Clean-room** (if we build): implement from the observable API contract in
  Part 1, not rustypipe source, to keep Rift Apache-2.0.

### 4.2 New crate

A new workspace member (e.g. `crates/youtube`), Apache-2.0, with:

- `client` — reqwest client (reuse Rift's existing client config), base-URL and
  client-name table from §1.1–1.2.
- `context` — builds the `context` object; **scrapes + caches the client
  version** (the `INNERTUBE_CONTEXT_CLIENT_VERSION` regex), matching §1.3.
- `identity` — anonymous visitor-data pool + rotate-on-failure (§1.4). Skip
  SAPISIDHASH/OAuth for v1 (Rift is anonymous).
- `endpoints` — typed builders for `search` / `browse` / `next` from §1.6–1.7
  (metadata only; no `player`).
- `params` — a minimal protobuf encoder for the search-filter `params`, or
  hardcode the three filter blobs (songs/artists/albums).
- `parse` — serde models with tolerant, partial-failure parsing (§1.8),
  mapping straight into Rift's existing `rift_types` (`Track`, `AlbumSummary`,
  `ArtistSummary`, `ArtistPage`, `AlbumPage`, `Playlist`) so the UI is unchanged.

### 4.3 Endpoint surface (must-have vs later)

- Must-have: `search` (tracks/artists/albums), `browse` artist + album +
  playlist (+ continuation), `next` music_details (for credit enrichment).
- Later/optional: `music/get_search_suggestions`, radio (`next` with `QRadio`)
  — currently unreachable in Rift.

### 4.4 Phased plan

1. **Phase 1 — spike**: implement `search` (tracks) end-to-end in a standalone
   example (like Rift's existing `probe.rs`) to de-risk the client-version
   scrape + context + parse round-trip.
2. **Phase 2 — full surface**: artists/albums search, artist page, album page,
   playlist + pagination, music_details.
3. **Phase 3 — integrate**: swap `PlayerShared.rp` for the new client in Rift;
   keep `fetch.rs`'s yt-dlp audio path and drop the rustypipe stream attempt.
4. **Phase 4 — cleanup**: remove `rustypipe`; add recorded-fixture tests (rustypipe
   uses insta snapshots; we can record canned innertube responses similarly).

### 4.5 Risks

- Client-version scraping is the main metadata breakage surface; mitigated by
  scraping at runtime and caching (self-healing across YouTube's version rolls).
- innertube response schema drift → tolerant parsing absorbs new/renamed fields.
- Search-filter protobuf blobs may change; keep them isolated and easy to update.

### 4.6 Integration surface (what Rift swaps)

Rift's rustypipe usage is small and well-isolated. The replacement client must
expose these operations, returning Rift's own `rift_types` directly:

| rustypipe call (Rift site) | replacement API |
|---|---|
| `RustyPipe::builder().storage_dir(dir).build()` (`main.rs`) | `Client::new(data_dir)` |
| `music_search_tracks(q)` (`commands/search.rs`) | `search_tracks(q) -> Vec<Track>` |
| `music_search_artists(q)` | `search_artists(q) -> Vec<ArtistSummary>` |
| `music_search_albums(q)` | `search_albums(q) -> Vec<AlbumSummary>` |
| `music_artist(id, false)` | `artist(id) -> ArtistPage` |
| `music_album(id)` | `album(id) -> AlbumPage` |
| `music_playlist(id)` + `extend_limit(q, 1000)` | `playlist(id, limit) -> Playlist` |
| `music_details(id)` (`player/playback.rs` enrich) | `track_details(id) -> (artists, album)` |
| `music_radio_track(id)` (currently unreachable) | drop, or `radio(id)` |
| `player_from_clients(id, &[Ios])` (`fetch.rs`) | **drop** — yt-dlp covers audio |

Integration is mechanical: replace the `PlayerShared.rp: RustyPipe` field with
the new client, and rewrite the ~6 command functions in `commands/search.rs` and
`commands/library.rs` to call it. `commands/convert.rs` disappears (the client
returns `rift_types`), and `fetch.rs`'s rustypipe streaming attempt is deleted,
leaving the yt-dlp path as the sole audio fetcher. The `get_artist_songs` fan-out
(album + details per track) and the artist-page duration backfill are currently
Rift-side logic in `search.rs`; those can stay in Rift or move into the client —
whichever keeps the client's surface small.

---

## Part 5 — alternatives deep-dive: `innertube-rs` & `ytmusic-api`

Two opposite philosophies. Both MIT, both single-maintainer, both 2026-new.

### 5.1 `innertube-rs` — the full YouTube.js port

- **Scope**: a complete, idiomatic async client: search, browse, playlist,
  channel, video info, comments, transcripts, feeds, OAuth, account, kids,
  studio, live chat, and mutation "actions" (like/subscribe/playlist CRUD).
  Music: `search_music`, `get_music_artist`, `get_music_album`, `get_music_home`,
  `get_music_explore`, `get_music_lyrics`.
- **Deps** (`Cargo.toml`): reqwest 0.12 (rustls, http2, cookies, gzip, brotli),
  **`rquickjs` 0.9** (embedded QuickJS — the sig/n decipher engine), **`prost` +
  `prost-build` + `protoc-bin-vendored`** (real protobuf codegen, unlike
  rustypipe's hand-rolled encoder), sha1, base64, regex.
- **Streaming**: `Innertube::new()` bootstraps a session (scrapes `sw.js` data)
  and downloads the player script to init the QuickJS decipher engine.
  `get_stream_url` / `get_streaming_data` / `download` solve **sig + n natively**
  by loading the full player `base.js` into embedded QuickJS (the "unified nsig"
  one-pass approach — the thing rustypipe could not do). **But it does NOT
  generate PO tokens**: `po_token` is caller-supplied everywhere; `attestation.rs`
  only does the `/att/get` challenge, and the `test_native_botguard*.rs` examples
  are standalone diagnostics (a partial `Waa/Create`→`GenerateIT` PoC) not wired
  into Session/Player. So it still needs an external PO-token source.
- **The parser is the cost**: it ports YouTube.js's renderer model into
  *hundreds* of typed node structs (`MusicShelfNode`, `MusicResponsiveListItemNode`,
  `MusicTwoRowItemNode`, …). Rift needs ~5 of them; adopting the crate inherits
  all of them and their drift.
- **Risks**: 5 releases in ~2 weeks (0.5.0→0.9.0), 181 downloads, ~39K Rust LoC,
  single maintainer. Red flags: `CHANGELOG.md` is a **verbatim copy of YouTube.js's
  changelog** (its own history is undocumented), stale docs (reference non-existent
  methods/files), a leftover unrelated `noctune-README.md`, and a hand-maintained
  ~19K-line renderer parser (`if val.get("xxxRenderer").is_some()` chain) with high
  drift risk. Clean API, but not something to depend on yet.

### 5.2 `ytmusic-api` — the minimal auth+transport core

- **Scope**: a faithful port of Python `ytmusicapi`'s protocol layer. Browser-
  cookie auth (SAPISIDHASH) + signed innertube transport + typed metadata
  endpoints. **Non-goals**: no stream resolution, no track-details enrichment, no
  search pagination, no visitor-data generation, no OAuth.
- **Deps**: reqwest 0.13 (rustls, json), sha1 0.11, serde, thiserror, tokio —
  **no JS engine, no protobuf**. (But reqwest 0.13 + rustls pulls a heavy TLS
  tree: aws-lc-rs → cc/cmake + ring + quinn.)
- **Auth** (`auth.rs`): `SAPISIDHASH {ts}_{sha1hex("{ts} {sapisid} {origin}")}`,
  recomputed per request; extracts `__Secure-3PAPISID` (falls back to `SAPISID`);
  origin `https://music.youtube.com`. Has a **session "canary"** — detects
  logged-out-but-HTTP-200 by requiring `activeAccountHeaderRenderer` in
  `account/account_menu`.
- **Transport** (`client.rs`): `POST music.youtube.com/youtubei/v1/{endpoint}?alt=json&key=AIzaSyC9XL3ZjWddXya6X74dJoCTL-WEYFDNX30`;
  body = extras + `context:{client:{clientName:WEB_REMIX, clientVersion:"1.YYYYMMDD.01.00", hl:en}}`;
  replays an allow-listed header subset + recomputed Authorization + `Cookie; SOCS=CAI`;
  15s/5s timeouts; a public `post()` escape hatch.
- **Endpoints**: search (single page, hardcoded protobuf filter params), album
  (`MPREb_…`), artist (channel id → top songs/albums/related), playlist (`VL…`,
  full continuation paging, 50-page cap), radio (`next`), home (`FEmusic_home`),
  library/history/lyrics/mutations.
- **Parsing**: lightweight two-stage (`nav.rs` path-walking over `serde_json::Value`
  → `parse.rs` dict→model), handles shelf/carousel/grid. Models are
  **stringly-typed and lossy** (artist IDs dropped, duration is `f64`).
- **Risks**: single maintainer, part of the `ytmusic-tui` monorepo, edition 2024
  (MSRV 1.85+), reqwest 0.13, untyped parsing fails silently, browser-cookie-only.

### 5.3 What we learn from them

1. **ytmusic-api validates the small-client design** — `auth` / `context` /
   `client` / `endpoints` / `parse` / `models` is exactly §4.2. It's the cleanest
   MIT blueprint for the metadata half, including the exact SAPISIDHASH, the
   `WEB_REMIX` context, header pruning, and the canary pattern.
2. **But it's not a drop-in**: browser-cookie auth doesn't match Rift's anonymous
   visitor-data model; its lossy models drop artist/album IDs Rift needs; its
   TLS tree (reqwest 0.13 + aws-lc-rs + cc/cmake) is heavier than Rift's existing
   reqwest 0.12 stack. → **Learn from it / port its small primitives; don't adopt
   wholesale.**
3. **innertube-rs solves sig/n natively but NOT PO tokens** — its embedded
   QuickJS decipher (`utils/decipher.rs`) is the valuable hard part worth studying
   (what rustypipe couldn't do), but `po_token` is still caller-supplied, so it
   does **not** replace yt-dlp's audio path (you'd still need an external PO-token
   source). At 3 weeks old, ~39K LoC, single-maintainer, with a borrowed changelog
   and a 19K-line hand parser, it's a **watch-and-revisit**, not a dependency. Its
   parser is also the clearest illustration of why a *full* renderer parser is
   overkill for Rift's 6 endpoints.
4. **Net**: the sharpened plan is — build a small anonymous metadata client
   in-house (ytmusic-api as blueprint + rustypipe's visitor-data approach),
   keep yt-dlp for audio (no Rust client, including innertube-rs, generates PO
   tokens natively today), and study innertube-rs's `utils/decipher.rs` if we
   ever revisit native sig/n solving.
