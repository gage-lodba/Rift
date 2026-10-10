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
> **Deferred, with reasons — all four since closed:** L7 regex caching
> (`a3a55f1`), L10's typed UGC signal (`9de9f22`), M10 shuffle-history remapping
> (`9a5679f`) and the `with_extension` shape in M9 (`1d2bc94`). Each item carries
> what closing it took, and two of the four needed their original reasoning
> corrected rather than merely completed. L16's `rust-version` and L17's
> `client.rs` transport tests were deferred and have since been done.
>
> **Added after this review closed — the frontend harness.** `rift-ui` was the
> one crate with no tests, so every UI change was unverifiable except by running
> the app. There is now a `wasm-bindgen-test` harness (`ui/src/tests/`, 53 tests,
> wired into CI next to the frontend clippy pass) covering the IPC boundary's
> argument shapes, the reorder index math, `App`'s pure decisions about
> shuffling, durations, and the offline pin, and — since the DOM shim landed —
> **second renders**, which is how `C1`'s behaviour and `H4`'s re-seed are
> covered. **The computed-style gap is still open and is now explicit rather
> than implied**: the white album text came from a `<button>` inheriting
> `buttonface`, which needs a real layout engine and `getComputedStyle`, and that
> is a different purchase from this harness — the DOM shim is the step toward it,
> not the thing itself. See "What the frontend harness does and does not cover"
> at the end of this document.

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
>
> **Follow-up (`a3a55f1`) — here the *guards*, not the fix, were the defect.** The
> visitor pool is process-global, and `identity::tests`' own lock and
> `client::tests`' gate did not exclude each other, so under `--test-threads=1` (or
> an unlucky schedule) the identity tests failed **on unmodified code**. One gate
> now covers both modules, and every identity test seeds a known pool through
> `seed_pool_for_test` instead of assuming an empty one; removing that seeding
> reproduces the reported failure exactly. The same "green for the wrong reason"
> shape this document already records for the L17 client harness.

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

> **FIXED** (`7df0c81`). Only a successful scrape is cached; the fallback stays per-call. The caching rule is extracted as a synchronous helper so both branches are tested without a network. Writing that test found that `OnceLock::get_or_init` alone memoizes its closure's `None` too — the first implementation still cached the fallback.
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

> **FIXED** (`5ab2a7d`). A non-success status is offline, and the response must also carry YouTube's own `x-youtube-client-name` — a portal answers with a perfectly good 200. Split into a pure helper and tested without a round trip.
>
> **CORRECTED — the discriminator does not exist, and requiring it made
> `is_online()` permanently `false`.** YouTube's web root never sends
> `x-youtube-client-name`: a live `HEAD https://www.youtube.com/` answers `200`
> with `server: ESF` and `content-type: text/html`, and no such header.
> `crates/ytmusic/src/client.rs:759` shows where the name really appears — it is a
> header **Rift sends** on innertube requests, not one the HTML root returns. So
> the probe answered "offline" on a machine that was online, and
> `src-tauri/src/commands/downloads.rs:267` then aborted every kept-offline retry
> pass with `offline; giving up on N kept-offline download(s)`. The ~7.5-minute
> backoff this item exists to avoid became a permanent refusal instead.
> Falsified live from this machine. **FIXED** (`e867b34`): the probe now requires
> a 2xx whose *final URL host* is `youtube.com` or a subdomain, compared with a
> label boundary so `notyoutube.com` does not pass. `server: ESF` is recorded as
> corroboration and deliberately not required, and the host comes from the
> redirect chain rather than a mutable Google header, so a header rename cannot
> silently invert the answer — the hardening this needed
> (`src-tauri/src/util.rs:169` `is_online`, `:219` `came_from_youtube`, `:279` the
> live-shape test). Mutations verified: restoring the header rule, restoring the
> pre-M2 2xx-only rule, dropping the host gate, and a naive `ends_with` each fail
> a named test.
>
> **Corrected once more** (`754e4e1`, the frozen revision) — the *status* was still
> in the gate, so a 403, 429 or 503 from YouTube's own edge read as offline, and
> rate limiting is exactly when the download retry pass runs: the same
> false-offline class returned, with a more plausible trigger.
> `came_from_youtube` (`src-tauri/src/util.rs:226`) now answers on the final URL
> host alone, the status deliberately still in the signature and documented as
> ignored so the test stating the indifference ("a 301 from YouTube is online")
> keeps it visible and re-adding a gate fails immediately instead of being
> invisible. A transport failure is the only "offline" now, decided inside the
> tested rule rather than in an untestable branch of `is_online` (`:169`), which
> logs the status at debug for diagnosis. Tests cover 200/204/301/302/403/429/500/503
> from a YouTube host, a portal's host with the same statuses, the near-miss host
> boundaries, and no response at all; the old
> `non_success_statuses_are_offline_even_from_youtube` was deleted rather than left
> pinning behaviour that had been removed. Three rounds on one small probe is this
> item's own lesson: each fix was defensible and each was wrong about what "online"
> has to mean to the caller.
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

