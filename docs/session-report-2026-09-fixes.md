# Rift — session report: the September 2026 review, fixed

> **Complete, frozen at `754e4e1`.** The tree that produced the numbers below is
> that revision; the one post-freeze change is a CI-tooling fix to
> `check-duplicate-deps.sh`, described in §3 and in the script itself, and it is
> called out where it matters. `docs/code-review-2026-09.md` remains the
> item-by-item record; this is the session view.

This is the report the prompt's definition of done asks for: what changed, what
was verified, what could not be reproduced, and what was deliberately left. The
work ran under `docs/next-session-prompt.md`; the review it fixes is
`docs/code-review-2026-09.md`, whose per-item notes are the authority on any
single item. Read that document for *why* an item existed; read this one for what
happened when it was fixed.

---

## 1. What changed

**Frozen revision `754e4e1`** — 25 commits on top of `38aea9f`. The review's own
fixes landed in an earlier block (`7df0c81..baa5af8`, recorded item by item in
`docs/code-review-2026-09.md`); this session closed the items that block deferred,
fixed what re-verification found, finished the accessibility work, and wired the
CI. Every claim below is one of those 25 commits or is attributed to the run that
produced it.

One note on the numbers themselves, because it is this session's theme: an interim
count of these commits read thirteen, then fourteen, and settled at 25 — each figure
was right at the moment it was counted and stale by the time it was quoted. The
table below is `git log --oneline 38aea9f..754e4e1`, which is the only form that
cannot drift, and every count in this report names the revision it was measured on.

