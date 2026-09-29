# Rift — full code review

Scope: every tracked source file (`src-tauri/`, `crates/ytmusic/`, `types/`, `ui/`,
`docs/`, `.github/`, manifests). HEAD `a4b8d30` plus the uncommitted player diff.

Verification performed on this checkout:

| Check | Command | Result |
|---|---|---|
| Formatting | `cargo fmt --all -- --check` | clean |
| Backend lints | `cargo clippy --workspace --exclude rift-ui --all-targets --locked -- -D warnings` | clean, 0 warnings |
| Frontend lints | `cargo clippy -p rift-ui --target wasm32-unknown-unknown --locked -- -D warnings` | clean |
| Tests | `cargo test --workspace --exclude rift-ui --locked` | 65 passed, 0 failed |
| Path semantics | isolated `rustc` program printing `with_extension` results | confirmed L2 |

Overall this is an unusually well-documented, well-tested codebase for its size.
Comments explain *why*; the panic surface in the backend is tiny (4 non-test
`unwrap`/`expect`, all on thread spawn or an already-proven `OnceLock`); the
shuffle/crossfade state machine has genuinely good tests; and the Tauri
capability set is admirably narrow. The findings below are ordered by severity.

---

## Critical

### C1. `use_state` after an early return — the queue panel violates hook order
`ui/src/components/queue.rs:21-30`

```rust
let collapsed = use_state(|| false);                    // :21
let reorder = use_reorder(...);                         // :23 — 7 more hooks
if props.queue.tracks.is_empty() {
    return html! {};                                    // :28-30
}
let toggle = { ... Callback::from(move |_: MouseEvent| collapsed.set(!*collapsed)) };  // :31-34
```

