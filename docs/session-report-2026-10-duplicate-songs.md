# Rift — session report: one song under two video ids

> **Complete at `65a5350`.** Two commits are the fix: `173aea8` (the match and
> the load-time repair) and `65a5350` (the hardening a second review pass, run
> before the push, required). This file and
> [`innertube-research.md`](innertube-research.md) §1.9 are the record of it.
> Every number below was measured on the tree it names, locally, before the push
> that carries it.

The report was, verbatim: *"There's a duplicate song bug. If you add a song to
your liked songs from an artist then navigate to the album the song is from it
doesn't say it's liked. We have two versions of the same song 'Around the
world' by daft punk. One from the album doesn't have an artist but a duration.
The one from the artist has the artist but no duration."*

That description was accurate in every detail, including the field split — which
is what pointed at the real cause.

---

## 1. What the bug actually was

**YouTube Music gives one recording more than one video id, and each surface
picks a different upload.** Measured against the live API (2026-10-10, re-checked
2026-10-18) on Daft Punk's *Homework* (`MPREb_dMHztPsrDgN`) and the artist
channel (`UCRr1xG_2WIDs18a6cIiCxeA`), and recorded in full in
[`innertube-research.md`](innertube-research.md) §1.9:

| row | id | kind | carries |
|---|---|---|---|
| album page, "Around the World" | `K0HSD_i2DvA` | official music video | duration 430 s, **no** artist credit |
| artist page → Top songs, same song | `Jb6gcoR266U` | art track | `artist` = Daft Punk, **no** duration |

