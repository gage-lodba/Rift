# Fix the defects found in the September 2026 code review

> **COMPLETED — this prompt has been run.** Product source is frozen at `754e4e1`;
> the session's head is `4005a4e`, 28 commits on top of `38aea9f`. The session's
> report is [`docs/session-report-2026-09-fixes.md`](session-report-2026-09-fixes.md):
> what changed, what was verified and by which run, what could not be reproduced, and
> what was deliberately left. The per-item record stays in
> [`docs/code-review-2026-09.md`](code-review-2026-09.md), which is the authority on
> any single item. The text below is the prompt exactly as it was issued — nothing
> rewritten — with two of its items (1.4 and 1.5) carrying corrections inline,
> because re-verification found the defects they described were not what the code
> actually did.

Paste this whole file as the prompt for a new session working in this repo.

---

## Context

You are working in `Rift` — a Tauri 2 + Yew (Rust/WASM) desktop YouTube Music
player. Workspace members: `types`, `ui` (Yew/Trunk frontend), `src-tauri`
(binary `rift-music` + library `rift`), `crates/ytmusic` (a clean-room
innertube metadata client). Playback shells out to `yt-dlp`.

A full code review was just completed: **`docs/code-review-2026-09.md`**. Read it
first — it is the source of truth for this session, with file:line references,
failure modes and suggested fixes for every item. This prompt sequences that
work; the review document explains it.

**Read before you edit:** `docs/code-review-2026-09.md`, then the specific file
for each item.

## Ground truth to establish first (do not skip)

The committed tree was clean at review time. Reproduce that baseline before
changing anything, so that any later failure is provably yours:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --exclude rift-ui --all-targets --locked -- -D warnings
cargo clippy -p rift-ui --target wasm32-unknown-unknown --locked -- -D warnings
cargo test --workspace --exclude rift-ui --locked
```

Expected: all clean, **65 tests passing**. The `ui` clippy pass needs the
`wasm32-unknown-unknown` target installed; the repo is set up for it.

Also note: **5 files are currently uncommitted** (`src-tauri/src/commands/playback.rs`,
`player/{mod,nav,playback,tests}.rs`). That is an in-flight feature — an extracted
`cycle_repeat`, a new `seed_shuffle_anchor`, and their tests. It is reviewed and
correct. **Do not revert, "tidy", or fold it into your work**; leave the diff
intact and rebase your changes on top of it.

## Two rules that matter more than speed

1. **Verify, don't trust.** Everything in the review was derived from reading
   code plus CLI checks — no test drives the Yew renderer, and the review's
   frontend/CI sections are less battle-tested than its backend ones. Before
   fixing an item, confirm the defect exists as described. Two claims were
   already retracted during review (a `shuffle_cursor` out-of-bounds panic and a
   `fetch_track` offline dedup bug — both correctly guarded), and one was
   downgraded (`artist.json` has no "Singles & EPs" shelf). **If you cannot
   reproduce an item, say so and skip it** rather than making a speculative
   change. Report any item you find to be wrong.
2. **Every fix lands with a test that fails without it.** This repo's existing
   tests are genuinely good (they assert observable ordering, not internals) —
   match that bar. Where a test is impractical (UI, network), say why in the
   commit message instead of silently skipping it.

Work in the phase order below. Commit per phase with a conventional-commit
subject line, as this history already uses (`fix:`, `feat:`, `chore:`, `docs:`).

---

## Phase 0 — P0: the app-breaking bug, plus two permanent-wedge bugs

These three are the highest-value changes in the whole review.

**0.1 — `use_state` called after an early return (CRITICAL, breaks the app)**
`ui/src/components/queue.rs:21-30`. The first render has an empty queue, returns
at line 29, and never reaches the `use_state` inside the `toggle` callback at
line 31. As soon as the queue becomes non-empty, Yew's hook-counter assertion
fires ("Hooks are called conditionally") in debug; in release the collapse state
misbinds into the wrong hook slot. Fix by hoisting the callback above the early
return. Re-check every other `#[function_component]` for the same shape (grep for
`return html!` and confirm no hook is called after it) — this class of bug is
easy to reintroduce.