> **FIXED** (`5ab2a7d`). Dropped tracks are `mark_failed`'d and emitted. The split is extracted as `partition_retryable` and tested, since the bug was precisely that the dropped ids were filtered away instead of reported; `e47986b` then made the release structurally inseparable from the split (`partition_and_release`), so the two cannot drift apart again.
`src-tauri/src/commands/downloads.rs:264-272`

```rust
let batch = failed.clone();
let retry: Vec<Track> = failed.into_iter().filter(|t| kept_offline(&app, &t.id, &batch)).collect();
if retry.is_empty() { return; }
```

`downloads.fail(id)` releases the claim first, so a track dropped by this filter
is gone from the retry set with **no claim, no offline copy and no failure
record** — its row looks like a track the user never downloaded, offers no retry,
and nothing revisits it. The playlist was edited out from under it, which is why
it was dropped. Fix: `mark_failed` the dropped ids and `emit_downloads`.

> **CORRECTED — this item described a state the code cannot reach.** It said a
> dropped track "may still show a spurious 'downloading' state" because of a claim
> `downloads.fail` left on it. **It leaves none:** `Downloads::fail`
> (`src-tauri/src/downloads.rs:88-92`) *removes* the in-flight claim, so there is
> no spinner and nothing spurious. The defect is the opposite one — the absence of
> any state at all, which is worse than a wrong one, because a row with no state
> offers nothing to click and nothing revisits it. Found by `backend-core` while
> writing the regression guard; the code and both functions' docs now say this, and
> a reader who goes looking for the leftover claim will not find it. The fix's
> shape also changed in `e47986b`: the split and the release are now one function
> (`partition_and_release`, `src-tauri/src/commands/downloads.rs:137-161`), because
> a caller able to take `retry` without acting on `dropped` *is* the defect.

### M4. `api::fire` makes 41 of 55 IPC calls fire-and-forget

> **FIXED** (`f323ed1`). `fire` reports failures to a sink `App` installs on mount, reusing the same toast the backend's `rift://error` events use — which also removes the inconsistency the review called the real problem.
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

> **FIXED** (`2cc59da`). Serialization moved to the persister's own thread, and an identical payload for the same path is dropped — which is exactly the double-emit case. The `Persister` doc now states the final-snapshot loss it previously only implied.
`src-tauri/src/player/playback.rs:195`, `state.rs:101-117`