The album page's row names only `K0HSD_i2DvA` — on the inline play button's
`watchEndpoint`, on `playlistItemData.videoId` *and* on the title run's
`watchEndpoint`. Neither row names the other's id, so **no choice of pointer in
`parse::video_id` could have reconciled them**; this was not a parser defect and
could not be fixed by making the parser smarter. The two rows also disagree
about which fields they carry by design: album rows leave the byline to the page
header (the row's own column is the play count), Top songs rows omit the
duration.

Rift keyed every "do I already have this?" on the video id, in four places:

- the like state (`HashSet<video id>` in the UI, `t.id == track.id` in
  `toggle_like`), so the album row never matched the like the artist row stored;
- the like itself, so liking from the second surface stored a second row;
- Recently played, the playlist add guard and the import dedup;
- credit backfill.

The reporter's `library.json` held the proof: two "Around the World" rows,
`K0HSD_i2DvA` with `duration: 430` and `artist: ""`, and `Jb6gcoR266U` with the
credit and no duration — one song stored as two half-filled rows.

## 2. The fix

| file | what changed |
|---|---|
| `types/src/lib.rs` | `Track::same_song` (the one comparison rule), `Track::title_key`, `Track::fill_missing_from`, and `SongIndex` — id fast path plus a title bucket, so a per-row membership test stays O(1) |
| `src-tauri/src/library.rs` | `fold_song_duplicates` / `collapse_duplicates` (run at load, snapshotting the file first), `toggle_like` (removes *every* copy), `push_recent` (merges the stored row's fields), `add_to_playlist`, `create_playlist_with`, `backfill_track` |
| `src-tauri/src/commands/library.rs` | `add_to_playlist`'s kept-offline pre-check asks the same question as the store, so a refused duplicate can't trigger a stray download |
| `src-tauri/src/player/playback.rs` | credit enrichment fills the plain byline, so it stops storing the `artists`-full/`artist`-empty shape |
| `ui/src/components/track_list.rs`, `ui/src/app.rs` | `TrackListCtx::liked` is an `Rc<SongIndex>` built once per render by `liked_index`; the row's heart and the player bar's heart both ask it |
| `ui/src/tests/*`, `src-tauri/src/player/tests.rs` | the id-set fixtures updated, and renderer/store/App-level tests for the reported flow |
| `docs/innertube-research.md` | §1.9, the platform finding the rule rests on |

### 2.1 The rule, and what it deliberately does not do

`same_song` compares in the order the evidence runs out:

1. the same non-empty id — an empty id is "no id", not one id every corrupt row
   shares;
2. equal titles;
3. credits, when known on both sides, must be **compatible** — they share a
   name, drawn from both credit fields and the byline split into names;
4. two known durations decide: within two seconds is one recording, beyond it is
   two;
5. with a duration missing, the album decides: the same album is the same entry,
   different albums are not guessed at (every album has an "Intro"), and with no
   album on at least one side the credit has to be known on both.

Three refinements came from the pre-push review, and each exists because the
first cut got a real case wrong:

- **Structured credits count.** `enrich_track` used to store the resolved
  credits on `artists` and leave `artist` empty (eight such rows in the live
  library). A rule that read only the byline saw "no credit" there and let a
  cover with the same title and length win on duration.
- **Credits are compared by compatibility, not string equality.** Playlist rows
  link only the primary artist while album pages link the features too ("Lady
  Gaga" vs "Lady Gaga, Colby O'Donis"); exact comparison called one recording
  two, which is the reported bug for the feat.-track class.
- **Durations have a tolerance.** Album rows round and re-uploads pad: a
  one-second difference is not proof of two recordings.

The costs are accepted rather than hidden. A pair that shares only a title and a
length with no credit on either side can still be two creditless album-page
rows, and two recordings that agree to the second are treated as one; both are
documented on the function. The residual false negative is by design: the same
title on two releases with neither length known stays unmatched.

Downloads and the playback queue were **not** changed: the two ids are two
different uploads, an offline copy is one upload's audio file, and treating them
as one would claim audio Rift never fetched.

### 2.2 Loading a library is now a repair

The collapse only merges rows `same_song` is sure about, and it is lossless: the
first row keeps its position and id and takes the artist/cover/album/duration it
was missing from the row it absorbs.

Because this rewrites the user's file and a wrong answer deletes a row, the
repair is bounded and recoverable: the first repair snapshots `library.json` to
`library.json.pre-collapse` before writing, folds `liked` and `recently_played`
alongside the playlists (a duplicate row there came from this same bug), logs
the id pairs it folds at debug, and writes the result once. Every later launch
is a no-op. Playlists are folded too, and that is the most debatable decision in
the change: the rows can only exist because of this bug, the app refuses
same-song adds going forward, and the backup is the undo.

Measured on a copy of the reporter's library (2026-10-10 state): liked 6 → 5,
with the surviving "Around the World" carrying both `artist: "Daft Punk"` and
`duration: 430`; Recently played 30 → 27, the three real duplicate groups
(Around the World across surfaces, Face to Face across surfaces, Veridis Quo
across releases); playlists untouched (99/79/88/16 rows, no same-song pairs in
any of them).

## 3. The review pass before the push

Two independent adversarial reviews ran against the diff before it was pushed —
one on the data layer, one on the frontend and the record. Both were read-only
and re-measured against the live API and the live library. What they found and
what happened to it:

| finding | disposition |
|---|---|
| **High** — the credit veto read only `artist`, so rows with credits only in `artists` were treated as creditless; a same-title/same-length cover could match and `toggle_like`/the load repair would delete the real like | fixed in `65a5350`: candidate names come from both fields; new tests `structured_credits_veto_a_match_the_byline_cannot` and the store-level `loading_does_not_fold_a_cover_whose_credit_is_structural` |
| **High** — exact-string credit comparison vetoed one recording as two when only one surface links the featured artists (the reported bug for feat. tracks) | fixed: `featured_credits_that_only_one_surface_links_do_not_split_a_song` |
| **High** — the load-time repair was a silent, irreversible rewrite with no snapshot; playlists are user-curated | fixed: `library.json.pre-collapse` snapshot + fold logging; playlist folding kept, documented above |
| **Medium** — a one-second duration difference was treated as a different recording; conversely, equal title+length was decisive before any album evidence | fixed: two-second tolerance, and the album rules were reordered so album equality is the strongest structural evidence; `a_second_or_two_of_length_is_still_the_same_recording` |
| **Medium** — the credit-conflict guard had no isolating test (the existing test failed for album reasons, so deleting the guard left the suite green) | fixed: `two_artists_on_the_same_album_entry_are_two_songs` fails when the veto is removed |
| **Medium** — import silently drops rows for one song; the "lossless" import doc no longer says where the fold happens | fixed: import logs the fold; the parser-level lossless test is unchanged because that layer is unchanged |
| **Low** — replaying a song from the other surface replaced a richer Recently-played row with a poorer one | fixed: `push_recent` merges the stored row's fields; `replaying_a_song_from_the_other_surface_keeps_its_fields` |
| **Low** — an empty id acted as an identity | fixed: `an_empty_id_is_not_an_identity`, `loading_does_not_fold_rows_that_only_share_an_empty_id` |
| **Low** — `enrich_track` never set `Track.artist`, the root producer of the half-filled shape | fixed: `byline()` + test |
| **Low** — the docs' `musicVideoType` claim did not reproduce (absent on 2026-10-10, present on 2026-10-18) | fixed: §1.9 now records it as unstable — nothing may depend on it |
| **Low** — the liked index was rebuilt and deep-cloned several times per render, and the App→hearts wiring was the only untested line in the frontend change | fixed: built once, shared as `Rc`, and driven by `the_liked_index_is_built_from_the_liked_songs_by_song` |
| **Low** — the player bar's like button said "Like" while liked, and the row-level accessibility contract is only asserted for rows | fixed: title follows the state |
| **Info** — download identity is still keyed on the video id, so a badge can disagree across surfaces | kept deliberately (above) and documented |

Both reviewers explicitly found no correctness problem in the frontend wiring
itself: every "is this song liked?" decision goes through one `SongIndex`, the
library event rebuilds it before the next render, and there is no optimistic
like state to go stale.

## 4. What was verified

- `cargo fmt --all -- --check`; backend clippy (`--workspace --exclude rift-ui
  --all-targets -D warnings`); frontend clippy (`-p rift-ui --target
  wasm32-unknown-unknown -D warnings`): clean.
- `cargo test --workspace --exclude rift-ui`: **193 passed** — 30 more than the
  tree the fix started from (`16` in `types`, `13` new `library` tests, a
  `player` byline test); `cargo test -p rift-ui --target
  wasm32-unknown-unknown`: **86 passed**, 2 more than before (renderer + App
  wiring).
- CI's guard scripts, run under Git Bash: toolchain consistency,
  wasm-bindgen version, duplicate dependencies — all green.
- **Mutation checks**, because a green test that cannot fail is this repo's
  recurring failure mode: reverting `same_song` to id-only fails 6 `types`
  tests, 7 `library` tests and the new renderer test (with exactly the reported
  symptom: "ibtn" instead of "ibtn liked"); deleting the credit veto fails the
  two credit tests and makes the store test fold a karaoke cover *over* the real
  like; removing the empty-id guard fails its test; reverting `toggle_like` to
  its old id match fails exactly the two like tests. The tree was restored
  byte-identically after each (diff hash compared), and the fix commits were
  made from restored trees only.
- `cargo tauri build --no-bundle` (CI's packaging step): the `trunk build
  --release` hook and the crate compile pass; the final link cannot run locally
  while Rift itself is open, because the running process holds
  `target/release/rift-music.exe` (a local artifact lock, not a build failure —
  the same step is exercised by CI on a clean runner).
- The live library after the fix had been running: 17 likes, no cross-surface
  duplicate in any list, and the one repeated title in Recently played — "How To
  Save A Life" by Clayton Shay and by The Fray, different albums, 217 s vs
  263 s — correctly kept apart. Eight live rows carry their credit only in
  `artists`; the hardening is what keeps those safe.
- The pre-push hygiene pass over the 71 commits already pending: no committed
  binaries, logs, secrets or `dbg!`/`todo!` leftovers; `origin/main` is an
  ancestor, so the push is a fast-forward with nothing to rebase.

## 5. What was deliberately left

- **Download and queue identity** (above), and the offline-copy badges keyed on
  them: an album-page row still does not show as downloaded when only the
  artist-page upload was downloaded, and a load-time merge can leave the merged
  row's id without an offline file (the file remains on disk under the id that
  was merged away). Both are consequences of the documented boundary, not
  regressions.
- **No version bump.** The workspace stays at 0.5.0; the release workflow is
  tag-driven and was not exercised.
- **No migration of the playback snapshot** (`playback.json`): a restored queue
  may contain both ids, which is a queue, not a library.
- The 71 commits already unpushed before this work were left exactly as they
  were; the fix sits on top of them as `173aea8` and `65a5350`.
