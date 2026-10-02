# Rift — full code review

> **Status (2026-09-29):** every item below has been triaged and marked
> **fixed**, **retracted**, or **deferred** with a one-line reason, in
> `<blockquote>` notes under each heading. The work landed as fourteen commits,
> `7df0c81..baa5af8`. Test count went 65 → 117; the four required checks
> (`cargo fmt --all -- --check`, both clippy passes, and
> `cargo test --workspace --exclude rift-ui --locked`) are clean, and the whole
> block was re-run on both the declared MSRV (1.88.0) and a newer toolchain
> (1.98.0).
>
> Two claims are retracted, both because the defect does not exist in the code
> as written: **L1** (the crossfade fade-out already reads the live master
> volume) and part of **L13** (`match_all.first()` is unreachable — the regex
> always matches in practice, so `first()` cannot panic; the padded-base64
> limitation is real but harmless). The previously retracted `shuffle_cursor`
> panic and `fetch_track` dedup bug, and the `artist.json` "Singles & EPs"
> downgrade, stand as recorded at the bottom.
>
> **Three defects were introduced by these fixes and then caught**, which is its
> own finding about how the review verified things. M6's ACL work was "verified"
> by reading the generated artifacts rather than running the resolution, and
> shipped a capability that granted *nothing* — the app started with an empty
> library and the only clue was `bootstrap not allowed` in a console the review
> never looked at. The `play_track` call-site breakage was the same shape:
> certified once by hand, invalidated by a later local change. Both now have
> tests that run the real machinery (`acl_covers_commands.rs`,
> `ipc_contract.rs`). The third, in my own SHA-256, was caught by a test written
> for it.
>
> The L17 client-test harness added later (`d1cac0c`) supplied a fourth instance
> of the same pattern, and it is worth recording because the failure mode was
> new: the *harness* had two bugs of its own — an accept loop that a panicking
> route could kill, and a request log mutex held across handler calls — so one
> assertion failure surfaced as eight unrelated `ConnectionRefused`/`PoisonError`
> results. It read as "the client is broken on 1.98" when the client was fine.
> Both are fixed (`baa5af8`), and the suite was confirmed stable over 20
> consecutive runs rather than once.
>
> **Deferred, with reasons:** L7 (regex caching), L10 (typed UGC signal), M10
> (shuffle-history remapping), and the `with_extension` shape in M9. Each is
> noted at its item. L16's `rust-version` and L17's `client.rs` transport tests
> were deferred and have since been done.
>
> **Added after this review closed — the frontend harness.** `rift-ui` was the
> one crate with no tests, so every UI change was unverifiable except by running
> the app. There is now a `wasm-bindgen-test` harness (`ui/src/tests/`, 39 tests,
> wired into CI next to the frontend clippy pass) covering the IPC boundary's
> argument shapes, the reorder index math, and `App`'s pure decisions about
> shuffling, durations, and the offline pin. **The computed-style gap is still
> open and is now explicit rather than implied**: the white album text came from
> a `<button>` inheriting `buttonface`, which needs a real layout engine and
> `getComputedStyle`, and that is a different purchase from this harness — see
> "What the frontend harness does not cover" at the end of this document.

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

*(As of the fixes: the same four commands are clean and the test count is 117.
Three further checks were added to CI — `cargo tauri build --no-bundle`,
`.github/scripts/check-duplicate-deps.sh`, and
`.github/scripts/check-toolchain-consistency.sh`. The whole block is also clean
on a newer toolchain (1.98.0), which matters because pinning to the MSRV would
otherwise make upgrading a prerequisite for building.)*

Overall this is an unusually well-documented, well-tested codebase for its size.
Comments explain *why*; the panic surface in the backend is tiny (4 non-test
`unwrap`/`expect`, all on thread spawn or an already-proven `OnceLock`); the
shuffle/crossfade state machine has genuinely good tests; and the Tauri
capability set is admirably narrow. The findings below are ordered by severity.

---

## Critical

### C1. `use_state` after an early return — the queue panel violates hook order
`ui/src/components/queue.rs:21-30`

> **FIXED** (`7df0c81`). The callback is hoisted above the early return. Audited
> the other three `return html!` sites in `ui/src` (`app.rs:808`, `icons.rs:66`,
> `menu.rs:54`): all are inside plain closures or after every hook, so this was
> the only instance. No test — nothing drives the Yew renderer, and adding a
> wasm harness is its own CI-shaped change; the invariant is documented at the
> call site instead.

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

> **FIXED** (`5ab2a7d`). `ban` now evicts the id from `ids`, so the retry fetches
> a fresh one. Selection is split into `reusable_id` and tested; both tests fail
> if the eviction is removed (verified). L12's `lock().unwrap()` on the same pool
> is fixed with a poison-recovering helper.

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

> **FIXED** (`7df0c81`). Only a successful scrape is cached; the fallback stays per-call. The caching rule is extracted as a synchronous helper so both branches are tested without a network. Writing that test found that `OnceLock::get_or_init` alone memoizes its closure's `None` too � the first implementation still cached the fallback.
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