| Item | Commit(s) | One line |
|---|---|---|
| M9 (deferred half) | `1d2bc94` | the atomic-write temp path is built by construction, not inferred from `with_extension` |
| H3 (second path) | `3051d7c` | `detect_ytdlp`'s probe bounded — the unbounded subprocess wait H3 did not name |
| H5 / Phase 5 | `f19e2ab` | nested-control keys, Tab-unreachable controls, unnamed controls, five silent IPC failures, the stale tab guard |
| C1 + the harness | `49bb626` | the frontend suite actually runs (the DOM shim's `[env]` value never loaded); the hook-order scanner stops crying wolf |
| M10 | `9a5679f` | shuffle history remapped through every queue edit instead of reset |
| M2 | `e867b34`, `754e4e1` | the probe stops resting on a header that is never sent, then on the status at all |
| L7, H1, L12 | `a3a55f1` | per-request regexes cached once; the visitor-pool guards made order-independent; L12's recovery test |
| L10, M14, 3.8 | `9de9f22` | the typed UGC signal enforced where it travels; guards that were only implemented now tested |
| playlist walk | `3f08d4f`, `5db9ca8` | an all-filtered page no longer ends the walk; the walk is bounded by requests, not only by tracks |
| `main.rs` client | `fedb79b` | the failure names its consequence — legibility, not behaviour (it already panicked) |
| M7 | `1fb4397`, `b6f10ab` | the install's verify-then-write ordering pinned, then the shipped entry point and its fetch base driven |
| M3 | `e47986b`, `2a34a3a` | release inseparable from the split; the release announces itself so the UI hears about it |
| `ipc_contract` | `3ba29ab` | the text scanner's blind spot recorded, its command-name rule widened |
| M10 (hardening) | `a213f54`, `1c1caf6` | the property test's invariants corrected, then two real defects fixed and the toggle's prefetch dropped |
| UI round two | `8bca5f2`, `d6f5c73`, `4b2c8d8` | the interaction layer driven; keyboard/nesting/focus finished; reorder gains a keyboard path; the nested-control rule pinned per host |
| docs | `b990c09`, `e122c54` | the audio-filter doc matches its call sites; the album badge rule and the ACL guard recorded |
| library | `02719f9` | playlist import made testable, and its lossless round trip pinned |

### Phase 6 (CI, toolchain, packaging)

Phase 6 is the part this report owns end to end; the other items are summarised
above and recorded at their entry in the review document.

| Phase | Item | Commit | One line |
|---|---|---|---|
| 6.1 (L4) | `epoch` dead code | `7ddd789` | field, its false comment and the discarded `radio:` parameter removed |
| 6.2 (M15) | CI never built the app; release could publish a mislabelled tag | `7ddd789` + this session | `--no-bundle` build, tag↔manifest assertion, tag pushes publish; **the build step had no CLI and could not run until this session** |
| 6.3 (M16) | duplicate majors in one binary | `7ddd789` | direct `reqwest` 0.13; `check-duplicate-deps.sh` as the guard |
| 6.4 (L15) | generated schemas tracked | `f323ed1` | `/src-tauri/gen` untracked and ignored |
| 6.5 (L14) | floating actions, no toolchain pin | `7ddd789`, `736c5b3` | SHAs + version comments, `rust-toolchain.toml` at the declared floor, **guard hole closed this session** |
| 6.7 (L16) | missing package metadata | `7ddd789`, `736c5b3` | workspace `authors`/`repository`/`rust-version`; bundle `publisher`/`copyright`/`licenseFile` |

One provenance caution, on the session's own theme: the `dist/` directory that had
been read as "the built frontend" was a **stale debug build from 2026-10-02** — a
5,256,474-byte wasm — rather than the release output. The release build of the same
crate is ~842 KB (`dist/rift-ui-*.wasm`, 842,332 bytes measured). An artifact that
looked like the thing it was not.

### Defects found in the sessions that followed the review

Six of these corrected statements the review document still makes, and are
recorded at the affected item as well as here.

- **CI's `cargo tauri build --no-bundle` could never have run anywhere.** The
  step invoked a cargo *subcommand* with no `cargo-tauri` binary to resolve: not on
  the hosted runners, not in any manifest, not installed by the job. It would have
  failed on its first line with `error: no such command: tauri` (exit 101). The
  local check that "passed" did so because a developer machine already has
  `cargo-tauri` in `~/.cargo/bin` from an earlier `cargo install`. Fixed:
  `ci.yml` installs a pinned `tauri-cli@2.11.5`.
- **`check-toolchain-consistency.sh` exited 0 with the `toolchain:` input
  deleted** — it compared pins that existed and had nothing to say when one did
  not, which is exactly the `@stable`-with-no-input drift it guards. Fixed by
  counting uses against pins per workflow file.
- **The frontend DOM shim never loaded, on any platform.** `[env]`
  `NODE_OPTIONS = { ..., relative = true }` makes Cargo path-join the whole value,
  flag included, onto the config directory; Node then silently ignored every token
  that did not start with `-`. The four renderer tests were red wherever the suite
  ran, and the CI job that would have carried them had never been pushed.
  Fixed (`49bb626`); 55/55 on Windows; CI now runs it on all three OSes.
- **The hook-order scanner false-positived on a `return` inside a closure**, the
  shape half this codebase's callbacks have. Fixed in `49bb626`.
- **`detect_ytdlp` was a second unbounded subprocess wait behind H3** — the same
  defect on the path H3 did not cover; Settings could show "checking" forever.
  Fixed (`3051d7c`) with a real-sleeper test.
- **UI round two** (`f19e2ab`): the way *into* several controls was still broken
  after Phase 5 — nested-control Enter keys cancelled by the row handler, a
  `display:none` kebab unreachable by Tab, a submenu index that pointed at the
  wrong list, five `api::invoke` rejections discarded, artist/album tab fetches
  with no generation token, and unnamed sliders, switches, inputs and images.

The first and third of those share more than a class: **neither had ever
executed.** The build step could not run for want of a CLI, and the frontend
suite's CI step had never run because the branch carrying it was never pushed.
Two green-looking jobs, no executions between them — the same "verified by
reading, not running" failure the review document opens with, in the one place
(the CI file itself) where a reader is most likely to assume otherwise.

### The one correction that went the wrong way

**M2 (`is_online`) was fixed into a permanent false.** The first fix required an
`x-youtube-client-name` response header; YouTube's web root never sends it (it is
a header Rift *sends*), so the probe said "offline" on a working connection and
kept-offline retries aborted with `offline; giving up on N kept-offline
download(s)` (`src-tauri/src/commands/downloads.rs:267`). Falsified live. The final
rule (`754e4e1`) decides on the **final URL host alone** and ignores the status
entirely — a 403, 429 or 503 from YouTube's own edge is online, because rate
limiting is exactly when the download retry pass runs — with a transport failure the
only "offline" and the indifference pinned by tests that a re-added status gate
fails. Three rounds on one small probe, each defensible and each wrong about what
"online" has to mean to the caller; the review doc carries all three.

### One mechanical change, with no content in it

`docs/code-review-2026-09.md` carried ten literal `U+FFFD` replacement characters
where em dashes were meant — a lossy write somewhere upstream of this session, not
a deliberate choice. They are repaired to `U+2014` in one pass. The diff is exactly
ten three-byte sequences on nine lines (one line held two of them) and nothing
else: same byte length, same line endings, same meaning. It is called out here so
that a reader of the diff does not mistake it for a content edit.