`emit_queue` clones every `Track`, `serde_json::to_vec`s the snapshot and queues
a disk write — once per track change, once per successful credit enrichment (so
twice on a track's first play), and on every queue mutation. With
`MAX_FETCH_TRACKS = 1000` that is a 1000-element clone plus a write burst from
inside a `tauri::async_runtime::spawn` task sharing the runtime with IPC. Fix:
debounce, or skip the write when the snapshot is unchanged.

### M9. `atomic_write`'s temp name is correct only by accident, and the whole write path is untested

> **FIXED (tests)** (`5ab2a7d`). `util.rs` has a round-trip, a corrupt-file-backup and an atomic-write test, the last pinning the temp-is-beside-the-target property the rename depends on. **DEFERRED:** the `with_extension` shape itself is unchanged — the review verified it is not a live bug, and the test now pins the behaviour rather than leaving it incidental.
>
> **CLOSED** (`1d2bc94`). The deferred half is done, and not by pinning the
> incidental name: `atomic_write` now builds the temp path from the target's
> `parent()` with a unique name (pid + counter), so "beside the target" — the
> property the rename actually depends on — holds by construction, two writers
> cannot collide, a failed write or rename removes the scratch file, and a path
> with no file name is rejected instead of guessed at
> (`src-tauri/src/util.rs:124`). The pinning test stays, and now asserts the
> property rather than the name.
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

> **FIXED** (`9a5679f`). The remembered order is no longer thrown away: every mutator maps the old indices through its own edit with `remap_history` (`src-tauri/src/player/queue.rs:31`), called from all four edit paths — found by name rather than by line, because the call sites moved twice as this item was hardened (this entry's first draft cited `:66, :158, :165, :196`; they are now `:73, :165, :172, :203`) — so the "already played this cycle" set survives a removal, an insert or a reorder. The design questions this item flagged are answered in the helper's doc rather than deferred: an index with no survivor is dropped, and a partial survivor keeps its relative order.
>
> **HARDENED** (`a213f54`), and the guard was the interesting part. The property test's first failure — "the cursor must name the playing track" — was **not a bug**: between a prefetch and the play that consumes the pin, the cursor deliberately names the reserved track while `current` is still the previous one. The invariant was wrong, so it was replaced by three that are true (structural soundness; the playing track is in the played set under shuffle; nothing playing implies empty history) plus a strong form asserted at the commands where it must hold — with the permitted intermediate states written down at the assertion (`player/tests.rs:1133`) rather than the check being quietly relaxed. With the invariants right, 8 000 steps over four fixed seeds (`tests.rs:1243-1251`) found **two real defects in this area**: `toggle_shuffle` rewrote the history to `[current]` while leaving `pending_next` armed, so the track a pinned pick later started was missing from the played set and Next could restart it immediately (fixed by `anchor_playing`, `player/nav.rs:90, :112`); and a cycle rollover cleared the history while the old track was still playing, so the pool could hand a still-playing track straight back (fixed by carrying the playing track into the new cycle as its first entry). Each fix has a deterministic test and the property test at a named seed and step. Residual, documented rather than hidden: a pick dropped without playing stays in the played set for that cycle — a fairness quirk, not a safety one.
>
> **HARDENED AGAIN** (`1c1caf6`): a prefetched pick is dropped when shuffle is toggled, and the anchors that remain are covered by tests. Three rounds on one item is also why its citations aged — `9a5679f` → `a213f54` → `1c1caf6` moved `remap_history`'s call sites while the line numbers in this document stayed where they were written. Re-measure these references rather than trusting them.
`src-tauri/src/player/queue.rs:31` (`remap_history`). The ranges this item cited when it was written — `:27-32, 106-108, 135-137` — no longer resolve: the item was hardened three times after they were measured, so they are recorded here rather than re-asserted.

The in-code rationale (indices shift) is sound, but the user-visible effect is
that removing one track throws away the "already played this cycle" set, so
tracks heard minutes ago become immediately eligible again. Remapping old indices
through the edit would preserve the cycle.

### M11. `parse::tracks` uses the whole subtitle as the artist

> **FIXED** (`226b537`), and `tests/fixtures.rs` no longer enshrines the defect. The rule is deliberately narrow — only structurally identifiable runs are dropped — because a bare year sits where a byline does on album rows and a band named "1975" or "2 Unlimited" would be a real name.
`crates/ytmusic/src/parse.rs:223-228`

`runs_all` concatenates every run of the subtitle column, which for an unlinked
row is `["Song", " • ", "3:22"]` → `"Song • 3:22"`. Verified against
`tests/fixtures/search_tracks.json`, and currently *enshrined* by
`tests/fixtures.rs:94-102`. Fix: drop the leading type badge and trailing
duration run before falling back.

### M12. `find_all(..).first()` is alphabetical, not document order

> **FIXED** (`226b537`). Every known location is read by path with `find_all` kept as an explicit fallback, and `track_credits` matches the requested `videoId` — returning nothing when the response does not contain it, rather than another song's byline.
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

> **FIXED** (`7ddd789`). A `cargo tauri build --no-bundle` step, a tag-versus-manifest assertion in the release job (tested both ways), and tag pushes now publish — `releases/latest` excludes drafts, so a draft-only workflow meant the update banner could never fire.
>
> **CORRECTED — the build step this added could not run, anywhere.** It invoked
> `cargo tauri build --no-bundle`, which is a cargo *subcommand*: cargo resolves
> it to a `cargo-tauri` binary on `PATH` (or beside cargo itself), and CI
> installed none — the hosted runners ship none, and `tauri-cli` is in no manifest
> and no `Cargo.lock` here. It would have stopped at its first line with
> `error: no such command: 'tauri'` (exit 101), before compiling anything.
> Confirmed by running the same command with `CARGO_HOME` pointed at a directory
> without it; a developer machine hides this because a previous
> `cargo install tauri-cli` leaves one in `~/.cargo/bin`, which is exactly why
> the local run recorded below passed. `ci.yml` now installs a pinned
> `tauri-cli@2.11.5` (the `tauri` version `Cargo.lock` resolves) before the build.
> It is the same lesson this document records twice already: a check that passed
> by hand is not a check. Both it and the frontend suite's never-run job share the
> reason they went unnoticed for so long: the branch carrying them was never
> pushed, so CI ran neither step.
>
> **And the bundle inputs are now exercised for real** (a local Windows bundle run,
> re-measured for this note): both installers were produced —
> `bundle/msi/Rift_0.5.0_x64_en-US.msi` at 7,061,504 bytes and
> `bundle/nsis/Rift_0.5.0_x64-setup.exe` at 5,119,744 — with
> `rift-music.exe` at 18,282,496, so `mainBinaryName` and `targets: "all"` both
> took effect; a scan of the 372-line log for licence/WiX/NSIS/warning/error terms
> found nothing unexpected; and the plain-text `licenseFile` was **converted** to the
> `LICENSE.rtf` WiX requires (`release/wix/LICENSE.rtf`, pointed at by
> `release/wix/x64/main.wxs:48`), so the packaging question this area left open is
> closed rather than deferred. WiX and NSIS were absent locally, so tauri-cli
> fetched and hash-validated both toolchains itself.
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

> **FIXED** (`7ddd789`). The direct `reqwest` is 0.13, matching what Tauri pulls in, so `reqwest` is gone from `cargo tree -d` entirely — as are `rand` and `zbus`. The CI guard is a script rather than bare `cargo tree -d`, which exits 0 either way and also flags same-version feature splits.
`Cargo.lock` vs `src-tauri/Cargo.toml:23,26,29`, `crates/ytmusic/Cargo.toml:12`

`reqwest 0.12.28` (direct) **and** `0.13.4` (via tauri 2.11.5); `rand 0.8.8`
(via souvlaki → zbus 3) **and** `0.10.2` (direct); `zbus 3.15.2` + `5.19.0`;
`thiserror 1` + `2`; both `native-tls`/`openssl 0.10.81` and `rustls 0.23.43`.
Two TLS stacks and two rand generations ship in the same binary. Bump the direct
`reqwest` to 0.13 to dedupe, and add `cargo tree -d` as a CI guard.

---

## Low / Nits

> **Status:** L1 **retracted**; L2 folded into M9; L3, L4, L5, L6, L8, L9, L11,
> L12, L14, L15, L16, L17, L18 **fixed**; L7 (`a3a55f1`) and L10 (`9de9f22`)
> **fixed** after being deferred — both needed their deferred reasoning corrected
> rather than just completed, see their items; L13 **partly retracted, partly
> folded into H1**.

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

  > **FIXED** (`a3a55f1`). Both are compiled once behind a helper, and the reason
  > the deferral gave — the pattern is a literal, so compilation cannot fail — is
  > now the reason that helper can return `&'static Regex` rather than a `Result`,
  > which removes the error path the deferral was objecting to. Pinned with
  > `ptr::eq` across two calls, so the mutation that leaks a fresh `Regex` per call
  > fails a named test. The visitor extraction had no test at all before this; it
  > has one now, as does L12's poison recovery, which was implemented untested.

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

  > **FIXED** (`9de9f22`), with a correction to this deferral's reasoning. The
  > typed signal is now enforced where it *does* travel: `is_non_audio_row`
  > (typed `MUSIC_VIDEO_TYPE_UGC` **or** the non-audio badge) is applied at the
  > point a row becomes a `Track`, so a UGC-typed row with a clean title is
  > dropped instead of surviving because its title carries no marker. The
  > signal-less paths (imported and exported JSON) keep the title fallback, and
  > `commands/convert.rs`'s module doc now states the real split — so the two
  > paths differ by decision rather than silently disagreeing, which is the
  > objection the deferral actually rested on.
  >
  > The deferral was also wrong about the fixture: the recorded playlist has no
  > `musicVideoType` keys at all, so playlist import genuinely carries no typed
  > signal, and enforcing the typed half only where it exists does not create the
  > disagreement it feared. Mutation evidence for how unpinned this was: removing
  > the typed half brings the clean-title UGC row back and all 40 pre-existing
  > tests stay green.

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
>
> **Follow-up found in the CI audit — the guard had a hole on the other side.** It
> compared the pins that *exist*, so a workflow that deleted its `toolchain:`
> input left the comparison with nothing to look at: the script still printed
> "toolchain version is consistent across Cargo.toml, rust-toolchain.toml, and the
> workflows" and exited **0**, which is the exact drift this item exists to
> prevent (`@stable` with no input installs whatever `stable` is that week). It
> now also counts `dtolnay/rust-toolchain` uses against `toolchain:` inputs, per
> workflow file, and fails naming the file
> (`.github/scripts/check-toolchain-consistency.sh:69-84`). The three paths were
> tested by deleting one pin, both pins, and the release job's pin.

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
  `pick_next` guards `cursor + 1 < len`, and since `9a5679f` the queue mutators
  *remap* the pair through the edit (`remap_history`,
  `src-tauri/src/player/queue.rs:31`) rather than resetting it, which is a
  stronger property than the one traced here — the history is carried, not
  dropped. The uncommitted diff's `seed_shuffle_anchor` is a real
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
- **`uploader`/`channel`-style rows are filtered by title substring on the
  signal-less paths only, and now by decision** (`commands/convert.rs`). The typed
  signal is enforced where it travels (`9de9f22`, L10 above); imported and
  exported JSON keep the fallback, and the module doc states the split instead of
  leaving the two paths to disagree silently.
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
  >
  > **Sharpened later, and it is worse than it reads:** this is not confined to
  > `trunk build`. `cargo tauri build --no-bundle` runs `trunk build --release`
  > as its `beforeBuildCommand`, and that child inherits the environment — so a
  > `NO_COLOR=1` in the shell fails the *whole application* build with
  > `Error beforeBuildCommand `trunk build --release` failed with exit code 2`,
  > which names neither `NO_COLOR` nor the value it rejected. Reproduced both
  > ways in one session: `NO_COLOR=1` → exit 1, `NO_COLOR=true` → exit 0, same
  > tree. Worth knowing that a local release build can fail for a reason CI
  > structurally cannot see.
- **`cargo tauri build --no-bundle` was added to CI but not run locally** in this
  session (it compiles the release profile with LTO). The flag is present in the
  installed CLI and the config parses under `generate_context!`, but the first
  CI run is what actually exercises it.
  >
  > **DONE, run locally.** `rift-music.exe`, 17.4 MB, `Finished 'release' profile
  > [optimized] in 3m 30s` with `lto = true` and `codegen-units = 1`. The claim
  > held: the release profile and the bundle inputs the linker does not read are
  > exercised by this, and nothing in them is malformed.
  >
  > **CORRECTED — the first CI run could not have exercised it.** The local run
  > above passed because a `cargo-tauri` binary was already in `~/.cargo/bin` from
  > an earlier `cargo install`; a hosted runner has none, `tauri-cli` appears in no
  > manifest and no `Cargo.lock` here, and the job installed nothing. So
  > `cargo tauri build --no-bundle` would have stopped at its first line with
  > `error: no such command: 'tauri'` (exit 101), before compiling anything —
  > verified by running the same command with `CARGO_HOME` pointed at a directory
  > without it. `ci.yml` now installs a pinned `tauri-cli@2.11.5` first. The
  > lesson is the one this document records twice already: a check that passed by
  > hand is not a check.
- **A stale generated permission file survives a command removal.**
  `src-tauri/permissions/autogenerated/queue_add_tracks.toml` is still on disk
  after the command was deleted (L5); it is harmless because nothing grants it,
  and it is gitignored, but `verify` never prunes files for commands that no
  longer exist. Worth a `git clean`-style note in the build docs.
  >
  > **RESOLVED — file removed, and the pruning question answered.** Deleting it
  > by hand and re-running `cargo tauri build --no-bundle` succeeds (exit 0), so
  > the file is genuinely dead: the build regenerates the 46 files for the
  > commands that still exist and does not recreate this one. It was never a
  > silent-grant risk — `[default]` does not name it.
  >
  > **A guard test was considered and rejected, for an ordering reason rather
  > than a difficulty one.** The autogenerated directory is gitignored, so on a
  > fresh runner it does not exist until something builds it — and CI runs
  > `cargo test` *before* `cargo tauri build --no-bundle`. A test asserting
  > "every file here names a live command" would therefore have to skip when the
  > directory is absent, which is exactly the vacuous-pass shape this work has
  > been avoiding elsewhere. The honest place for this is the build docs: if you
  > rename or delete a command, the stale file is inert and safe to delete, and
  > nothing will regenerate it.

### Found in the sessions that followed (CI, packaging, and the second UI pass)

Six of these correct statements this document still makes. They are recorded here
as well as in the session report, because the next reader comes from the code.

- **CI's `cargo tauri build --no-bundle` step could never have run**: no
  `tauri-cli` on any runner, none in the manifests, none installed by the job. See
  the correction under M15 and the note in "Follow-ups found while fixing". Fixed
  — `ci.yml` installs a pinned `tauri-cli@2.11.5`.
- **`check-toolchain-consistency.sh` exited 0 with the `toolchain:` input
  deleted.** See the correction under L14. Fixed, with the three failure paths
  tested by mutation.
- **The frontend `[env]` table never loaded the DOM shim on any platform**, so the
  suite was red wherever it ran and the Linux/macOS CI job carrying it would have
  failed had it ever been pushed — and it never was, so CI ran neither the step nor
  the failure. That is the same reason M15's build step went unnoticed: the branch
  was unpushed, which is precisely the "verified by reading, not running" theme
  this document opens with. See "Three things worth knowing before trusting a green
  run". Fixed (`49bb626`).
- **`detect_ytdlp` was a second unbounded subprocess wait behind H3** — the same
  defect H3 names, on the path H3 did not cover. Bounded by `3051d7c`
  (`src-tauri/src/fetch.rs`, `run_version_probe`), with a test that spawns a real
  sleeper and fails after ~29 s without the timeout.
- **The hook-order scanner false-positived on a `return` inside a closure** — the
  shape half of this codebase's callbacks have. Fixed in `49bb626`, with cases for
  a closure `return`, a hook inside a closure, and the word `return` inside a
  string or a comment (`ui/src/tests/hook_order.rs`).
- **UI round two** (`f19e2ab`): Phase 5 was complete for the controls it named,
  not for the way *into* them. Four row/header `onkeydown` handlers called
  `prevent_default()` for Enter aimed at a nested `<button>`, so a focused kebab
  played the track — now a shared `key_on_nested_control` guard
  (`ui/src/components/mod.rs:79`), tag-based because Yew 0.21 delegates events to
  the subtree root. The sidebar playlist kebab was `display:none` except on hover
  and unreachable by Tab; four hover-revealed controls hid their own focus ring
  under `opacity:0`; the submenu's ArrowRight indexed submenus instead of
  `props.actions` (`ui/src/components/menu.rs`); five `api::invoke` sites
  discarded the rejection, the worst replacing a backend reason with "not found"
  (`ui/src/api.rs:124`, `invoke_or_default`) — the commit records these as silent
  IPC failures M4 undercounted; the Artists/Albums tab fetches captured no
  generation token; and the crossfade slider, three `role=switch` buttons, the
  rename input and the cover images had no accessible name at all.


---

## What the frontend harness does and does not cover

`rift-ui` was the one crate with no tests: ~160 KB of source, including a 62 KB
`app.rs`, and no way to verify any of it except by running the app and clicking.
That is how the `play_track` argument-shape breakage reached a runtime failure
with both sides compiling, and how the white album text reached a user.

`ui/src/tests/` is now a `wasm-bindgen-test` harness, 53 tests, run under Node by
`wasm-bindgen-test-runner` and wired into CI beside the frontend clippy pass.
`ui/README.md` is the operational reference; this is the record of what it buys.

The harness has **two layers**, and which one a test sits in decides what it can
see:

- **No DOM** — the Tauri bridge is replaced and everything above it runs for
  real. Covers the IPC boundary's argument shapes, the reorder math, `App`'s
  pure decisions, and *first* renders through Yew's server renderer.
- **A real DOM** (`jsdom`, installed into Node by `ui/tests/dom-shim.cjs`) — the
  real Yew **client** renderer. This is the only layer that can see a second
  render, an effect, or a callback. It is what `C1` and `H4` needed.

### Covered

| Priority | Where | What is asserted |
|---|---|---|
| 1. The IPC boundary | `ui/src/tests/api.rs` | The JS argument object each command sends — key names, key sets, and **value types** — read back out of `serde_wasm_bindgen`, so it is observed rather than restated. Plus the error sink, response deserialization, and the non-string-rejection fallback. |
| 2. Drag/drop index math | `ui/src/tests/reorder.rs` | `slot_at_offset` and `drag_shift_style`: the last row's bottom edge, exact slot boundaries across a range of list lengths and row heights, scroll offset, and that a preview is a permutation of the slots (parsed back out of the generated CSS, not recomputed). |
| 3. `app.rs` decisions | `ui/src/tests/app.rs` | `shuffled_start`, `fmt_total_duration`, `playlist_fully_downloaded`. |
| 4. Components, first render | `ui/src/tests/components.rs` | That `QueuePanel` and `PlayerBar` render at all and render what their props describe — the empty queue rendering nothing, a one-track queue rendering exactly one row, a loaded track reaching the bar. |
| 5. Hook order, statically | `ui/src/tests/hook_order.rs` | That no hook sits below an early return in any component source — the structural cause of **C1**. Mutation-verified: with C1's defect re-injected into the queue panel, the check fails at the offending line *and* the render test below aborts on Yew's own assertion. |
| 6. Second renders | `ui/src/tests/render.rs` | The real client renderer in a real DOM: the harness's own proof that it runs effects and re-renders; the queue panel surviving **empty-then-populated**, the transition C1 broke; and `PlayerBar` re-seeding from the app's props across a **track change**, driven through the real PROGRESS subscription rather than a callback called by hand. |

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
fail. Three mutations **survived**, and they are the honest boundary:

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
- **Removing the track key from `PlayerBar`'s re-seed effect fails nothing in
  `render.rs`.** Measured by building it both ways, and it is the sharpest
  boundary the harness has. The stale `live` values are masked on every render by
  `track_changed`, which stays `true` forever when the effect never re-runs — so
  the bar falls back to the new track's own duration and a zeroed position,
  which is exactly what it shows for the one render *before* the effect fires.
  Defect and fix therefore differ for a single render, and this harness cannot
  sample one: Yew renders a component and runs its effects inside the same
  scheduler pass, which one microtask drains in full (measured hop by hop). See
  the open gap below.

### Not covered

**A single render, and with it the last of `H4`.** `flush()` runs the scheduler
to quiescence, so "rendered but effects not yet run" is not an observable state
here. That window is where the surviving `H4` mutation above lives. Seeing it
needs a real frame boundary — the same purchase as the computed-style work
below, not a separate one.

**Nothing can assert that a panic happened.** On `wasm32-unknown-unknown` there
is no unwinding: `panic = "abort"` is the only strategy, and
`wasm_bindgen::__rt::maybe_catch_unwind` is the identity function without
`panic = "unwind"` (`wasm-bindgen-0.2.128/src/rt/mod.rs:1090`). A panic raised by
a render happens on a microtask, so the process dies with
`RuntimeError: unreachable` and reports no results at all. The harness installs a
panic hook that prints the message before the abort, which is a diagnostic rather
than a detector. This is why the C1 detector — re-inject the defect, watch Yew's
assertion fire — is a **documented manual procedure** in `hook_order.rs`, with
its measured output, instead of a test that could only ever kill the run.

**Computed style — the class of bug that already shipped.** The white album text
came from a `<button>` inheriting `buttonface`, and nothing here can see it.
jsdom has no layout engine, so `getComputedStyle` answers from inline and
stylesheet declarations only and an *inherited* value is not observable. It needs
a real browser, `getComputedStyle`, and a layout engine: a separate harness
(chromedriver or `wasm-bindgen-test`'s in-browser mode) with its own install and
version pinning on three operating systems. **This gap is open, and the DOM shim
does not close it** — it is the stepping stone toward it, which is why it was
worth taking, but it is not the thing itself. The harness is deliberately not
described as covering it.

**Which callback a control is wired to.** The DOM makes this reachable —
`dispatch_event` works under the shim, and `scripts/check-dom-shim.cjs` proves it
— but no test drives a click. Recorded as open.

**Second-render behaviour, beyond the two components above.** `components.rs`
still renders each component once through SSR, which drives an instance a single
time and never runs an effect; two attempts to force a second pass that way are
recorded as dead ends in that file. `render.rs` reaches second renders, but only
for the queue panel and the player bar, and only at `flush()` granularity.

### Three things worth knowing before trusting a green run

**The suite runs on Windows; it could not always, and part of the reason recorded
here was wrong.** The upstream defect is real: `wasm-bindgen-test-runner` 0.2.128
builds its Node script with `Path::display()` inside a *single-quoted* JS string,
so a Windows path became `'C:\Users\...'` and Node rejected the file while
*parsing* it. It is unconditional (the `readFile` sits inside `if (false)`, which
is still parsed) and 0.2.129 does not fix it. Two corrections to the note this
replaces: the invalid escape is **`\ui`**, from the package directory — `\U` is a
`NonEscapeCharacter` to JS, exactly as `\Users` is — and
`WASM_BINDGEN_BENCH_RESULT` **does** help when it is pinned to a *relative* path.
`.cargo/config.toml` now sets it to `target/wbg_benchmark.json`, which keeps the
literal free of backslashes and puts the runner's bug out of reach.

**The worse finding is that the `[env]` table never worked at all.**
`NODE_OPTIONS = { value = "--require ui/tests/dom-shim.cjs", relative = true }`
makes Cargo path-join the **whole value, flag included**, onto the config's base
directory, so Node received `...\Rift\--require ui/tests/dom-shim.cjs` (measured
with a probe crate), silently ignored every token that does not begin with `-`
(measured: exit 0, where a real `--require` to a missing file exits 1), and the
DOM shim never loaded — on any platform, in or out of CI. The four renderer tests
were failing for that reason everywhere the suite ran, and the CI job that would
have run them had never executed at all (the branch was unpushed), so nothing
contradicted the note above. With the bare value in place the suite is **55
passed / 0 failed on Windows** with no shell overrides, and CI now runs the shim
check and the suite on all three operating systems. The "run them under WSL"
advice is withdrawn.

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

**A shared target directory can run another tree's artifact.** With
`CARGO_TARGET_DIR` pointing somewhere a second checkout or worktree also builds
into, `cargo test` can execute a binary compiled from *other* sources: the unit
hash covers the cargo config and the build profile, not the path the sources were
read from, so an artifact from a sibling tree is a legitimate cache hit. A
mutation that was supposed to fail can therefore pass — the most dangerous shape a
verification step can have, because the result looks like evidence rather than a
miss. Every isolated run in this project now uses a private target directory for
that reason, including the frontend suite run that confirmed the 55/55 above.

### The DOM shim: what it cost, and the three traps in it

This was a decision that needed a call rather than a discovery: a renderer-level
mechanism is a dependency, and the standing rule is to ask before adding one. The
options were mirroring Yew's `assemble` macro (a Rust dev-dependency, no DOM,
`C1`'s behaviour only) or a DOM shim driving the real CSR renderer. **The shim
was chosen**, because it is the only one of the two that also moves toward the
computed-style class — the one bug class that actually reached a user.

What it cost: **one Node-side dev-dependency**, `jsdom@26.1.0`, pinned in
`ui/package.json` and `ui/package-lock.json`, plus an `npm install` step in CI's
frontend job and an `npm ci`-able lockfile to cache. Nothing in the Cargo
dependency tree, and nothing in the shipped bundle: `trunk build` never runs
Node. The version was chosen for a `node >= 18` engine floor, so the CI runners'
preinstalled Node cannot fall below it — jsdom 30 requires Node 22+, which would
have made the job depend on the runner image's Node.

Three traps, each of which cost a run to find and each of which is now recorded
at the line that has to stay that way:

- **The shim must define `pre#output`.** `wasm-bindgen-test` picks its output
  formatter by asking whether the global object has a `self` property. A DOM
  shim has one, so the *browser* formatter runs and writes every result line into
  `document.getElementById("output")`. Missing it, the run dies on
  `Cannot read properties of null` before a single test executes.
- **The shim must not shadow `performance`, `crypto` or `fetch`.** jsdom's own
  implementations delegate to the global one, so mirroring jsdom's wrapper over
  Node's built-in makes it call itself: `Maximum call stack size exceeded`,
  before any test executes.
- **`self` must resolve to `undefined`.** Deleting it does not work — jsdom's
  `window` still answers a structural `self` lookup, so the detection reports
  Browser anyway and the run then produces *no output at all*, which in CI is a
  silent hole rather than a red build. The shim keeps the browser formatter and
  mirrors its writes to stdout instead.

The first two are the kind of failure that reads as "the harness is broken" and
sends you looking at the tests; both are now checked in about a second by
`node scripts/check-dom-shim.cjs`, without compiling anything.