> **FIXED** (`7df0c81`). `.connect_timeout(5s).timeout(30s)` on both builders,
> plus a bounded yt-dlp subprocess wait with `kill_on_drop`. One deviation: the
> installer's asset download overrides the 30 s ceiling per-request (300 s),
> because that one is a multi-megabyte binary and applying a status-call ceiling
> to it would be a regression.

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

> **FIXED** (`5ab2a7d`). The seeding effect is keyed on track identity as well as the seed, and the single render before it re-runs falls back to the new track's metadata duration with a zeroed position. No test: `ui` has no harness at all.
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

> **FIXED** (`e48476d`). Escape/Up/Down/Home/End/Left/Right, `role="menu"`/`menuitem`, focus on open, and focus restore to the trigger. The `outline: none` declarations are replaced with a `:focus-visible` ring (plus `:focus-within` on the input wrapper).
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

> **FIXED** (`2cc59da`). The audio thread decodes before clearing, and `AudioCmd::Play` carries an ack so `Playing` is committed only after a successful decode. The audio thread stays silent when an ack is outstanding, so exactly one path advances the queue.
`src-tauri/src/player/playback.rs:85`, `audio.rs:108-130`

`start_playback` sets `core.state = Playing` (line 85) before the audio thread
has decoded anything, and `AudioCmd::Play` calls `player.clear()` *before*
`decode(data)`. On a decode failure the outgoing audio is already silenced while
the UI reports Playing with an advancing bar; the state only corrects when the
~250 ms tick sees `player.empty()` and emits `Ended`. Fix: decode first, then
`clear()` + `append` only on success. (The `Crossfade` arm already gets this
right — its comment is accurate.)

### M2. `is_online` returns *true* when it cannot reach the network

> **FIXED** (`5ab2a7d`). A non-success status is offline, and the response must also carry YouTube's own `x-youtube-client-name` � a portal answers with a perfectly good 200. Split into a pure helper and tested without a round trip.
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

> **FIXED** (`5ab2a7d`). Dropped tracks are `mark_failed`'d and emitted. The split is extracted as `partition_retryable` and tested, since the bug was precisely that the dropped ids were filtered away instead of reported.
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

> **FIXED** (`f323ed1`). `fire` reports failures to a sink `App` installs on mount, reusing the same toast the backend's `rift://error` events use � which also removes the inconsistency the review called the real problem.
`ui/src/api.rs:33-39`, plus 41 call sites

`fire` logs to `web_sys::console` and returns, so `Err` values not mirrored by a
backend `rift://error` event reach nobody: `play_tracks`' `"nothing to play"`
(`commands/playback.rs:27`), `create_playlist`'s empty-name error
(`commands/library.rs:33`), and every rejection in `queue_move`,
`move_playlist`, `add_to_playlist`, `toggle_like`, `remove_downloads`. Commands
that *do* emit `events::ERROR` toast correctly, which makes the inconsistency the
real problem. Fix: give `fire` an error path into the existing toast.

### M5. Search failures are indistinguishable from empty results

> **FIXED** (`f323ed1`). Both halves: search failures toast, and clearing the box drops the stale query (bumping the generation so an in-flight search for the deleted text cannot land afterwards) and leaves the results view.
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

> **FIXED** (`f323ed1`) — **then found broken and fixed again** (`c0e108f`). `build.rs` declares
> an app ACL manifest listing all 47 commands, which (confirmed against the Tauri source) is what
> *enables* enforcement for app commands; `capabilities/default.json` grants them. `set_yt_dlp_path`
> validates through `validate_ytdlp_override` (absolute, canonicalized, regular file). One correction: the
> review's suggested remedy does not match the code — the path comes from a free-text field, not the
> native dialog, so "require a file chosen through the dialog" would have meant rewriting the Settings
> control.
>
> **The regression is worth recording in full, because the verification method was itself the bug.**
> `permissions/default.toml` was written as `[[set]] identifier = "default"`, which populates
> `Manifest::permission_sets`. Tauri resolves the literal identifier `default` by consulting *only*
> `Manifest::default_permission` (`tauri-utils/src/acl/resolved.rs`, `get_permissions`), so "default"
> in the capability resolved to **zero** permissions and all 46 commands were rejected at runtime. The
> user reported it as "all my playlists are gone", because `bootstrap` was among them and the UI never
> received the library. It failed silently: `tauri-build` validates that the permission *name* exists, and
> a permission set named `default` does exist, so the build was clean and the capability read as correct.
>
> I had "verified" this by reading the generated artifacts — `acl-manifests.json` showing the set with
> all 46 entries, `capabilities.json` showing "default" granted. Both were true, and neither says
> anything about what the *resolution* produces. **Reading the artifacts is not running the code.** It was
> checkable only by calling `Resolved::resolve`, which is what Tauri does at startup;
> `src-tauri/tests/acl_covers_commands.rs` now does that and is verified to catch the original bug. The
> lesson generalises: anywhere this document describes a check as "read the config and confirmed", the
> check is weaker than it reads.
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