---

## 2. What was verified

### On the frozen revision, run by me

All with a private `CARGO_TARGET_DIR` (§3), on `754e4e1`:

- `cargo test --workspace --exclude rift-ui --locked --no-fail-fast` → **162 passed,
  0 failed**, in the same per-binary breakdown the lead's acceptance reports
  (13 + 90 + 3 + 4 + 37 + 15). That number has a history worth keeping: an
  independent run mid-session read 142 passed / 1 failed, and the failure was the
  player property test whose *invariant* turned out to be the defect (see §3).
- `cargo test -p rift-ui --target wasm32-unknown-unknown --locked` → **84 passed,
  0 failed**, including `tests::render::the_dom_shim_is_loaded`, the renderer tests,
  the hook-order cases, and the interaction-layer tests added at the end.
- `node ui/scripts/check-dom-shim.cjs` → **10 checks, all ok**.
- `.github/scripts/check-toolchain-consistency.sh`, `check-wasm-bindgen-version.sh`
  and `check-duplicate-deps.sh` → exit 0 under Git Bash; the third one's failure
  path now names itself, and all three were exercised red as well as green (§3).
- Both workflows parse as YAML; all 14 `uses:` are SHA-pinned; the release tag
  assertion was extracted from the YAML and run both ways; `git ls-files
  src-tauri/gen` is empty.

### The lead's acceptance run on `754e4e1`, attributed

`cargo fmt --all -- --check` exit 0; `cargo clippy --workspace --exclude rift-ui
--all-targets --locked -- -D warnings` exit 0; `cargo clippy -p rift-ui --target
wasm32-unknown-unknown --locked` exit 0 in both the CI form and with `--all-targets`;
backend **162 passed / 0 failed** (13 + 90 + 3 + 4 + 37 + 15); frontend **84 passed /
0 failed**; the DOM-shim check 10 ok; the three CI guards exit 0 under Git Bash.

### Phase 6, verified rather than read

| Claim | Evidence | Verdict |
|---|---|---|
| CI builds the packaged artifact | `.github/workflows/ci.yml` `Install tauri-cli` + `Build the app (no bundle)` | **was impossible**; fixed and structurally verified |
| Release asserts tag = manifests | `.github/workflows/release.yml` tag step | holds; script extracted from the YAML and run both ways (`v0.5.0` → 0, `v1.0.0` over 0.5.0 → 1) |
| A tag push publishes | `release.yml` `releaseDraft: ${{ github.event_name == 'workflow_dispatch' }}` | holds: push → published, manual run → draft |
| `src-tauri/gen` untracked and ignored | `git ls-files src-tauri/gen` empty; `git ls-tree HEAD` empty; `git check-ignore -v` names `.gitignore` | holds |
| Every action SHA-pinned with its version | 14 `uses:` across both workflows | holds (the three `dtolnay/rust-toolchain` refs are a branch, correctly labelled) |
| Workspace and bundle metadata | `Cargo.toml` `authors`/`repository`/`rust-version`; `tauri.conf.json` `publisher`/`copyright`/`licenseFile` | holds; keys confirmed against the installed `tauri-utils`, licence and icons present |
| The three helper scripts do what their headers claim | each run green and red | holds; one hole found and closed in `check-toolchain-consistency.sh` |

### The Windows release lane, exercised locally

The one bundle question this report originally left open is closed, and answered
from the generated files rather than inferred:

- `bundle.licenseFile` points at the plain-text `LICENSE` (11,558 bytes). Tauri
  **converts** it: the run produced `target/release/wix/LICENSE.rtf` (12,550 bytes),
  and `target/release/wix/x64/main.wxs:48` points `WixUILicenseRtf` at it. NSIS
  takes the other route and points at the plain file
  (`target/release/nsis/x64/installer.nsi:40`, `!define LICENSE`, consumed by
  `MUI_PAGE_LICENSE`). No packaging change is needed, and the non-RTF concern was
  never a risk.
- Both Windows bundles exist, which `targets: "all"` and `mainBinaryName` are
  supposed to produce: `bundle/msi/Rift_0.5.0_x64_en-US.msi` (7,061,504 bytes) and
  `bundle/nsis/Rift_0.5.0_x64-setup.exe` (5,119,744 bytes), with
  `target/release/rift-music.exe` at 18,282,496 bytes. Sizes re-measured for this
  report; a case-insensitive scan of the run's 372-line log for
  `licen|wix|nsis|makensis|candle|light|missing|not found|warning|error` found
  nothing beyond the expected download/run lines and `thiserror` false positives.