**0.2 — No HTTP timeouts anywhere (HIGH)**
`src-tauri/src/main.rs:111` and `crates/ytmusic/src/client.rs:35-39`. Both
`reqwest::Client`s use defaults; reqwest's default is *no timeout*, so a stalled
socket wedges the Settings yt-dlp probe, the update check, and every metadata
command forever — and permanently leaks a download claim so that track can never
be retried. Add `.connect_timeout(5s).timeout(30s)` to both builders, and bound
the yt-dlp subprocess wait in `src-tauri/src/fetch.rs:96` with
`tokio::time::timeout`.

**0.3 — The fallback client version is cached permanently (HIGH)**
`crates/ytmusic/src/context.rs:15, 44-49`. One transient failure of the first
home-page scrape pins `FALLBACK_VERSION` ("1.20240101.01.00") into a `OnceLock`
that is never invalidated; innertube then rejects every request with 400 (and
`client.rs:186` retries only 403), bricking all metadata until restart. Only
write the cache on a successful scrape; keep the fallback per-call.

Verify after this phase: full command block above, still 65+ tests green.

---

## Phase 1 — correctness of the ytmusic client and the progress bar

**1.1 — A banned visitor id is immediately reused (HIGH)**
`crates/ytmusic/src/identity.rs:25-35` with `client.rs:186-189`. `ban()` inserts
into `banned` but never evicts from `ids`, so the retry re-selects the id it just
banned and fails identically — the first 403 disables search/artist/album/
playlist-import for the life of the process. Drop the id on ban. Add a unit test
asserting that `visitor_data` after `ban(id)` does not return `id`.

**1.2 — `PlayerBar`'s live progress survives a track change (HIGH)**
`ui/src/components/player_bar.rs:36, 43-46, 107, 115-119`. `props.progress` is
the bootstrap seed and never changes afterwards (`App` deliberately does not
subscribe to `PROGRESS`), and `live` is never reset on track change — so the
previous track's elapsed time and slider `max` persist for the whole fetch window.
Key the seeding effect on track identity too, falling back to `track.duration`
while loading.

**1.3 — `DiscordIpcClient::recv()` is unbounded (MEDIUM)**
`src-tauri/src/discord.rs:325`. Blocks forever on a socket with no timeout, on a
dedicated thread; the loop's coalescing and heartbeat then never run and the
unbounded channel grows for the session. Wrap in `tokio::time::timeout` and drop
`connected` on expiry.

**1.4 — `is_online` reports online when it can't reach the network (MEDIUM)**
`src-tauri/src/util.rs:101-110`. `.unwrap_or(false)` only covers transport
errors; a captive portal answers with a 200/302/403, so the function meant to
*skip* pointless retries instead burns the full ~7.5-minute backoff ladder. Treat
a non-success status as offline.

> **CORRECTED in the session that ran this prompt.** The diagnosis stands — a
> non-success status is not offline — but the fix built on it was wrong in the
> other direction: it also required an `x-youtube-client-name` response header,
> which YouTube's web root never sends (it is a header Rift *sends*), so
> `is_online()` became permanently `false` and kept-offline retries were aborted
> on a working connection. The live measurement, the consequence and the re-fix
> are under **M2** in `docs/code-review-2026-09.md`; read that before touching
> this item.

**1.5 — In-flight download claims leak on the retry path (MEDIUM)**
`src-tauri/src/commands/downloads.rs:264-272`. Tracks dropped by the
`kept_offline` filter are abandoned without `mark_failed`, so the row shows a
spurious "downloading" state and gets no retry affordance. Mark them failed and
`emit_downloads`.