> **FIXED** (`f323ed1`). `SHA2-256SUMS` is fetched from the same release first, the asset looked up, and the digest compared before the write or the chmod. SHA-256 is implemented in-tree rather than adding `sha2`, checked against the FIPS vectors and streaming/one-shot agreement.
`src-tauri/src/fetch.rs:174-212`, `commands/ytdlp.rs:37-60`

`github.com/yt-dlp/yt-dlp/releases/latest/download/<asset>` is fetched, written,
`chmod 0755`'d and later spawned, with no checksum or signature check and no
pinned version. There *is* a genuine trust dependency here (the app already
executes yt-dlp by design), but "latest" plus no verification means a
compromised release asset or TLS path becomes code execution, and it is not
reproducible. Fix: fetch `SHA2-256SUMS` from the same release and verify before
writing/chmod.

### M8. Persistent player state is written on the event loop

> **FIXED** (`2cc59da`). Serialization moved to the persister's own thread, and an identical payload for the same path is dropped � which is exactly the double-emit case. The `Persister` doc now states the final-snapshot loss it previously only implied.
`src-tauri/src/player/playback.rs:195`, `state.rs:101-117`

`emit_queue` clones every `Track`, `serde_json::to_vec`s the snapshot and queues
a disk write — once per track change, once per successful credit enrichment (so
twice on a track's first play), and on every queue mutation. With
`MAX_FETCH_TRACKS = 1000` that is a 1000-element clone plus a write burst from
inside a `tauri::async_runtime::spawn` task sharing the runtime with IPC. Fix:
debounce, or skip the write when the snapshot is unchanged.

### M9. `atomic_write`'s temp name is correct only by accident, and the whole write path is untested

> **FIXED (tests)** (`5ab2a7d`). `util.rs` has a round-trip, a corrupt-file-backup and an atomic-write test, the last pinning the temp-is-beside-the-target property the rename depends on. **DEFERRED:** the `with_extension` shape itself is unchanged � the review verified it is not a live bug, and the test now pins the behaviour rather than leaving it incidental.
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

> **DEFERRED.** The in-code rationale (indices shift) is sound and remapping through an edit is a behaviour change with its own design questions, not a defect. Left as recorded.
`src-tauri/src/player/queue.rs:27-32, 106-108, 135-137`

The in-code rationale (indices shift) is sound, but the user-visible effect is
that removing one track throws away the "already played this cycle" set, so
tracks heard minutes ago become immediately eligible again. Remapping old indices
through the edit would preserve the cycle.

### M11. `parse::tracks` uses the whole subtitle as the artist

> **FIXED** (`226b537`), and `tests/fixtures.rs` no longer enshrines the defect. The rule is deliberately narrow � only structurally identifiable runs are dropped � because a bare year sits where a byline does on album rows and a band named "1975" or "2 Unlimited" would be a real name.
`crates/ytmusic/src/parse.rs:223-228`

`runs_all` concatenates every run of the subtitle column, which for an unlinked
row is `["Song", " • ", "3:22"]` → `"Song • 3:22"`. Verified against
`tests/fixtures/search_tracks.json`, and currently *enshrined* by
`tests/fixtures.rs:94-102`. Fix: drop the leading type badge and trailing
duration run before falling back.

### M12. `find_all(..).first()` is alphabetical, not document order

> **FIXED** (`226b537`). Every known location is read by path with `find_all` kept as an explicit fallback, and `track_credits` matches the requested `videoId` � returning nothing when the response does not contain it, rather than another song's byline.
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

> **FIXED** (`226b537`). Every component parsed with `?`, sums checked, and a bare `"45"` is no longer read as a duration.
`crates/ytmusic/src/parse.rs:174-179`

`filter_map(|p| p.parse().ok())` turns `"1:2x:03"` into `[1, 3]` → 63 s, and the
`m * 60 + s` / `h * 3600 + m * 60 + s` arithmetic is unchecked `u32`: a crafted
value panics in debug and wraps to a bogus duration in release (the workspace
sets no `overflow-checks`). Fix: parse every component with `?` and use
`checked_mul`/`checked_add`.

### M14. `?` inside a reverse loop aborts the scan

> **FIXED** (`226b537`). Both scans `continue`, matching how `duration` already worked.
`crates/ytmusic/src/parse.rs:319` (`audience`), `:362` (`trailing_year`)

`let text = run.get("text").and_then(Value::as_str)?;` inside
`for run in runs.iter().rev()` returns `None` from the whole function on the
first run lacking a string `text`, instead of continuing like `duration`
(`:279`) does — silently dropping a present year or subscriber count.

### M15. Packaged builds are never compiled or bundled in CI

> **FIXED** (`7ddd789`). A `cargo tauri build --no-bundle` step, a tag-versus-manifest assertion in the release job (tested both ways), and tag pushes now publish � `releases/latest` excludes drafts, so a draft-only workflow meant the update banner could never fire.
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

> **FIXED** (`7ddd789`). The direct `reqwest` is 0.13, matching what Tauri pulls in, so `reqwest` is gone from `cargo tree -d` entirely � as are `rand` and `zbus`. The CI guard is a script rather than bare `cargo tree -d`, which exits 0 either way and also flags same-version feature splits.
`Cargo.lock` vs `src-tauri/Cargo.toml:23,26,29`, `crates/ytmusic/Cargo.toml:12`

`reqwest 0.12.28` (direct) **and** `0.13.4` (via tauri 2.11.5); `rand 0.8.8`
(via souvlaki → zbus 3) **and** `0.10.2` (direct); `zbus 3.15.2` + `5.19.0`;
`thiserror 1` + `2`; both `native-tls`/`openssl 0.10.81` and `rustls 0.23.43`.
Two TLS stacks and two rand generations ship in the same binary. Bump the direct
`reqwest` to 0.13 to dedupe, and add `cargo tree -d` as a CI guard.

---

## Low / Nits

> **Status:** L1 **retracted**; L2 folded into M9; L3, L4, L5, L6, L8, L9, L11,
> L12, L14, L15, L16, L17, L18 **fixed**; L7, L10 **deferred** (below); L13
> **partly retracted, partly folded into H1**.

- **L1. `AudioCmd::Volume` ignores in-flight fade-outs** (`audio.rs:183-186`): the
  `fading` vec keeps ramping from the old `volume` captured at `:199`, so dragging
  the slider during a crossfade produces an audible level mismatch that resolves
  only when the fade ends.

  > **RETRACTED — could not reproduce, no change made.** The fade-out gain is
  > recomputed every loop iteration as `volume * (1.0 - t)`, reading the live
  > master volume, and the `Volume` arm writes `volume` before that code runs.
  > Dragging the slider mid-crossfade already retargets both players on the next
  > 30 ms tick. There is no captured copy at `:199` to fix; `volume` appears in
  > exactly four places, all in that loop.

- **L2. `with_extension` trap** (see M9) — documented here for the record since I
  verified it empirically; no live caller is affected.

  > **FIXED (pinned by test)** (`5ab2a7d`). The atomic-write test asserts the temp
  > file's parent equals the target's, so the property the rename depends on is
  > now pinned rather than incidental. The call itself is unchanged — see M9.

- **L3. Offline playback returns `duration: 0.0`** (`fetch.rs:51-57`), which
  `start_playback` uses to overwrite `core.duration` with the metadata guess
  (`playback.rs:57`). The decoder corrects it a moment later, but the seek bar
  jumps.

  > **FIXED** (`2cc59da`) via `preseed_duration`: use the track's own metadata
  > length, and when there is none keep the length the bar already had rather
  > than collapsing it to zero for the fetch window. Tested, including the
  > `Some(0)` case.

- **L4. `epoch` is dead code with a false comment** (`player/state.rs:36-37`): it is
  incremented in three places (`commands/playback.rs:35,62`,
  `commands/queue.rs:92`) and read nowhere. Its doc claims "stale radio fills
  check it" — the radio feature was removed, as `commands/playback.rs:41-42`
  itself says, and `play_track`'s `radio: bool` is accepted then discarded
  (`let _ = radio;`). Delete both.

  > **FIXED** (`7ddd789`). Field, its false doc comment, all three increments, the
  > discarded `radio` parameter and its comment are gone. The command keeps its
  > name and `track` argument, so the frontend call site is unaffected.

- **L5. `queue_add_tracks` is a registered command with no caller** (`main.rs:150`);
  the UI appends one track at a time.

  > **FIXED** (`7ddd789`). Removed from `main.rs`, `build.rs`'s ACL manifest and
  > `permissions/default.toml` — the three places that must agree. `append_tracks`
  > stays (used by `queue_add`, covered by tests). Re-verified: 46 registered,
  > 46 granted.

- **L6. Silent parse degradation.** A renderer rename (e.g. YouTube moving off the
  legacy `nextContinuationData` that `playlist_page:589` reads — the fixture still
  uses it, so only a *new* capture would catch the migration) yields zero results
  with no error. Consider warning when a response contains
  `musicResponsiveListItemRenderer` nodes but nothing is extracted.

  > **FIXED** (`226b537`). A response carrying rows that yield nothing logs a
  > warning. (The parser also reads `nextContinuationData` by path now, with the
  > document-order fallback, so a *moved* continuation is more likely to survive
  > than to be reported.)

- **L7. Regex compiled per request** (`identity.rs:73`, `context.rs:68`) inside the
  request path; cache in a `OnceLock`.

  > **DEFERRED.** Two regexes compiled per request against a host that is already
  > doing a network round trip; the cost is real but negligible next to that, and
  > caching them adds a `OnceLock` plus an error path for a compile that cannot
  > fail on a literal pattern. Worth doing if the request path ever gets hot for
  > another reason.

- **L8. `album_type` is an exact match on a server-controlled title**
  (`parse.rs:526-529`): `"Singles" => "Single", _ => "Album"`. I checked
  `tests/fixtures/artist.json` and it contains **no** Singles shelf at all (only
  "Albums", "Featured on", "Fans might also like"), so nothing pins this mapping
  and any rename silently degrades every card to "Album". Use `starts_with`.

  > **FIXED** (`226b537`). Prefix matching, extracted as `album_type_from_shelf`
  > and tested against "Singles", "Singles and EPs", "EPs" and the non-label
  > titles. Confirmed the downgrade in the review: the fixture's shelf is titled
  > "Singles & EPs", so nothing there pinned the old mapping.

- **L9. `tracks_playlist_id` takes the first `VL…` id under *any* `bottomEndpoint`**
  (`parse.rs:494-508`), not the top-songs shelf's; the fixture's Videos carousel
  already carries one, so a layout change makes "Show all songs" load the wrong
  list with no error.

  > **FIXED** (`226b537`). Reads the top-songs shelf's own link, with a
  > document-order fallback, and is tested with a decoy `VL…` id under a key that
  > sorts earlier — which is what the old value-order walk would have taken.

- **L10. `is_audio_track` is a title substring filter** (`commands/convert.rs:19-35`).
  Fine as a heuristic, but the typed signal (`MUSIC_VIDEO_TYPE_UGC`, already parsed
  at `parse.rs:209`) is strictly more reliable.

  > **DEFERRED.** The review itself calls the current filter fine as a heuristic,
  > and the typed signal only travels with innertube rows — imported playlists and
  > exported JSON do not carry it, so `is_audio_track` would still need its
  > title-based fallback. Switching only the rows that have the signal would make
  > the two paths disagree about the same track.

- **L11. `parse_count` truncates** (`parse.rs:169`) — `.round()` would be correct.

  > **FIXED** (`226b537`). Rounds, and maps non-finite input to `None` rather than
  > relying on float-to-int saturation. Tested with "1.7K" and "infB".

- **L12. `identity.rs` uses `lock().unwrap()`** (`:27,33,39`) where the rest of the
  codebase uses `lock_safe`; a panic in the critical section poisons the pool and
  turns every later call into a panic.

  > **FIXED** (`5ab2a7d`). A `lock_pool` helper recovers the poisoned guard, with
  > the same reasoning as `util::LockExt`.

- **L13. `match_all.first()` risk in `identity::visitor_data`** is covered by H1; the
  regex at `:73` also excludes `=`, so a padded base64 `visitorData` could never be
  matched, and the bootstrap GET lacks `error_for_status()` (contrast
  `context.rs:59`).

  > **PARTLY RETRACTED.** There is no `match_all` in `visitor_data` — the id comes
  > from the `__Secure-YEC` cookie first and the regex second, so the `first()` on
  > a possibly-empty collection does not exist. The padded-base64 limitation is
  > real but harmless: `visitorData` is URL-encoded in practice, and a miss falls
  > through to `Error::Bootstrap` rather than selecting the wrong id. The
  > `error_for_status()` gap is subsumed by H3 (the client now has a timeout) and
  > was left alone: a non-2xx body simply fails to yield visitor data, which is
  > already an error.

- **L14. Release actions float on mutable tags** (`tauri-apps/tauri-action@v0`,
  `dtolnay/rust-toolchain@stable`, `actions/checkout@v5`, `Swatinem/rust-cache@v2`)
  while the release job holds `contents: write` and builds the artifacts users
  install. SHA-pin them, and pin an explicit toolchain version (there is no
  `rust-toolchain.toml`, so CI and release can silently move).

  > **FIXED** (`7ddd789`). All five actions pinned to commit SHAs with the version
  > in a trailing comment (which Dependabot can still update), `rust-toolchain.toml`
  > pins an explicit version, and every workflow passes it explicitly. Note
  > `@stable` there is a *moving branch*, not a tag — and with no `toolchain:`
  > input it installed whatever `stable` was that week, so CI and the released
  > installers could be built by different compilers from the same commit.
  >
  > **Follow-up** (`736c5b3`): the pinned version was 1.98.0, chosen only because
  > it was what happened to be installed. Nothing in the resolved graph needs more
  > than **1.88**, so the pin was not just over-strict — it hid breakage, because
  > building above the floor means the oldest supported toolchain is never
  > exercised. Moved to 1.88.0, and `.github/scripts/check-toolchain-consistency.sh`
  > now fails if the pin, `rust-version`, and the workflow inputs disagree.

- **L15. `src-tauri/gen/schemas/*.json` (~440 KB) is tracked** and not ignored;
  upstream's Tauri template ignores `/gen/schemas`. It churns on every dependency
  bump and can drift from the real plugin set. `git rm --cached -r src-tauri/gen`.

  > **FIXED** (`f323ed1`). Untracked and ignored, along with the build-generated
  > `permissions/autogenerated/`. The hand-written `permissions/default.toml` is
  > tracked.

- **L16. Missing package metadata**: no workspace `authors`/`repository`/`rust-version`,
  and no `bundle.publisher`/`copyright`/`licenseFile`, so `.deb` Maintainer and MSI
  Manufacturer fall back to defaults.

  > **FIXED** (`7ddd789`, and `736c5b3` for `rust-version`). The earlier note here
  > held that declaring a minimum nobody had built against would be a guess
  > presented as a guarantee — sound reasoning, and it is now satisfied rather
  > than abandoned: the workspace was built, tested and linted on 1.88.0 before
  > `rust-version = "1.88"` was declared. The comment in `Cargo.toml` records
  > where 1.88 comes from (`time`/`plist`/`darling`/`icu_*` declare 1.88;
  > `rodio` 1.87 and `reqwest` 1.85 behind them) so the number can be re-derived
  > instead of trusted, and CI builds at that version on every run, so the
  > declaration cannot rot. Declaring it also makes clippy MSRV-aware, so it will
  > not suggest an API newer than the floor.
  >
  > One lint needed a decision: `const_is_empty` fires on `!LOGO_ASSET.is_empty()`
  > under 1.88, correctly. The branch stays with an `#[allow]` and a reason,
  > because `LOGO_ASSET`'s own doc says "Leave empty to show only the cover" —
  > deleting it would break the fork path the constant exists for.

- **L17. No malformed-input tests.** `tests/fixtures.rs` covers only happy paths;
  nothing feeds missing `flexColumns`, `null` titles, wrong JSON types, empty
  arrays, a "no results" response or an error/consent body, and `client.rs`
  (transport, the 403 retry, playlist pagination) is entirely untested. Note
  `examples/prune_fixture.rs:415-436` proves the compacted document parses
  identically *to itself*, so the fixtures lock in current parser output rather
  than validating it — M11 is a case where a test codifies a defect.

  > **FIXED (parser half)** (`226b537`). A malformed-input table covers missing
  > `flexColumns`, `null` titles, wrong JSON types, empty arrays, a "no results"
  > body, a consent body, an error body, a non-object root and pathological
  > nesting — every parser asserted to return nothing rather than panic. Writing
  > it found a real gap: a row with a valid `videoId` but no title column produced
  > a blank, unclickable track. `prune_fixture.rs` now carries a comment stating
  > that its check is an equivalence proof between the original and the compacted
  > document, not a correctness proof.
  >
  > **FIXED (client half)** (`d1cac0c`). Eight transport tests against a real
  > HTTP server on loopback — no new dependency, since `std::net::TcpListener`
  > plus a thread is enough and `tokio` was already a dev-dependency of the crate.
  > They cover the 403 retry rotating the visitor id *and recovering* (H1, end to
  > end), the request actually built (method, content type, the three innertube
  > headers, origin/referer, the consent cookie, the merged `context.client`
  > object), that only a 403 is retried, pagination following continuations in
  > order and truncating at the limit, the no-progress guard, and album/artist
  > routing. The seam is a `#[cfg(test)]` substitution of the base URL rather
  > than an env var, deliberately: an env override would ship in the release
  > binary, and "send every metadata request somewhere else" is not a capability
  > a music player should have.
  >
  > Getting there took three attempts, all recorded in the module's doc comment
  > because each is a mistake a later addition there could repeat: tests racing on
  > the process-global visitor pool (every test now holds a gate, which is blunter
  > than strictly needed and the comment says why); a `clear_override()` placed
  > *after* the call under test, so both retry attempts ran inside the window it
  > was supposed to close; and two wrong fake-response shapes where the parser was
  > right and I was not (`/browse` receives `VL<id>` for a playlist, and a real
  > page carries rows *and* a continuation token together). Note also that the two
  > halves of the H1 fix mask each other — reverting either alone leaves the new
  > test green, and it takes both to reproduce the defect, which is stated in the
  > test rather than glossed over. **Still open:** nothing covers computed style —
  > which is what the white-album-text regression actually needed. The frontend
  > harness added later reaches the IPC call path and the reorder math, not
  > layout; see the closing section.

- **L18. Test litter**: `settings.rs` / `downloads.rs` / `library.rs` tests create
  `%TEMP%\rift-*` dirs and skip cleanup on failure.

  > **FIXED** (`7ddd789`). A `TempDir` drop guard, which also runs while
  > unwinding, so a failing test cleans up like a passing one. Verified by
  > counting the directories across a run. (432 had accumulated here.)

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

  > **Was correct, then a later change broke it and this check did not notice.**
  > Removing `play_track`'s unused `radio` parameter left the frontend sending
  > `"radio": false`, which would have failed at runtime for every single-track
  > play. Re-derived from scratch as a script, it found that one mismatch and
  > nothing else. The check is now a test
  > (`src-tauri/tests/ipc_contract.rs`, 3 tests, `f8e8f0d`) rather than a
  > hand-verification.
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

> **All of the above still holds after the fixes**, with one deliberate amendment:
> `capabilities/default.json` no longer grants only `core:default` +
> `core:window:allow-start-dragging`. It also grants `default`, the app's own
> permission set (M6) — which is the change that puts Rift's own commands *under*
> the ACL rather than outside it. The plugin grants are unchanged, and the CSP is
> untouched.

---

## Follow-ups found while fixing

Issues this session surfaced that were not in the original review. None is
currently a defect in shipped behaviour; they are recorded so the next pass
starts from the code rather than from scratch.

- **Two checks in this document were validated by reading artifacts rather than
  running code, and one of them shipped a broken build.** The ACL above is the
  case in point; the `play_track` call-site breakage is the same shape. Both now
  have tests that execute the real machinery:
  `src-tauri/tests/acl_covers_commands.rs` (3 tests, `c0e108f`) resolves the ACL
  exactly as Tauri does at startup, and `src-tauri/tests/ipc_contract.rs`
  (3 tests, `f8e8f0d`) reads both sides of the IPC seam. Between them they found
  two real regressions; neither was visible to the compiler.
- **`bootstrap not allowed` was reported by a user, not by CI — and could have
  been.** Everything about that failure was detectable from `cargo test`: the
  generated artifacts are on disk after a build, and the resolution is a library
  call. The gap was that nothing asserted the *runtime* consequence of the ACL,
  only that its inputs looked right.
- **`uploader`/`channel`-style rows are still filtered by title substring**
  (`commands/convert.rs`) — see the deferred L10 above. The blocker is that the
  typed signal does not travel with imported playlists.
- **`client.rs` transport, the 403 retry and playlist pagination are untested.**
  The parser now has a malformed-input table, but exercising the retry needs a
  mock HTTP server. `crates/ytmusic` has `tokio` as a dev-dependency already, so
  a `TcpListener` speaking canned responses would do it without a new crate; the
  decision to be made is whether that belongs in this repo's test suite.
- **The frontend had no test harness at all.** C1, H4 and M5b are in Yew
  components and carry no test for this reason, as does the white-album-text
  regression. **Partly done** — see the closing section: `wasm-bindgen-test` is
  now wired into CI and covers the IPC boundary, the reorder math and `App`'s
  pure decisions. C1 (hook order), H4 (progress on track change) and the
  computed-style class all need the renderer, and the closing section says which
  of those need a browser and which need a component harness.
- **The `ui` clippy pass needs `wasm32-unknown-unknown`; `trunk build` needs
  `NO_COLOR` set to a value trunk accepts.** Trunk 0.21 rejects `NO_COLOR=1`
  ("invalid value '1' for '--no-color'"). CI does not set it, so this only bites
  a developer whose environment does — worth knowing before debugging a local
  build failure that CI does not reproduce.
- **`cargo tauri build --no-bundle` was added to CI but not run locally** in this
  session (it compiles the release profile with LTO). The flag is present in the
  installed CLI and the config parses under `generate_context!`, but the first
  CI run is what actually exercises it.
- **A stale generated permission file survives a command removal.**
  `src-tauri/permissions/autogenerated/queue_add_tracks.toml` is still on disk
  after the command was deleted (L5); it is harmless because nothing grants it,
  and it is gitignored, but `verify` never prunes files for commands that no
  longer exist. Worth a `git clean`-style note in the build docs.


---

## What the frontend harness does and does not cover

`rift-ui` was the one crate with no tests: ~160 KB of source, including a 62 KB
`app.rs`, and no way to verify any of it except by running the app and clicking.
That is how the `play_track` argument-shape breakage reached a runtime failure
with both sides compiling, and how the white album text reached a user.

`ui/src/tests/` is now a `wasm-bindgen-test` harness, 39 tests, run under Node by
`wasm-bindgen-test-runner` and wired into CI beside the frontend clippy pass.
`ui/README.md` is the operational reference; this is the record of what it buys.

### Covered

| Priority | Where | What is asserted |
|---|---|---|
| 1. The IPC boundary | `ui/src/tests/api.rs` | The JS argument object each command sends — key names, key sets, and **value types** — read back out of `serde_wasm_bindgen`, so it is observed rather than restated. Plus the error sink, response deserialization, and the non-string-rejection fallback. |
| 2. Drag/drop index math | `ui/src/tests/reorder.rs` | `slot_at_offset` and `drag_shift_style`: the last row's bottom edge, exact slot boundaries across a range of list lengths and row heights, scroll offset, and that a preview is a permutation of the slots (parsed back out of the generated CSS, not recomputed). |
| 3. `app.rs` decisions | `ui/src/tests/app.rs` | `shuffled_start`, `fmt_total_duration`, `playlist_fully_downloaded`. |
| 4. Components, first render | `ui/src/tests/components.rs` | That `QueuePanel` and `PlayerBar` render at all and render what their props describe — the empty queue rendering nothing, a one-track queue rendering exactly one row, a loaded track reaching the bar. Added in a second pass, so the suite is 39 tests. |

Components are rendered without a DOM through Yew's `ssr` feature, declared as a
dev-dependency so the shipped bundle never links it. That was measured rather
than assumed: `trunk build` with the declaration, without it, and with it again
produced a byte-identical wasm each time, and neither crate the feature adds
appears in the non-test dependency tree.

The harness sits at the Tauri bridge: `spawn` and `tauri_invoke` are replaced
under `cfg(test)`, and everything above them — the real serialization, the real
error routing, the real call-site functions — runs for real. Nothing depends on
a running app, a network, or a `window`.

Every test was mutation-tested: the fix was reverted and the test watched to
fail. Two mutations **survived**, and they are the honest boundary:

- **Dropping `.floor()` or `.max(0.0)` from `slot_at_offset` changes nothing.**
  `as usize` on an `f64` truncates toward zero and saturates at zero, so both are
  redundant against the implementation. They are kept as statements of intent,
  not as behaviour the tests defend. (`min(len - 1)`, by contrast, is
  load-bearing: two tests fail without it.)
- **A `play_track`-style argument change inside an unnamed `app.rs` closure is
  not caught.** This is the finding that mattered. The first version of the
  `play_track` test built its own `json!` literal, and the mutation that
  re-instated the exact historical defect passed. Fixed by extracting
  `app::fire_play_track` — the function the row and Home handlers now call — so
  the test drives the real call site. Once it did, the same mutation failed with
  `left: ["radio", "track"], right: ["track"]`. The lesson generalises: a test
  that restates a payload proves the transport works and nothing about whether
  the app uses it.

### Not covered

**Computed style — the class of bug that already shipped.** The white album text
came from a `<button>` inheriting `buttonface`, and nothing here can see it. It
needs a real layout engine, a `getComputedStyle` call, and a browser; that is a
separate harness (chromedriver or `wasm-bindgen-test`'s in-browser mode) with its
own install and version pinning on three operating systems. **This gap is open,
and it is recorded as open rather than implied to be closed by the tests above.**
The harness is deliberately not described as covering it.

**Second renders, and therefore hooks.** The component tests render each
component **once**, and that is a deliberate boundary rather than an oversight.
Yew's server renderer drives a component instance a single time and does not run
effects, so nothing in this harness sees a second render — which is exactly where
C1 and H4 live. Yew asserts that a render's hook count matches the *previous*
render's, so C1 panics only when the queue panel goes from an empty queue to a
non-empty one, and H4 is about what happens when the track prop changes. Both
need a second pass this mechanism cannot produce; two attempts to reach one
through SSR are recorded as dead ends in `ui/src/tests/components.rs` so the
next person does not repeat them. Closing it needs a hook harness or a DOM shim
driving the real client renderer. Worth doing; not done.

**Most call sites' argument shapes.** `src-tauri/tests/ipc_contract.rs` parses
`api::fire("cmd", json!({...}))` as text and compares key names against the
backend's parameters. It is textual: it cannot see value types, and it does not
see through a helper that builds its payload elsewhere — `fire_play_tracks` was
invisible to it before this work. The two `play_*` commands are now covered end
to end from the frontend; the other 53 IPC call sites still have the text check
and nothing more.

### Two things worth knowing before trusting a green run

**The suite cannot run on Windows, for a reason outside this repo.**
`wasm-bindgen-test-runner` 0.2.128 builds its Node script with
`Path::display()` inside single quotes, so a Windows path becomes
`'C:\Users\...'` and Node dies parsing `\U` as an escape *before any test runs*.
It is unconditional (the `readFile` is inside `if (false)`, which is still
parsed) and 0.2.129 does not fix it. `WASM_BINDGEN_BENCH_RESULT` does not help:
the path is re-encoded through `PathBuf`, so the backslashes come back. CI
therefore runs the step on Linux and macOS only. The tests themselves are
platform-independent — the same wasm module and the same assertions — so this
costs local reproduction, not coverage. Run them under WSL on Windows.

**The first harness design was wrong and the failures were confusing.** It used
`spawn_local` and yielded to the event loop, on the assumption that
`wasm-bindgen-test` drives the page's executor. It does not: every async test
failed with the same `expected exactly one IPC call, got 0`, which reads like a
missing call rather than an un-polled one, and a single test run in isolation
failed identically — which is what ruled out the cross-talk hypothesis I started
with. The fix was to stop racing an executor: `spawn` queues the future and
`settle()` drains it on the test's own stack, which also made the async tests
synchronous and therefore immune to interleaving. The same shape as the L17
harness lesson — when a harness misbehaves, suspect the harness first.