- WiX and NSIS were **absent** on the machine, so tauri-cli downloaded and
  hash-validated both toolchains itself — which is also the best evidence available
  locally that the same path works on a clean runner.
- Tag-push equivalence: same matrix entry, same `beforeBuildCommand`, same pinned
  1.88.0 toolchain, same `targets: "all"`, so a `v0.5.0` push would build and
  publish exactly these two installers.

### Guard behaviour, tested by mutation

The tag assertion, the toolchain comparison and the duplicate-dependency baseline
were each exercised in both directions on scratch copies or with a mutated script
against the real workspace: version disagreements fail, a missing `toolchain:`
input fails, a baseline that loses a duplicated crate fails, and a baseline that
names a crate no longer duplicated warns without failing.

### Comment audits: a negative result worth recording

Two sweeps of the comment layer ran alongside the fixes, looking for prose that had
drifted from the code: ui-app audited the frontend and found **17 stale comments**,
ytmusic-client audited the Rust tree and found **four**. Every one was documentation
lag — a description that no longer matched the code it described — and **none was
hiding a behavioural defect**. The negative result is worth as much as a fix: this
session's live bugs were found by *running* things (the property test, the mutation
runs, the CI-fidelity check), not by reading, so the comment layer is not where they
were concealed. One of the Rust four was a test whose *name* still asserted the
corrected M3 claim; the rename was approved rather than left, because a test name is
printed in every failure message and a wrong one teaches the wrong thing each time
it runs.

---

## 3. What could not be reproduced, and what only CI can settle

- **`cargo tauri build --no-bundle` was not re-run in this session** — it is a
  ~4-minute LTO build sharing `./target` with other work. Its inputs are verified
  (the CLI resolves, the config parses under `generate_context!`, an earlier local
  run produced the binary); the step itself is exercised by CI.
- **Nothing on GitHub's runners.** The matrix and the release lane's Linux and
  macOS entries are still out of reach locally; the Windows bundle half has since
  been run — see §2.
- **The retractions stand** (L1, part of L13, the `shuffle_cursor` and
  `fetch_track` non-bugs, the `artist.json` downgrade) — nothing here changed
  them, and the review document remains the record.

### The finding that outlives the bugs: the guard was the defect

Six times this session, the thing that was supposed to make a behaviour safe — a
test, a lock, a scanner, a stated invariant — was itself wrong, or narrower than
its name implied. None was found by reading the code; every one came from trying
to *use* the guard. They are collected here rather than left scattered across six
items because the shared failure mode is the point: a broken guard does not crash,
it returns a green result, so what it damages is confidence.

- **H1's order-dependent guards.** `crates/ytmusic`'s identity tests fail under
  `--test-threads=1` **on unmodified code**: two different locks guard one
  process-global pool, so the exclusion the tests assume is not there. The
  behaviour was fine; the synchronisation meant to make the observation sound was
  not. Fixed at the guard (`a3a55f1`), not by running the tests in parallel.
- **M3's restatement.** The item described a state the code cannot reach — a
  leftover in-flight claim. `Downloads::fail` removes it
  (`src-tauri/src/downloads.rs:88-92`), so a row cannot show the spinner the review
  described. Writing the regression guard is what exposed the false claim; the
  guard now asserts the real end state, "no in-flight claim may be left behind"
  (`src-tauri/src/commands/downloads.rs:441-476`).
- **H2's guard stops at the helper.** `crates/ytmusic/src/context.rs:178-182`
  exercises `resolve_client_version` — where the caching rule lives — and says so:
  `client_version` short-circuits on the process-wide cache before ever reaching
  it. The helper is pinned; the call site that decides whether to reach the helper
  is not.
- **`download_ytdlp` had no coverage at all.** `src-tauri/src/fetch.rs:675-679`:
  "Nothing else in the repo drives `download_ytdlp` at all, so moving the
  verification after `create_dir_all` and the write leaves every other test green —
  this is the guard that fails." The installer's security property is an ordering,
  and it was unpinned until `1fb4397` wrote the guard that fails.
- **`ipc_contract`'s scanner has a recorded blind spot.**
  `src-tauri/tests/ipc_contract.rs:275-284`: it reads the `json!` object *inside*
  the `invoke`/`fire` call, so a site that builds its payload into a local first
  matches nothing and the historical `"radio"` defect could return that way. The
  compensating guard is named (`play_track_sends_only_the_track_argument`);
  `3ba29ab` then widened the scanner's command-name rule after a digits/uppercase
  skip was found in it.