> **CORRECTED in the session that ran this prompt.** There is no leaked claim to
> leak: `Downloads::fail` (`src-tauri/src/downloads.rs:88-92`) *removes* the
> in-flight claim, so the row cannot show a spurious spinner. The real defect is
> the absence of any state — no claim, no offline copy, no failed mark — so the row
> offers nothing to click and nothing revisits the track. The diagnosis above is
> right that `mark_failed` is missing; it is wrong about what the row shows. See
> **M3** in `docs/code-review-2026-09.md` for the correction and the fix's final
> shape.

---

## Phase 2 — playback pipeline correctness

**2.1 — Decode before you clear (MEDIUM)**
`src-tauri/src/player/playback.rs:85` sets `state = Playing` before the audio
thread has decoded anything, and `audio.rs:108` calls `player.clear()` *before*
`decode(data)` — so a decode failure leaves the UI reporting Playing with an
advancing bar over silence until the ~250 ms tick notices. Decode first, then
`clear()` + `append` only on success. The `Crossfade` arm already does this
correctly; make `Play` match it.

**2.2 — `AudioCmd::Volume` ignores in-flight fade-outs (LOW)**
`src-tauri/src/audio.rs:183-186, 199`. The `fading` vec keeps ramping from the
old `volume`, so dragging the slider during a crossfade gives an audible level
mismatch. Scale the fade using the current master volume.

**2.3 — Persisting the queue blocks the event loop (MEDIUM)**
`src-tauri/src/player/playback.rs:195`, `state.rs:101-117`. `emit_queue`
clones every `Track`, serialises, and queues a disk write — twice on a track's
first play (once for the change, once for credit enrichment) and on every queue
mutation. With `MAX_FETCH_TRACKS = 1000` that is a large clone plus a write burst
from a task sharing the runtime with IPC. Debounce, or skip the write when the
snapshot is unchanged.