The first render has an empty queue (`app.rs:107` seeds
`QueueSnapshot::default()`, whose `tracks` is empty), so it runs 8 hooks and
returns at `:29`. As soon as bootstrap lands (`app.rs:251`) or the user queues
one track, `toggle` at `:31` needs `use_state`'s slot — but the hook was *not*
called on the previous render. Yew's `use_state` asserts its counter against
`total_hook_counter` (`yew-0.21.0/src/functional/mod.rs`, "Hooks are called
conditionally"), so **debug builds panic during mount the moment the queue
becomes non-empty**; with `debug_assertions` off the slot misbinds and the
collapse state lives in the wrong hook slot. Nothing in CI would catch this — no
test drives the Yew renderer.

Fix: build the callback above the early return (or drop the early return and use
`if props.queue.tracks.is_empty() { html! {} } else { ... }` in the markup):

```rust
let collapsed = use_state(|| false);
let toggle = { let c = collapsed.clone(); Callback::from(move |_| c.set(!*c)) };
let reorder = use_reorder(...);
if props.queue.tracks.is_empty() { return html! {}; }
```

---

## High

### H1. A banned visitor id is immediately reused — one 403 permanently 403s the session
`crates/ytmusic/src/identity.rs:25-35`, with `client.rs:186-189`

```rust
if let Some(id) = p.ids.iter().find(|id| !p.banned.contains(*id)) { return Ok(id.clone()); }
...
pool().lock().unwrap().ids.push(id.clone());   // fresh id queued
```

`client::post`'s 403 handler calls `identity::ban(&vdata)` and loops. The retry
calls `visitor_data()`, which finds the just-banned id is the only entry in
`ids`, returns it, and fails identically. Nothing ever evicts from `ids`. So the
first 403 from YouTube breaks **every** metadata feature (search, artist, album,
playlist import, credits) for the life of the process; only a restart recovers.
The control flow also makes `banned` nearly pointless — the `continue` can never
yield a different id. Fix: drop the id on ban (`p.ids.retain(|i| i != id)`).

### H2. A stale client version is cached for the whole process
`crates/ytmusic/src/context.rs:15, 44-49`

```rust
const FALLBACK_VERSION: &str = "1.20240101.01.00";
let version = scrape(http, user_agent).await.unwrap_or_else(|| FALLBACK_VERSION.to_owned());
let _ = CLIENT_VERSION.set(version.clone());   // the *fallback* is cached
```

One transient failure of the first home-page scrape (offline launch, 429, DNS
blip) pins a 2024 `clientVersion` into a `OnceLock` that is never invalidated.
Innertube rejects stale versions with 400, and `post` retries only 403 — so
every request fails with `innertube returned status 400` until restart. Fix: only
write the cache on a successful scrape; keep the fallback per-call.

### H3. No HTTP timeouts anywhere — a half-open connection wedges the app
`src-tauri/src/main.rs:111`, `crates/ytmusic/src/client.rs:35-39`

Both `reqwest::Client`s use defaults, and reqwest's default is **no timeout**.
The only timeout in the codebase is the deliberate 2 s one in `util::is_online`.
Consequences, easiest-to-hit first:
- `check_ytdlp` (`commands/ytdlp.rs:14`) never resolves → Settings shows
  "checking" forever.
- `check_update` (`commands/updates.rs:47`) likewise.
- Every metadata command hangs; the UI's `busy: true` spinner has no cancel.
- The download worker (`commands/downloads.rs:147`) blocks mid-batch and the
  `Downloads::begin` claim is never released, so that track can never be retried
  for the rest of the session.

Fix: `.connect_timeout(5s).timeout(30s)` on both builders, plus a
`tokio::time::timeout` around the yt-dlp subprocess wait (`fetch.rs:96`).

### H4. PlayerBar's live progress survives a track change
`ui/src/components/player_bar.rs:36, 43-46, 107, 115-119`

```rust
let live = use_state(|| props.progress);                              // :36 seeded once
use_effect_with(props.progress, move |p| { live.set(*p); || () });    // :43 only prop-driven
let duration = live.duration.max(1.0);                                // :107
```

`props.progress` is the bootstrap seed and never changes afterwards (`App`
deliberately does not subscribe to `PROGRESS` — `app.rs:207-210`), and `live` is
never reset when `props.track` changes. The backend only emits a corrective
`PROGRESS {0.0, duration}` after the stream resolves (`playback.rs:102-108`),
i.e. after a network fetch. So clicking a 3-minute track then a 10-second
interlude leaves the bar showing the *previous* track's elapsed time and the
slider's `max` set to the previous duration for the whole fetch window. Fix: key
the effect on track identity as well as the seed, falling back to
`track.duration` while loading.

### H5. Menus are unreachable by keyboard and cannot be dismissed with Escape
`ui/src/components/menu.rs:53-66, 75`

No `onkeydown`, no `role="menu"`/`menuitem`, no `tabindex`, no focus trap, no
focus restore. The only exit is a pointer click on the backdrop (`menu.rs:61-64`,
`position: fixed; inset: 0; z-index: 80`). Items are `div`s, so they are not
focusable at all — a keyboard-only user cannot operate any kebab or context menu.
Compounding this, `styles.css:97, 179, 353, 988` set `outline: none` on inputs and
there is no `:focus-visible` rule anywhere, so even the real `<button>`s give no
focus indication. Fix: `onkeydown` for Escape, `role`/`tabindex` on items, focus
the first item on open, and replace blanket `outline: none` with a
`:focus-visible` style.

---

## Medium

### M1. Crossfade decode failure can leave the UI showing a track that isn't playing
`src-tauri/src/player/playback.rs:85`, `audio.rs:108-130`

`start_playback` sets `core.state = Playing` (line 85) before the audio thread
has decoded anything, and `AudioCmd::Play` calls `player.clear()` *before*
`decode(data)`. On a decode failure the outgoing audio is already silenced while
the UI reports Playing with an advancing bar; the state only corrects when the
~250 ms tick sees `player.empty()` and emits `Ended`. Fix: decode first, then
`clear()` + `append` only on success. (The `Crossfade` arm already gets this
right — its comment is accurate.)

### M2. `is_online` returns *true* when it cannot reach the network
`src-tauri/src/util.rs:101-110`

```rust
http.head("https://www.youtube.com/").timeout(2s).send().await
    .map(|r| r.status().is_success()).unwrap_or(false)
```

A captive portal or transparent proxy answers the HEAD with a 200/302/403, so the
function meant to *skip* pointless retries when offline reports "online" and the
kept-offline download loop burns its full ~7.5-minute backoff ladder. Fix: treat
a non-success status as offline too, and check the response actually came from
YouTube.

### M3. In-flight download claims are never released on the retry path
`src-tauri/src/commands/downloads.rs:264-272`

```rust
let batch = failed.clone();
let retry: Vec<Track> = failed.into_iter().filter(|t| kept_offline(&app, &t.id, &batch)).collect();
if retry.is_empty() { return; }
```

`downloads.fail(id)` released the claim earlier, but any track dropped by this
filter is gone from the retry set while its row may still show a spurious
"downloading" state, and `mark_failed` is never called for it — so the user gets
no retry affordance. Also, if the playlist is edited during the backoff the track
is silently abandoned. Fix: `mark_failed` the dropped ids and `emit_downloads`.

### M4. `api::fire` makes 41 of 55 IPC calls fire-and-forget
`ui/src/api.rs:33-39`, plus 41 call sites

`fire` logs to `web_sys::console` and returns, so `Err` values not mirrored by a
backend `rift://error` event reach nobody: `play_tracks`' `"nothing to play"`
(`commands/playback.rs:27`), `create_playlist`'s empty-name error
(`commands/library.rs:33`), and every rejection in `queue_move`,
`move_playlist`, `add_to_playlist`, `toggle_like`, `remove_downloads`. Commands
that *do* emit `events::ERROR` toast correctly, which makes the inconsistency the
real problem. Fix: give `fire` an error path into the existing toast.

### M5. Search failures are indistinguishable from empty results
`ui/src/app.rs:473-480`, `ui/src/components/titlebar.rs:60`

```rust
Err(e) => { web_sys::console::error_1(...); search.set(Search { query, results: vec![], busy: false }); }
```

Offline or rate-limited, the user sees "No results." — identical to a genuinely
empty result set, with no toast, even though the neighbouring navigation paths do
call `show_toast` (`app.rs:383`). Separately `titlebar.rs:60` returns early on an
empty query, so clearing the box leaves the old results rendered under
`Results for "<old query>"` (`app.rs:979`).

### M6. `withGlobalTauri` + app commands outside the ACL is a wide surface
`src-tauri/tauri.conf.json:19`, `src-tauri/build.rs:1-3`, `commands/ytdlp.rs:21-32`

`withGlobalTauri: true` is required by `ui/src/api.rs:12`, so it is intentional.
It does mean the whole IPC surface sits on `window`, and the app's own 46
commands are not named in `capabilities/default.json` (Tauri v2 enforces the ACL
on plugin commands and on apps that declare their own manifest; app-defined
commands do not appear in the generated manifest). The sharpest edge is
`set_yt_dlp_path`: any webview script can point Rift at an arbitrary local
binary, which `fetch.rs:228-243` accepts on `p.is_file()` alone and
`fetch.rs:80,136` then spawns, persisting the path into `settings.json`.

The CSP (`default-src 'self'`, no `unsafe-eval`) and the fact that the webview
loads only bundled assets mean there is no XSS path today — this is
defense-in-depth, not a live exploit. Worth doing anyway: declare an app manifest
(`tauri_build::Attributes::new().app_manifest(...)`) so the commands are
explicitly granted, and validate the yt-dlp override (canonicalize; require a
file chosen through the existing native dialog).

### M7. The one-click yt-dlp installer executes an unverified download
`src-tauri/src/fetch.rs:174-212`, `commands/ytdlp.rs:37-60`

`github.com/yt-dlp/yt-dlp/releases/latest/download/<asset>` is fetched, written,
`chmod 0755`'d and later spawned, with no checksum or signature check and no
pinned version. There *is* a genuine trust dependency here (the app already
executes yt-dlp by design), but "latest" plus no verification means a
compromised release asset or TLS path becomes code execution, and it is not
reproducible. Fix: fetch `SHA2-256SUMS` from the same release and verify before
writing/chmod.

### M8. Persistent player state is written on the event loop
`src-tauri/src/player/playback.rs:195`, `state.rs:101-117`

`emit_queue` clones every `Track`, `serde_json::to_vec`s the snapshot and queues
a disk write — once per track change, once per successful credit enrichment (so
twice on a track's first play), and on every queue mutation. With
`MAX_FETCH_TRACKS = 1000` that is a 1000-element clone plus a write burst from
inside a `tauri::async_runtime::spawn` task sharing the runtime with IPC. Fix:
debounce, or skip the write when the snapshot is unchanged.

### M9. `atomic_write`'s temp name is correct only by accident, and the whole write path is untested
`src-tauri/src/util.rs:92-96`

`path.with_extension("tmp")` is purely lexical, so `settings.json` →
`settings.tmp` preserves the directory (verified with an isolated `rustc`
program). It is a sharp edge for future callers, not a live bug. The substantive
finding is that **`util.rs` has zero tests**: `load_json`, `save_json`,
`atomic_write` and `Persister` guard the user's library, and one round-trip test
would pin the behaviour. Note also that `Persister::write` silently drops the
final snapshot if the channel is closed, and the detached writer means the last
snapshot is lost on quit — acceptable per the module doc, but it should be stated
as a known loss.

### M10. Shuffle history is discarded on every queue edit
`src-tauri/src/player/queue.rs:27-32, 106-108, 135-137`

The in-code rationale (indices shift) is sound, but the user-visible effect is
that removing one track throws away the "already played this cycle" set, so
tracks heard minutes ago become immediately eligible again. Remapping old indices
through the edit would preserve the cycle.

### M11. `parse::tracks` uses the whole subtitle as the artist
`crates/ytmusic/src/parse.rs:223-228`

`runs_all` concatenates every run of the subtitle column, which for an unlinked
row is `["Song", " • ", "3:22"]` → `"Song • 3:22"`. Verified against
`tests/fixtures/search_tracks.json`, and currently *enshrined* by
`tests/fixtures.rs:94-102`. Fix: drop the leading type badge and trailing
duration run before falling back.

### M12. `find_all(..).first()` is alphabetical, not document order
`crates/ytmusic/src/parse.rs:11-28`, call sites `:395, 455, 472-478, 590-595, 611`

`serde_json 1.0.151` is built without `preserve_order` (the lockfile shows no
`indexmap`), so `Value::Object` is a `BTreeMap` and `map.values()` iterates keys
alphabetically. Every "first match wins" site therefore depends on key spelling,
not on the response layout: `artist_page`'s subscriber lookup visits `contents`
before `header`, `playlist_page` takes whichever continuation node sorts first,
and `track_credits` takes the first `longBylineText` in the document **without
checking it belongs to the requested `videoId`** — so a substituted or
unavailable video silently yields another song's artists and album. Fix: read the
known paths first and keep `find_all` as a fallback; match the video id in
`track_credits`.

### M13. `parse_duration` drops bad segments and can overflow
`crates/ytmusic/src/parse.rs:174-179`

`filter_map(|p| p.parse().ok())` turns `"1:2x:03"` into `[1, 3]` → 63 s, and the
`m * 60 + s` / `h * 3600 + m * 60 + s` arithmetic is unchecked `u32`: a crafted
value panics in debug and wraps to a bogus duration in release (the workspace
sets no `overflow-checks`). Fix: parse every component with `?` and use
`checked_mul`/`checked_add`.

### M14. `?` inside a reverse loop aborts the scan
`crates/ytmusic/src/parse.rs:319` (`audience`), `:362` (`trailing_year`)

`let text = run.get("text").and_then(Value::as_str)?;` inside
`for run in runs.iter().rev()` returns `None` from the whole function on the
first run lacking a string `text`, instead of continuing like `duration`
(`:279`) does — silently dropping a present year or subscriber count.

### M15. Packaged builds are never compiled or bundled in CI
`.github/workflows/ci.yml`, `release.yml:84-100`

CI runs `trunk build`, two clippy passes and the backend tests. Clippy links the
crate, so a broken `tauri.conf.json` **is** caught via `generate_context!`, but
the bundler stage (`"targets": "all"`, icon set, `mainBinaryName` rename,
NSIS/WiX prerequisites) and the release-only `[profile.release] lto/codegen-units`
are exercised only on a tag push, which publishes a *draft*. A malformed icon path
surfaces at release time, on the wrong commit. Add a
`cargo tauri build --no-bundle` job. Two related release gaps: nothing asserts the
tag matches `Cargo.toml`/`tauri.conf.json` (so `v1.0.0` over 0.5.0 manifests
publishes mislabelled installers *and* makes `check_update` report a phantom
update forever), and the updater queries `releases/latest` while the workflow only
ever creates **drafts** — which that endpoint excludes, so the update banner can
never fire.

### M16. Duplicate majors in one binary
`Cargo.lock` vs `src-tauri/Cargo.toml:23,26,29`, `crates/ytmusic/Cargo.toml:12`

`reqwest 0.12.28` (direct) **and** `0.13.4` (via tauri 2.11.5); `rand 0.8.8`
(via souvlaki → zbus 3) **and** `0.10.2` (direct); `zbus 3.15.2` + `5.19.0`;
`thiserror 1` + `2`; both `native-tls`/`openssl 0.10.81` and `rustls 0.23.43`.
Two TLS stacks and two rand generations ship in the same binary. Bump the direct
`reqwest` to 0.13 to dedupe, and add `cargo tree -d` as a CI guard.

---

## Low / Nits

- **L1. `AudioCmd::Volume` ignores in-flight fade-outs** (`audio.rs:183-186`): the
  `fading` vec keeps ramping from the old `volume` captured at `:199`, so dragging
  the slider during a crossfade produces an audible level mismatch that resolves
  only when the fade ends.
- **L2. `with_extension` trap** (see M9) — documented here for the record since I
  verified it empirically; no live caller is affected.
- **L3. Offline playback returns `duration: 0.0`** (`fetch.rs:51-57`), which
  `start_playback` uses to overwrite `core.duration` with the metadata guess
  (`playback.rs:57`). The decoder corrects it a moment later, but the seek bar
  jumps.
- **L4. `epoch` is dead code with a false comment** (`player/state.rs:36-37`): it is
  incremented in three places (`commands/playback.rs:35,62`,
  `commands/queue.rs:92`) and read nowhere. Its doc claims "stale radio fills
  check it" — the radio feature was removed, as `commands/playback.rs:41-42`
  itself says, and `play_track`'s `radio: bool` is accepted then discarded
  (`let _ = radio;`). Delete both.
- **L5. `queue_add_tracks` is a registered command with no caller** (`main.rs:150`);
  the UI appends one track at a time.
- **L6. Silent parse degradation.** A renderer rename (e.g. YouTube moving off the
  legacy `nextContinuationData` that `playlist_page:589` reads — the fixture still
  uses it, so only a *new* capture would catch the migration) yields zero results
  with no error. Consider warning when a response contains
  `musicResponsiveListItemRenderer` nodes but nothing is extracted.
- **L7. Regex compiled per request** (`identity.rs:73`, `context.rs:68`) inside the
  request path; cache in a `OnceLock`.
- **L8. `album_type` is an exact match on a server-controlled title**
  (`parse.rs:526-529`): `"Singles" => "Single", _ => "Album"`. I checked
  `tests/fixtures/artist.json` and it contains **no** Singles shelf at all (only
  "Albums", "Featured on", "Fans might also like"), so nothing pins this mapping
  and any rename silently degrades every card to "Album". Use `starts_with`.
- **L9. `tracks_playlist_id` takes the first `VL…` id under *any* `bottomEndpoint`**
  (`parse.rs:494-508`), not the top-songs shelf's; the fixture's Videos carousel
  already carries one, so a layout change makes "Show all songs" load the wrong
  list with no error.
- **L10. `is_audio_track` is a title substring filter** (`commands/convert.rs:19-35`).
  Fine as a heuristic, but the typed signal (`MUSIC_VIDEO_TYPE_UGC`, already parsed
  at `parse.rs:209`) is strictly more reliable.
- **L11. `parse_count` truncates** (`parse.rs:169`) — `.round()` would be correct.
- **L12. `identity.rs` uses `lock().unwrap()`** (`:27,33,39`) where the rest of the
  codebase uses `lock_safe`; a panic in the critical section poisons the pool and
  turns every later call into a panic.
- **L13. `match_all.first()` risk in `identity::visitor_data`** is covered by H1; the
  regex at `:73` also excludes `=`, so a padded base64 `visitorData` could never be
  matched, and the bootstrap GET lacks `error_for_status()` (contrast
  `context.rs:59`).
- **L14. Release actions float on mutable tags** (`tauri-apps/tauri-action@v0`,
  `dtolnay/rust-toolchain@stable`, `actions/checkout@v5`, `Swatinem/rust-cache@v2`)
  while the release job holds `contents: write` and builds the artifacts users
  install. SHA-pin them, and pin an explicit toolchain version (there is no
  `rust-toolchain.toml`, so CI and release can silently move).
- **L15. `src-tauri/gen/schemas/*.json` (~440 KB) is tracked** and not ignored;
  upstream's Tauri template ignores `/gen/schemas`. It churns on every dependency
  bump and can drift from the real plugin set. `git rm --cached -r src-tauri/gen`.
- **L16. Missing package metadata**: no workspace `authors`/`repository`/`rust-version`,
  and no `bundle.publisher`/`copyright`/`licenseFile`, so `.deb` Maintainer and MSI
  Manufacturer fall back to defaults.
- **L17. No malformed-input tests.** `tests/fixtures.rs` covers only happy paths;
  nothing feeds missing `flexColumns`, `null` titles, wrong JSON types, empty
  arrays, a "no results" response or an error/consent body, and `client.rs`
  (transport, the 403 retry, playlist pagination) is entirely untested. Note
  `examples/prune_fixture.rs:415-436` proves the compacted document parses
  identically *to itself*, so the fixtures lock in current parser output rather
  than validating it — M11 is a case where a test codifies a defect.
- **L18. Test litter**: `settings.rs` / `downloads.rs` / `library.rs` tests create
  `%TEMP%\rift-*` dirs and skip cleanup on failure.

---

## Checked and found correct

Recorded because these are the places a reviewer expects trouble:

- **Shuffle index arithmetic** (`nav.rs`, `queue.rs`). I traced every path that
  mutates `shuffle_history`/`shuffle_cursor` and could not produce an
  out-of-bounds access: `pick_prev` guards `cursor > 0` before decrementing,
  `pick_next` guards `cursor + 1 < len`, and the three queue mutators reset the
  pair together. The uncommitted diff's `seed_shuffle_anchor` is a real
  improvement over `history = vec![index]`, and its new tests assert observable
  ordering rather than internals.
- **`pick_next` reads `pending_next` before the repeat-one check**, so a prefetched
  crossfade target wins; that ordering is exactly why `cycle_repeat` must
  invalidate the pick — and the diff adds that invalidation plus a test.
- **Progress ticks do not re-render the app.** `PROGRESS` is deliberately not
  subscribed in `App` (`app.rs:207-210`); only `PlayerBar` subscribes
  (`player_bar.rs:51-54`). This was the biggest performance risk in the brief and
  the code handles it correctly.
- **Stale-response guards are correct.** `search_gen` (`app.rs:153`, checked
  `:464`) and `page_gen` (`:157`, checked `:378,403,420`) bump-and-compare around
  every async invoke, and the toast-generation counter prevents an older timeout
  from clearing a newer toast.
- **Event listeners are registered once** — every `listen_event` sits in a
  `use_effect_with((), …)` (`app.rs:178`, `player_bar.rs:51`, `settings.rs:87,162`,
  `sidebar.rs:82`), and the deliberate closure leak (`api.rs:54`) means no
  use-after-free.
- **IPC contract: PASS.** All 47 commands in `main.rs:130-178` resolve to handlers,
  and all 55 frontend call sites use matching names *and* snake_case argument keys
  (spot-checked `seek`/`position`, `set_crossfade`/`secs`, `set_yt_dlp_path`/`path`,
  `remove_from_playlist`/`track_id`, `queue_move`/`from,to`,
  `import_yt_playlist`/`url`). No runtime "command not found"/"invalid args" from
  this code.
- **Lock discipline.** `bootstrap.rs:17` documents the non-reentrancy trap and
  avoids it; `add_to_playlist` releases the library lock before calling
  `start_downloads`, avoiding the deadlock the naive version would have.
  `identity.rs:26-34` correctly drops the guard before `.await`.
- **CSP and capabilities are genuinely tight.** `default-src 'self'`, no
  `unsafe-eval`, only `wasm-unsafe-eval`; `capabilities/default.json` grants only
  `core:default` + `core:window:allow-start-dragging`. No asset protocol, no
  updater endpoint, no `fs` plugin registered; devtools are not compiled in
  release. I verified the UI performs **no** direct network I/O (no
  `fetch`/XHR/WebSocket in `ui/src`), so `connect-src 'self' ipc: …` is sufficient
  and covers load via `img-src https:`. The Trunk inline loader is legal because
  tauri-codegen injects compile-time `sha256-…` hashes.
- **`open_url`** (`commands/updates.rs:103-132`) validates the scheme and passes
  the URL as an argument — no shell, no injection path.
- **File-path handling** for imports/exports uses only native-dialog paths, and
  video ids are sanitized before use in filenames (`fetch.rs:26-42`, tested).
- **`is_newer`** (`updates.rs:16-40`) handles uneven lengths, a leading `v` and
  pre-release suffixes without panicking, and is tested.
- **Fixture tests are honest about their intent** — real captured responses,
  compacted with a generator that verifies parser equivalence — even though their
  coverage is happy-path only (L17).
- **Repo hygiene**: `dist/`, `target/`, `*.log` are ignored and untracked; only 112
  tracked files; LICENSE is canonical Apache-2.0 matching all manifests; no
  copyleft dependency in the lock.
- **`docs/innertube-research.md` does not contradict the code** — its WEB_REMIX /
  `clientVersion`-scraping claims match `context.rs:8,24,35`, and its recommended
  `yt-dlp -f 140/bestaudio[ext=m4a] -o <tmp> <url>` matches `fetch.rs:81-89`.
- **No TODO/FIXME/HACK markers** in `ui/src` or `src-tauri/src`; comments are
  unusually accurate (the exceptions are called out above: `state.rs:36`, and
  `reorder.rs` setting state during render, which converges because the values are
  unchanged).