- **The nested-control guard is tag-based.** `ui/src/components/mod.rs:80-89`
  treats `BUTTON | INPUT | SELECT | TEXTAREA | A` as interactive, so a
  `role="menuitem"` DIV slips through and Enter aimed at it is still swallowed by
  the row handler — while `App`'s shortcut handler answers the same question from
  the other side by `tabIndex` (`:77-79`). Two guards, one question, two rules;
  ui-app's round two is closing it.
- **M3's broadcast is unpinned (in flight).** The verifier's last attack: removing
  the caller's `emit_downloads` leaves the suite green — the store updates and the
  UI is never told, which is half of what M3 was about.
- **M7's wrapper is unpinned (in flight).** Repointing `download_ytdlp` at a bogus
  base likewise leaves the suite green: `1fb4397` pins the ordering *inside* the
  installer, not the wrapper that decides where it fetches from. Both are assigned
  back to `backend-core`, so they close with commits or are recorded here as
  deliberate gaps by the freeze.
- **The property test's invariant was the defect — and fixing it found two real
  bugs.** Its first failure ("the cursor must name the playing track") was a false
  alarm: between a prefetch and the play that consumes the pin, the cursor
  deliberately names the reserved track while `current` is still the previous one
  (`src-tauri/src/player/tests.rs:1133`). The invariant was replaced by three true
  ones plus a strong form asserted where it must hold, with the permitted states
  written at the assertion rather than the check relaxed — and with those, 8 000
  steps over four fixed seeds (`tests.rs:1243-1251`) found two genuine defects in
  the shuffle bookkeeping (`a213f54`). The direction is the opposite of every other
  entry here — a guard too strong rather than too narrow — and the remedy is
  identical: make the guard say exactly what is true, and it starts finding what is
  not.

Two of these are *recorded* gaps rather than closed ones — the scanner's blind spot
and H2's call site — and the two verifier findings at the end of the list are still
to be closed or recorded. That is the honest shape, and it is the one worth
carrying forward: a guard whose coverage is narrower than its name, written down
where the next reader will find it, is worth more than a guard that claims the
whole property and silently holds half of it.

One more instance, one level up from the code — **the review's own citations aged
out.** M10's entry cited `queue.rs:66, :158, :165, :196` for `remap_history`'s call
sites; after three hardening rounds (`9a5679f` → `a213f54` → `1c1caf6`) they sit at
`:73, :165, :172, :203`. The citations were accurate when they were made and became
wrong as the thing they described was fixed — the same failure mode as the stale
comments the two comment audits swept for, one level up. A reference that points at
the wrong line is worse than no reference, because it is trusted, so that entry now
names the function instead of numbering its callers and says why.

### The mutation procedure is part of the verification surface

This is a separate finding, not a sub-case of the one above. The guard list is
about *coverage* — a check that does not check what its name claims — while these
two are about the *procedure* that produces the evidence. They share a symptom (a
green result that means nothing) and not a cause, and the first of them has nothing
to do with this repository's tooling at all.

- **A restore that preserves `LastWriteTime` is indistinguishable from "nothing
  changed".** ui-app's confirmation pass read 79 passed / 2 failed, with a DOM dump
  that contradicted the source. The mtimes were the explanation: the wasm artifact
  (15:04:45) was newer than the sources (15:04:40), so cargo considered it fresh and
  served the **mutated** binary. The mutation run's snapshot/restore used
  `Copy-Item`, which preserves mtime, so putting the file back made it look *older*
  than the artifact compiled from the mutated version. Touching the file — mtime
  only, no content change — and re-running gave 82 passed / 0 failed with the source
  unmodified. Every build system, not just cargo, reads that signal the same way, so
  a perfectly private target directory would not have prevented it.
- **A shared `CARGO_TARGET_DIR` can execute another tree's artifact.** The unit hash
  covers the cargo config and the build profile, not the path the sources were read
  from, so two checkouts building into one directory can serve each other's
  binaries: a mutation run can "pass" without running the mutated code, or fail
  against a change it never saw.