**2.4 — Offline playback returns `duration: 0.0` (LOW)**
`src-tauri/src/fetch.rs:51-57`, used at `playback.rs:57`. The metadata guess
overwrites the real duration until the decoder corrects it, so the seek bar
jumps on every offline track. Return the known duration where available (e.g.
probe cheaply, or don't overwrite a non-zero `core.duration` with 0).

---

## Phase 3 — the ytmusic parser

These are all in `crates/ytmusic/src/parse.rs` and are the kind of change that
needs fixture-backed tests. Note the existing fixtures cover only happy paths.

- **3.1 Unlinked rows get the whole subtitle as the artist** (`:223-228`): an
  unlinked search row yields `Track.artist == "Song • 3:22"`. This is currently
  *enshrined* by `tests/fixtures.rs:94-102` — **update that test**, don't leave
  it asserting the defect. Drop the leading type badge and trailing duration run.
- **3.2 `find_all(..).first()` is alphabetical, not document order** (`:11-28`,
  call sites `:395, 455, 472-478, 590-595, 611`): `serde_json` is built without
  `preserve_order`, so `Value::Object` is a `BTreeMap`. Read the known paths
  directly and keep `find_all` as a fallback. Critically, `track_credits`
  (`:608-638`) takes the first `longBylineText` **without checking it belongs to
  the requested `videoId`** — so a substituted or unavailable video silently
  yields another song's artists and album. Match the video id.
- **3.3 `parse_duration` drops bad segments and can overflow** (`:174-179`):
  `"1:2x:03"` becomes 63 s, and the unchecked `u32` arithmetic panics in debug /
  wraps in release. Parse every component with `?`, use
  `checked_mul`/`checked_add`.
- **3.4 `?` inside a reverse loop aborts the scan** (`:319`, `:362`): one run
  lacking a string `text` drops a present year or subscriber count entirely.
  Use `let Some(..) else { continue }` to match `duration` at `:279`.
- **3.5 `album_type` is an exact match on a server-controlled title** (`:526-529`):
  `"Singles" => "Single", _ => "Album"`. The fixture has no Singles shelf at all,
  so nothing pins this. Use `starts_with`.
- **3.6 `tracks_playlist_id` takes any `VL…` id** (`:494-508`), not the top-songs
  shelf's — a layout change makes "Show all songs" load the wrong list silently.
- **3.7 `parse_count` truncates** (`:169`): use `.round()`.
- **3.8 Silent degradation**: when a response contains
  `musicResponsiveListItemRenderer` nodes but the parser extracts none, log a
  warning. Today a renderer rename looks identical to "no results".

**Add the missing robustness tests** (review item L17): a malformed-input table
asserting empty results and no panic for missing `flexColumns`, `null` titles,
wrong JSON types, empty arrays, a "no results" body, and an error/consent body.
Also note `examples/prune_fixture.rs:415-436` only proves the compacted document
parses identically *to itself* — worth a comment, since it reads like a
correctness proof.

---

## Phase 4 — error surfacing and the IPC surface

- **4.1 `api::fire` swallows errors on 41 of 55 IPC calls** (`ui/src/api.rs:33-39`):
  give it an error path into the existing toast mechanism (`show_toast` +
  `events::ERROR`). Concretely today: clicking play on an empty collection does
  nothing; `create_playlist` with a blank name silently closes the dialog.
- **4.2 Search failures look like empty results** (`ui/src/app.rs:473-480`): route
  through `show_toast` like the neighbouring navigation paths (`:383`).
- **4.3 Clearing the search box leaves stale results** (`ui/src/components/titlebar.rs:60`):
  the early return on an empty query means the old results stay rendered under
  `Results for "<old query>"` (`app.rs:979`).
- **4.4 `withGlobalTauri` + app commands outside the ACL** (`tauri.conf.json:19`,
  `build.rs:1-3`, `commands/ytdlp.rs:21-32`): the webview can point Rift at an
  arbitrary local binary, which `fetch.rs:228-243` accepts on `p.is_file()` alone
  and `fetch.rs:80,136` then spawns, persisting the path. There is no XSS path
  today (tight CSP, bundled assets only), so this is defense-in-depth: declare an
  app manifest so the commands are explicitly granted, and validate the override
  (canonicalize; require a file chosen through the existing native dialog).
- **4.5 The one-click yt-dlp installer executes an unverified download**
  (`fetch.rs:174-212`): `releases/latest/download/…`, written, `chmod 0755`'d,
  spawned — no checksum, no pinned version. Fetch `SHA2-256SUMS` from the same
  release and verify before writing. This one changes install behaviour, so keep
  the failure path user-legible.

---

## Phase 5 — accessibility

Treat this as real work, not polish: the app is currently unusable without a
mouse for several features.

- **Menus are unreachable by keyboard and can't be dismissed** (`menu.rs:53-66, 75`):
  no `onkeydown`, no `role="menu"`/`menuitem`, no `tabindex`, no focus trap, no
  focus restore. Items are `div`s so they aren't focusable at all. Add Escape
  handling, roles, `tabindex`, and focus the first item on open.
- **Clickable `div`s with no keyboard path** (`track_list.rs:309-312`,
  `sidebar.rs:227, 230, 274, 296, 301, 309`, `home.rs:29, 71`, `grids.rs:37, 55`).
  The sidebar resizer is `onmousedown`-only. Make these `<button>`/`<a>` where
  possible, or add `role`/`tabindex`/`onkeydown`.
- **Focus is invisible**: `styles.css:97, 179, 353, 988` set `outline: none` and
  there is no `:focus-visible` rule anywhere. Replace the blanket suppression
  with a `:focus-visible` style.
- **Sliders have no accessible name** (`player_bar.rs:115, 155`): add
  `aria-label` for seek and volume.

---

## Phase 6 — cleanup, CI, packaging

- **6.1 `epoch` is dead code with a false comment** (`src-tauri/src/player/state.rs:36-37`):
  incremented in three places, read nowhere; its doc claims "stale radio fills
  check it" but the radio feature was removed (as `commands/playback.rs:41-42`
  says). Delete the field, and `play_track`'s discarded `radio: bool` parameter.
  Re-check whether `queue_add_tracks` (`main.rs:150`) still has any caller.