- **A guard invocation that failed silently because of *how* it was run.** The lead
  invoked `check-duplicate-deps.sh` from PowerShell, where `bash` resolves to WSL's —
  which has no `cargo`. With `set -euo pipefail` and cargo's stderr sent to
  `/dev/null`, the pipeline died and the script exited 1 having printed **nothing**.
  The check itself was fine; the way it was launched was not, and the result is
  indistinguishable from a guard that never ran. Fixed in this report's scope (the
  one post-freeze source change, CI tooling): the `cargo tree` output is captured,
  its status checked, and a failure now prints `::error::` lines naming the command,
  its exit status, and the trap (Git Bash, not WSL's bash). Verified green under Git
  Bash, red with a named error where no workspace resolves, and still red for the
  dependency regression it exists to catch.
  - One footnote that belongs with it: the first version of that fix printed
    "failed (exit 0)", because `$?` inside `if ! cmd` is the status of the *negation*
    rather than of the command. Running the failure path is what caught it — a new
    error path is new code and needs its own test, which is why the wrong version is
    recorded here too.

**A mutation result is only evidence if the mutated source was compiled, and the
procedure that reverts it must make that provable.** These three broke that rule
from different ends — one by never compiling the mutation, one by making the
reversion invisible, one by never reaching the check at all — and in every case "the
mutation went red" and "the restore went green" were equally untrustworthy while
looking exactly like evidence. The mitigation this session adopted (a private
`CARGO_TARGET_DIR`, §5) addresses the second; the first needs the restore step
itself to leave a provable change signal, and the third needs the guard to fail
loudly when it cannot run.

---

## 4. What was deliberately left

- **The computed-style gap.** jsdom has no layout engine, so an inherited value
  like the white album text is not observable in this harness; it needs a real
  browser. The DOM shim is the step toward it, not the thing itself, and ui/README.md
  says so rather than implying coverage.
- **The launch-time update probe stays silent on failure** — a background probe on
  a possibly-offline launch is the common case, and a toast per launch is worse.
- **Two guard gaps are recorded rather than closed**, deliberately: the
  `ipc_contract` text scanner cannot see a payload built into a local
  (`src-tauri/tests/ipc_contract.rs:275-284`), and H2's regression guard stops at the
  helper while the call site short-circuits before it
  (`crates/ytmusic/src/context.rs:178-182`). §3 argues that a gap which says so is
  worth more than a guard that claims the whole property.
- **The shuffle fairness quirk** — a pick dropped without playing stays in the
  played set, so that track waits for the next cycle. The history cannot distinguish
  a reservation from a genuine play, and un-recording it would corrupt the replay
  window; documented rather than hidden, and now a fairness question rather than a
  safety one since `a213f54` fixed the rollover defect beside it.
- **The nested-interactive-element container rewrite — ui-app's call, quoted
  verbatim from their analysis:**
  > Cost — a rewrite of the four containers the whole app is built from (`.trow`,
  > `.qrow`, `.queue-head`, the sidebar playlist row), including per-row stacking,
  > hover-reveal and focus-ring CSS, a full-card overlay that must not swallow the
  > drag handle, and a fresh visual review of the densest views; buy — nothing a
  > user can observe today, since the nesting is valid HTML and every control in
  > those rows is keyboard-complete and now pinned by tests, only ARIA conformance
  > for screen readers that flatten a `role="button"` subtree; so it is worth doing
  > only as a standalone change with before/after screenshots and the interaction
  > tests re-run, never folded into an accessibility fix near a freeze.

---

## 5. How to re-derive this

```sh
cargo fmt --all -- --check
cargo clippy --workspace --exclude rift-ui --all-targets --locked -- -D warnings
cargo clippy -p rift-ui --target wasm32-unknown-unknown --locked -- -D warnings
cargo test --workspace --exclude rift-ui --locked
cargo test -p rift-ui --target wasm32-unknown-unknown --locked   # needs ui/node_modules
.github/scripts/check-toolchain-consistency.sh
.github/scripts/check-wasm-bindgen-version.sh
.github/scripts/check-duplicate-deps.sh
```

Run these with a **private `CARGO_TARGET_DIR`** (a fresh directory per run). That is
the mitigation for the cross-tree half of the method finding above, not the finding
itself. It does not cover the other half: a procedure that mutates a file and
restores it with `Copy-Item` must leave a provable change signal on restore, or the
build system will reasonably conclude that nothing changed. The frontend suite needs
`npm ci` in `ui/` and the DOM shim loaded through `.cargo/config.toml`; if the shim
is not loading, `node ui/scripts/check-dom-shim.cjs` says so in a second. Everything
the phases changed is marked in `docs/code-review-2026-09.md` at the item it belongs
to, with the commit that did it.