- **6.2 CI never builds or bundles the app** (`.github/workflows/ci.yml`): clippy
  links the crate, so a broken `tauri.conf.json` *is* caught, but the bundler
  stage (`"targets": "all"`, icon set, `mainBinaryName`, NSIS/WiX) is only
  exercised on a tag push — which publishes a draft. Add a
  `cargo tauri build --no-bundle` job. Two related release gaps: nothing asserts
  the pushed tag matches the manifest version (so `v1.0.0` over 0.5.0 manifests
  publishes mislabelled installers *and* makes `check_update` report a phantom
  update forever), and the updater queries `releases/latest` while the workflow
  only ever creates **drafts** — an endpoint that excludes them, so the update
  banner can never fire.
- **6.3 Duplicate majors in one binary** (`Cargo.lock` vs `src-tauri/Cargo.toml:23,26,29`):
  `reqwest` 0.12 **and** 0.13, `rand` 0.8 **and** 0.10, `zbus` 3 **and** 5,
  `thiserror` 1 **and** 2, plus both `native-tls`/`openssl` and `rustls`. Bump the
  direct `reqwest` to 0.13 to dedupe and add `cargo tree -d` as a CI guard.
- **6.4 Generated schemas are tracked** (`src-tauri/gen/schemas/*.json`, ~440 KB)
  and not ignored, unlike upstream's template. `git rm --cached -r src-tauri/gen`
  and ignore it.
- **6.5 Pin the toolchain and the actions**: `dtolnay/rust-toolchain@stable` with
  no `toolchain:` input and no `rust-toolchain.toml` lets CI and release drift;
  the release job holds `contents: write` while every action floats on a mutable
  tag. Pin an explicit toolchain and SHA-pin the actions.
- **6.6 `util.rs` has zero tests** — the code guarding the user's library
  (`load_json`, `save_json`, `atomic_write`, `Persister`, incl. the corrupt-file
  backup path). Add a round-trip test. While there, tighten
  `atomic_write`'s `path.with_extension("tmp")` (`util.rs:92-96`): it is correct
  by accident (lexical, directory-preserving) rather than by construction.
- **6.7 Package metadata**: no workspace `authors`/`repository`/`rust-version`,
  no `bundle.publisher`/`copyright`/`licenseFile`.

---

## Guardrails

- **Don't expand scope.** The review's "Checked and correct" section lists what is
  already right — shuffle arithmetic, stale-response generations, the IPC
  contract, lock discipline, the CSP and capability set. Leave those alone.
- **Don't add dependencies** to fix these items; each has a std/`tokio`/existing-crate
  answer. If you believe one genuinely needs a new crate, ask first.
- **Don't touch the uncommitted player diff** beyond building on top of it.
- **Preserve the documentation culture.** This codebase's comments explain *why*
  (the shuffle/crossfade rationale is the best example). Fix any comment your
  change invalidates — `state.rs:36` is a known offender.
- **Don't rewrite `parse.rs` wholesale.** Its tolerance is deliberate and correct;
  fix the specific defects.
- If a phase reveals a deeper problem than the review described, stop and report
  it rather than silently redesigning around it.

## Definition of done

1. The full verification block passes, with the test count strictly higher than
   65 and no test asserting behaviour you changed.
2. A focused test exists for every behavioural fix, or the commit message
   explains why one isn't practical.
3. `docs/code-review-2026-09.md` is updated to mark each item fixed / retracted /
   deferred with a one-line reason — the review is a living document, and its
   retractions so far (two false positives, one downgrade) are as valuable as its
   findings.
4. A short final report: what changed, what you verified, what you could not
   reproduce, and what you deliberately left.
