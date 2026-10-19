# Controller notes — backend bridge, event pump, cover art, page generations

Owner: `controller`. Files: `crates/rift-desktop/src/controller.rs`, `crates/rift-desktop/src/cover.rs`,
`crates/rift-desktop/src/state.rs` (additions only), `crates/rift-desktop/tests/ui_flow.rs` (headless
interaction suite, §14), this document. `app.rs` / `main.rs` stay Lead-owned; the
controller is wired by the Lead through the contract in §2.

Status: controller/cover/state compile with and without the `backend` feature
(`cargo check -p rift-desktop`, `cargo check -p rift-desktop --features backend`);
the whole crate suite is green (§13) and the headless interaction tests pass in
both configurations (§14).

---

## 1. What the controller owns

* Construction/ownership of `rift::RiftBackend` (and therefore its dedicated
  Tokio runtime) on the UI thread, exactly once, after the window exists
  (`Controller::new(hwnd)`). `RiftBackend` is not `Clone`; every spawned task
  gets a `BackendHandle` clone instead, so the runtime is always dropped on the
  UI thread.
* `bootstrap()` — synchronous `rift_types::Bootstrap` before the first frame
  (no event race), plus the launch-time update check the legacy web app ran from
  its bootstrap effect when `update_notifications` was enabled.
* `poll()` — drains both the backend event bus and the async-completion channel
  without blocking. Called once per frame, before layout.
* `dispatch(Action)` — full Action→backend-command translation (§4).
* `navigate_view(&View)` — page-load request bookkeeping (§6).
* `cancel_search()` — clear-box invalidation (§6).
* `request_cover(url)` — cover-art fetch/decode/dedupe pipeline (§7).
* `check_update()` / `check_ytdlp()` — Settings buttons.
* `shutdown()` — drop the event bus, then `RiftBackend::shutdown()` (stops
  audio/media/Discord threads and the runtime).

Without the `backend` feature the crate still builds: the controller is created
with `new_preview()` (an inert command implementation, no threads, no network)
and `new()` returns an explanatory error, which `main.rs` handles by falling
back to the preview controller with a warning toast. Public signatures are
identical in both configurations.

### Seam: `BackendCommands`

All commands go through a trait (`BackendCommands`), so the mapping is testable
without a live backend, a window or a network:

* `live::LiveCommands` — production implementation over `rift::BackendHandle`.
* `InertCommands` — preview/screenshot mode; accepts and ignores everything.
* test `RecordingCommands` — records a typed `Call` per command and can inject
  async completions, used by the tests in `controller.rs`.

---

## 2. What `app.rs` calls (integration contract)

Already true today (unchanged public surface): `Controller::new_preview()`,
`Controller::new(Option<isize>)`, `is_live()`, `bootstrap()`, `poll()`,
`dispatch(Action)`, `request_cover(String)`, `check_update()`, `check_ytdlp()`,
`shutdown()`. `app.tick()` keeps doing:

```rust
for event in self.controller.poll() { self.apply_controller_event(event); }
let requests = std::mem::take(&mut self.state.cover_requests);
for url in requests { self.controller.request_cover(url); }
```

Additions the Lead integrates in `app.rs` (§8 has the full apply code):

1. **Page loads** — `App::navigate()` calls
   `self.controller.navigate_view(&view)` after setting `state.view`. This
   issues `get_artist` / `get_artist_songs` / `get_album` for page views, and
   bumps the page generation on every navigation so a reply for a view the user
   has left is dropped (the old `page_gen` rule). `Action::RetryPage` falls
   through the `other => controller.dispatch(other)` arm and re-issues the last
   page request.
2. **Search sub-tabs** — the `SetSearchTab(tab)` arm forwards
   `self.controller.dispatch(Action::SetSearchTab(tab))`; the controller lazily
   fetches artists/albums once per submitted query (old UI behaviour) and clears
   those flags on every new search.
3. **Clearing the search box** — when the query becomes empty,
   `self.controller.cancel_search()` bumps the search generation so a late reply
   for the cleared query can't repopulate the results (old `on_search_clear`).
4. **New `ControllerEvent` arms** — during drain (§8).
5. **Launch update banner** — the existing `UpdateStatus` arm should also set
   `state.update_banner = Some(status.clone())` when `status.update_available`,
   as the old app did from its launch check. The controller already triggers the
   check itself after bootstrap, so no extra call is needed.

`Action`s that `app.rs` applies locally and never needs to forward to the
controller: `Navigate`, `SearchQueryChanged` (except the empty-box
`cancel_search` above), `ToggleQueuePanel`/`ToggleQueueCollapsed`,
`BeginRenamePlaylist`/`BeginNewPlaylist`/`PlaylistDraftChanged`/
`ClosePlaylistEditor`, `DismissUpdateBanner`, `Toast`/`ToastError`,
`Window*` (winit), `SetOnline` (test hook). The controller also ignores them if
they arrive; an Action is translated exactly once.

---

## 3. Threading model

| Where | What |
|---|---|
| UI thread | `Controller::new` (backend construction), `bootstrap()`, `poll()`, `dispatch()` sync commands, `navigate_view`, `request_cover` de-dupe bookkeeping, `shutdown()` |
| Backend runtime (`rift-backend`, owned by `RiftBackend`) | every `async` command future: `play_tracks`, `play_track`, `search*`, `get_artist*`, `get_album`, `import_yt_playlist`, `check_update`, `check_ytdlp`, `set_yt_dlp_path`, `download_ytdlp`, `fetch_image` (cover fetch), `is_online` (reachability probe) |
| Backend blocking pool | cover-art decode + downscale (`spawn_blocking`), and the backend's own `rfd` dialogs / file IO |
| Backend threads | audio (`rift-audio`), media keys (`rift-media`), Discord (`rift-discord`), playback-snapshot persistence (`rift-persist`), durable `library.json`/`settings.json` writes (`rift-durable`) |

The UI thread never awaits a future and never does network/CPU-heavy work. The
event bus is unbounded, so `poll()` drains everything queued each frame
(`try_recv` loop); a stalled consumer would grow memory, exactly as
`backend-api.md` §9 documents.

Async completions travel through a controller-owned `std::sync::mpsc` channel
of private `TaskResult` values. `poll()` maps them to public
`ControllerEvent`s only after applying the stale-generation and cover-dedupe
bookkeeping, so `app.rs` can apply what it receives unconditionally for search
and knows it is current.

---

## 4. Action → backend command mapping

All 46 commands the old desktop shell registered are covered (the migration brief
said 55; `backend-api.md` §7 checked the checked-in `invoke_handler` and the
generated permission set: 46 registered, 43 preserved 1:1 as methods, 3 window
commands moved to winit). The table is exhaustive for `Action`:

### 4.1 Playback / transport / settings

| `Action` | Backend call | Kind | Notes |
|---|---|---|---|
| `PlayTracks { tracks, start, source }` | `play_tracks(tracks, start, source)` | async → `TaskResult::PlayTracks` | `Err("nothing to play")` becomes `ControllerEvent::Error` |
| `PlayTrack(track)` | `play_track(track)` | async → `TaskResult::PlayTrack` | radio-style single track |
| `TogglePlay` | `toggle_play()` | sync | play/pause/resume/cancel load |
| `NextTrack` | `next_track()` | sync | manual next (wraps, overrides repeat-one) |
| `PrevTrack` | `prev_track()` | sync | restarts near start |
| `Seek(f64)` | `seek(position)` | sync | backend clamps + emits `Progress` immediately |
| `SetVolume(f32)` | `set_volume(volume)` | sync | live change, no disk write; controller remembers the value |
| `SaveVolume` | `save_volume(volume)` | sync | action carries no value → persists the last `SetVolume` (old UI sent the slider-release value) |
| `ToggleShuffle` | `toggle_shuffle()` | sync | backend emits `Queue` |
| `CycleRepeat` | `cycle_repeat()` | sync | backend emits `Queue`; `app.rs` already optimistically cycles `state.queue.repeat` |
| `SetDiscordRpc(bool)` | `set_discord_rpc(enabled)` | sync | persists |
| `SetCrossfade(f32)` | `set_crossfade(secs)` | sync | backend clamps to `0..=MAX_CROSSFADE` |
| `SetUpdateNotifications(bool)` | `set_update_notifications(enabled)` | sync | persists |

### 4.2 Queue

| `Action` | Backend call | Kind |
|---|---|---|
| `QueueAdd(track)` | `queue_add(track)` | sync |
| `QueuePlayNext(track)` | `queue_play_next(vec![track])` | sync |
| `QueueMove { from, to }` | `queue_move(from, to)` | sync |
| `QueueRemove(usize)` | `queue_remove(index)` | sync |
| `QueueJump(usize)` | `queue_jump(index)` | sync |
| `QueueClear` | `queue_clear()` | sync |

### 4.3 Library / playlists

| `Action` | Backend call | Kind | Notes |
|---|---|---|---|
| `ToggleLike(track)` | `toggle_like(track)` | sync | backend emits `Library` |
| `ToggleLikeId(id)` | `toggle_like(track)` | sync | id resolved via the controller's `TrackCache` (§5); unknown id → `Error` toast rather than guessing |
| `CreatePlaylist(name)` | `create_playlist(name)` | sync, fallible | `Err("playlist name cannot be empty")` → `Error`; backend emits `Library` |
| `RenamePlaylist { id, name }` | `rename_playlist(id, name)` | sync | emits `Library` |
| `DeletePlaylist(id)` | `delete_playlist(id)` | sync | emits `Library` |
| `MovePlaylist { from, to }` | `move_playlist(id, to)` | sync | old UI resolved the sidebar row index to a playlist id at emit time; the controller resolves `from` against its library mirror so a stale index can never move the wrong playlist (unknown row → ignored) |
| `AddToPlaylist { playlist_id, track }` | `add_to_playlist(id, track)` | sync | backend may auto-download for kept-offline playlists |
| `RemoveFromPlaylist { playlist_id, track_id }` | `remove_from_playlist(id, track_id)` | sync | emits `Library` |
| `ImportYtPlaylist` | — | UI-local | opens the paste box (old sidebar inline input) |
| `ImportYtPlaylistUrl(url)` | `import_yt_playlist(url)` | async → `TaskResult::ImportYtPlaylist` | new Action variant; trims and ignores blank. Success → `OpenPlaylist(id)` + `Notice("Imported “name” (N songs)")`, failure → `Error`; backend emits `Library` |
| `ImportPlaylistFile` | `import_playlist()` | fire-and-forget | native `rfd` Open dialog on the backend blocking pool; reports via backend events (`Library` + `OpenPlaylist` + `Notice`, or `Error`, nothing on cancel) |
| `ExportPlaylist(id)` | `export_playlist(id)` | fire-and-forget | native `rfd` Save dialog; `Notice`/`Error` events, nothing on cancel |
| `BeginRenamePlaylist`, `BeginNewPlaylist`, `PlaylistDraftChanged`, `ClosePlaylistEditor` | — | UI-local | |

### 4.4 Downloads

| `Action` | Backend call | Notes |
|---|---|---|
| `DownloadTracks(Vec<String>)` | `download_tracks(tracks)` | ids resolved via `TrackCache` (backend stores full metadata with the offline copy); unknown ids are reported with an `Error` toast, never silently skipped |
| `RemoveDownloads(Vec<String>)` | `remove_downloads(ids)` | the old UI's "kept offline playlist" guard lives in the screens (`State::pinned_ids`); unchanged |

### 4.5 Settings / updates / yt-dlp / misc

| `Action` | Backend call | Kind | Notes |
|---|---|---|---|
| `CheckUpdate` | `check_update()` | async → `TaskResult::CheckUpdate` → `ControllerEvent::UpdateStatus` | launch check also runs from `bootstrap()` when notifications are enabled |
| `CheckYtDlp` | `check_ytdlp()` | async → `YtDlpStatus` | |
| `SetYtDlpPath(Option<String>)` | `set_yt_dlp_path(path)` | async → `YtDlpStatus` or `Error` | invalid path keeps the stored value and its rejection message is toasted |
| `DownloadYtDlp` | `download_ytdlp()` | async → `YtDlpStatus` | backend toasts failure itself, so the controller forwards success only (no duplicate toast) |
| `OpenUrl(url)` | `open_url(url)` | sync, fallible | http(s)-only; refusal → `Error` |
| `RetryPage` | last page request re-issued (`get_artist` / `get_artist_songs` / `get_album`) | async | no-op when no page was ever requested |
| `OpenCurrentArtist` | — (derived) | local | resolves the current track's first linkable artist from the `Track` mirror → `ControllerEvent::OpenArtist(id)`; app clears + navigates |
| `DismissUpdateBanner`, `Toast`, `ToastError`, `SetOnline` | — | UI-local | |
| `WindowDrag`, `WindowMinimize`, `WindowToggleMaximize`, `WindowClose`, `WindowBeginResize` | — | winit/`main.rs` | the three former `window_*` commands are the app's job now |
| `Navigate`, `SearchQueryChanged`, `ToggleQueuePanel`, `ToggleQueueCollapsed` | — | UI-local (see §2 for exceptions) | |

`navigate_view`-only Actions (`OpenArtist`, `OpenArtistSongs`, `OpenAlbum`) are
also handled in `dispatch()` for completeness, but `app.rs` intercepts them for
local state changes; the page request comes from the `navigate()` hook so there
is exactly one request path.

---

## 5. Track metadata cache (id-only Actions)

`ToggleLikeId`, `DownloadTracks` and `OpenCurrentArtist` are the only backend
directions that need a full `Track` but carry less. The backend matches likes by
song identity and stores metadata with a download, so the controller keeps a
bounded `id → Track` cache (8192 entries, oldest-insertion eviction) filled from
everything it delivers: `bootstrap()` (library/queue/current), `Track`/`Library`/
`Queue` backend events, search results, artist pages, artist-song catalogs and
album pages. A miss is surfaced as an `Error` toast; nothing is guessed.

The old UI always handed over a `Track`, so no UI-facing behaviour is lost —
the cache only exists because the native `Action` enum reduces some payloads to
ids.

---

## 6. Stale-generation guarding (search + pages)

The old web app kept `search_gen` and `page_gen` counters in component state;
each async reply captured its token and was dropped if the counter had moved.
The controller now owns the counters (it is the half that issues requests and
receives replies):

* `Action::Search(query)` bumps `search_generation`, resets the lazy-tab flags,
  and spawns `search_songs(generation, query)`. A songs/artists/albums reply is
  emitted only when `generation == search_generation`.
* `Controller::cancel_search()` bumps `search_generation` again (box cleared), so
  nothing in flight can land afterwards.
* `navigate_view(&View)` bumps `page_generation` on **every** navigation
  (exactly the old `page_gen` semantics): leaving a page invalidates its reply.
  A page view then issues `load_artist_page` / `load_artist_songs` /
  `load_album_page` with the new token. Re-navigating to the page already in
  flight is de-duplicated (the first reply fills the state the app just cleared);
  `RetryPage` always issues a fresh request with a new token.
* Page events carry their `generation` so `app.rs` can keep
  `State::page_generation` monotonic and ignore anything older as defence in
  depth (§8). The controller already dropped stale replies before they became
  events.

Search sub-tabs keep the old lazy behaviour: `SetSearchTab(Artists|Albums)`
fetches that list once per submitted query (the old UI fetched on first tab
open, not with the songs search). Flags reset on every new search/cancel.

---

## 7. Cover art

`State::request_cover(url)` (unchanged) pushes into `state.cover_requests`;
`app.tick()` hands each URL to `Controller::request_cover(url)`. Then:

1. **De-dupe** — `CoverTracker` (`cover.rs`) keeps the controller-authoritative
   set of in-flight URLs and known-failed URLs (bounded to 1024 remembered
   failures, FIFO). A URL is fetched at most once while in flight; a known-bad
   URL is never retried this session and immediately yields
   `ControllerEvent::CoverFailed(url)` so the UI renders its fallback tile.
   Preview mode has no HTTP client, so requests fail immediately the same way.
2. **Fetch** — `live::LiveCommands::fetch_cover` spawns a task on the backend
   runtime that calls `BackendHandle::fetch_image(url)` (the backend's shared
   `reqwest::Client`, 16 MiB cap, http(s)-only). No second HTTP client is built
   in the app.
3. **Decode/downscale off the UI thread** — the encoded bytes are handed to
   `spawn_blocking(move || decode_cover(&url, &bytes))`; `decode_cover` uses the
   `image` crate (PNG/JPEG) and downscales the longest edge to
   `MAX_COVER_DIMENSION = 256` with Lanczos3, preserving aspect ratio and never
   upscaling, then converts to tightly packed RGBA8. A 256×256 cover is 256 KiB;
   the player-bar cover is the largest drawn size in the design.
4. **Reply** — one `TaskResult::Cover { url, result }`; `poll()` records
   success/failure in the tracker and emits `ControllerEvent::Cover(CoverImage)`
   or `ControllerEvent::CoverFailed(url)`. `app.rs` allocates the texture id and
   uploads, exactly as its existing arms already do. Offline/failed URLs are
   graceful: one probe, fallback tile, no retry storm.

Dedupe bookkeeping is inside the controller rather than trusting
`State::covers`, because UI state can be rebuilt/evicted while frames still ask
for the same URL.

## 8. `app.rs` apply code (exact)

```rust
fn apply_controller_event(&mut self, event: ControllerEvent) {
    match event {
        // ... existing arms unchanged ...
        ControllerEvent::SearchResults { query, results } => {
            if self.state.search.shown_query == query {
                self.state.search.busy = false;
                match results {
                    Ok(songs) => self.state.search.songs = songs,
                    Err(message) => {
                        self.state.search.songs.clear();
                        self.toast_error(message);
                    }
                }
            }
        }
        ControllerEvent::SearchArtists { query, results } => {
            if self.state.search.shown_query == query {
                match results {
                    Ok(artists) => self.state.search.artists = Some(artists),
                    Err(message) => {
                        self.state.search.artists = Some(Vec::new());
                        self.toast_error(message);
                    }
                }
            }
        }
        ControllerEvent::SearchAlbums { query, results } => {
            if self.state.search.shown_query == query {
                match results {
                    Ok(albums) => self.state.search.albums = Some(albums),
                    Err(message) => {
                        self.state.search.albums = Some(Vec::new());
                        self.toast_error(message);
                    }
                }
            }
        }
        ControllerEvent::ArtistPage { generation, page } => {
            if generation >= self.state.page_generation {
                self.state.page_generation = generation;
                self.state.artist_page = Some(page);
            }
        }
        ControllerEvent::ArtistSongs { generation, artist_id, tracks } => {
            if generation >= self.state.page_generation {
                self.state.page_generation = generation;
                self.state.artist_songs = Some((artist_id, tracks));
            }
        }
        ControllerEvent::AlbumPage { generation, page } => {
            if generation >= self.state.page_generation {
                self.state.page_generation = generation;
                self.state.album_page = Some(page);
            }
        }
        ControllerEvent::Online(online) => self.state.online = online,
        ControllerEvent::OpenArtist(id) => {
            self.state.artist_page = None;
            self.navigate(View::Artist(id));
        }
    }
}
```

Page error replies are emitted as `ControllerEvent::Error` (toast) and leave the
page empty; `Action::RetryPage` re-issues the load, matching the old UI's
toast-and-stay-put behaviour.

---

## 9. Backend event → `ControllerEvent` mapping

`BackendEvent` keeps the old `rift://…` payloads 1:1, and so does the mapping:

| Backend event (old name) | `ControllerEvent` |
|---|---|
| `Track(Track)` (`rift://track`) | `Track(Some(track))`; updates the current-track mirror |
| `State(PlaybackState)` (`rift://state`) | `PlaybackState(state)` |
| `Queue(QueueSnapshot)` (`rift://queue`) | `Queue(snapshot)`; tracks cached |
| `Progress(Progress)` (`rift://progress`) | `Progress(progress)` |
| `Library(Library)` (`rift://library`) | `Library(library)`; updates the library mirror + track cache |
| `Downloads(DownloadState)` (`rift://downloads`) | `Downloads(state)` |
| `Error(String)` (`rift://error`) | `Error(message)` (toast) |
| `Notice(String)` (`rift://notice`) | `Notice(message)` (toast) |
| `OpenPlaylist(String)` (`rift://open_playlist`) | `OpenPlaylist(id)` (navigate) |

Async command completions become: `SearchResults`, `SearchArtists`,
`SearchAlbums`, `ArtistPage`, `ArtistSongs`, `AlbumPage`, `UpdateStatus`,
`YtDlpStatus`, `Cover`/`CoverFailed`, `OpenPlaylist`+`Notice` (YT import),
`Error` (failed playback/search/page/import/path validation), `Online`.

## 10. Online status

The old frontend used the webview's `online`/`offline` window events; the native
app asks the backend instead. `Controller` spawns
`BackendHandle::is_online()` (the backend's HEAD-request probe, 2 s cap, "reached
a youtube.com host" rule shared with the download-retry logic) once from
`bootstrap()` and then at most every `ONLINE_PROBE_INTERVAL = 10 s`, and only
when no probe is already in flight. A changed result emits
`ControllerEvent::Online(bool)`; `app.rs` sets `state.online` (the existing
`track_unavailable`/row-greying logic keeps working). `Controller::online()`
returns the last observed value for callers that cannot wait for a drain.

Failure is graceful: a probe that cannot run just leaves the previous value.

## 11. State additions

`crates/rift-desktop/src/state.rs` (extend-only):

* `State::page_generation: u64` — monotonic generation of the newest artist/album
  page result applied; `app.rs` guards page events with it (§8).
  `SearchState::generation` keeps its existing app-side role; the authoritative
  search guard lives in the controller now (documented on the field).
* `Action::ImportYtPlaylistUrl(String)` — the sidebar paste box's submitted
  link; bare `Action::ImportYtPlaylist` stays UI-local ("open the paste box").
  Screens emit the URL-carrying variant.

## 12. Known limitations / notes

* **`BackendEvents` is unbounded** — `poll()` drains every queued event each
  frame; if the app ever stops calling `poll()`, the queue grows
  (`backend-api.md` §9). A tracked cover/import completion channel is likewise
  unbounded, but each command sends exactly one completion.
* **Backend `shutdown()` can wait** for an in-flight blocking task (e.g. an open
  `rfd` dialog on the blocking pool). It also flushes the queued durable
  `library.json`/`settings.json` writes (bounded wait) before the runtime stops —
  those writes run on the `rift-durable` thread, never the UI thread. `main.rs`
  already calls `shutdown()` from `exiting()` after the UI is done, so the
  documented loss (waiting for the dialog) is acceptable; do not call
  `shutdown()` while the app is still drawing.
* **`ImportYtPlaylistUrl` is new API** — screens must emit it; see the note sent
  to `screens-chrome`/`screens-pages`. The old `invoke("import_yt_playlist",
  { url })` came from the sidebar's inline input.
* **Id-only Actions** use the bounded `TrackCache` (§5). An extremely
  long-browsing session could evict a track before an id-only action references
  it; the user sees an explicit error toast instead of a wrong action. Views
  that deliberately reduce payloads to ids should keep doing so.
* **`SaveVolume` carries no value** (state.rs model); the controller persists
  the last `SetVolume` it saw (or the bootstrap volume). `app.rs` sends
  `SetVolume` on every drag tick, so this is always the release value.
* **Preview mode never constructs the backend** and never fetches covers:
  commands are inert and cover requests fail immediately, so screenshots are
  deterministic and offline.
* Backend construction must not happen inside a Tokio runtime context
  (`backend-api.md` §1); `Controller::new` is called from winit's window
  creation path on the UI thread, which satisfies this.

---

## 13. Tests

Feature-independent (no live backend, no network, no window):

| Test (in `crates/rift-desktop/src/controller.rs` unless noted) | What it pins |
|---|---|
| `dispatch_maps_transport_and_volume_to_the_backend` | play/pause/next/prev/seek/volume/save-volume/shuffle/repeat |
| `dispatch_maps_playback_and_queue_commands` | play_tracks/play_track/queue add/next/move/remove/jump/clear |
| `dispatch_maps_library_and_playlist_commands` | like (Track + id), create/rename/delete/move (index→id)/add/remove/export/import file + URL trim |
| `an_unknown_toggle_like_id_reports_an_error_instead_of_guessing` | id miss → Error, no backend call |
| `dispatch_maps_settings_update_and_ytdlp_commands` | discord/crossfade/update notifications/check/yt-dlp path+download/open_url |
| `dispatch_maps_downloads_and_resolves_ids_through_the_track_cache` | id→Track resolution for downloads |
| `dispatch_keeps_ui_local_actions_off_the_backend` | navigation/window/toast/online/dev actions never reach the backend |
| `fallible_commands_surface_their_error_on_the_next_poll` | `open_url` refusal → `Error` |
| `open_current_artist_resolves_the_playing_tracks_first_linkable_artist` | `OpenCurrentArtist` → `OpenArtist` |
| `search_results_for_a_superseded_query_are_dropped` | oldest search reply never reaches the UI |
| `clearing_the_search_box_cancels_the_in_flight_search` | `cancel_search` invalidation |
| `artist_and_album_tabs_are_lazily_fetched_once_per_search` | lazy tabs, once per query, reset per search |
| `navigating_to_a_page_requests_it_at_the_current_generation` | page loads + generations |
| `a_page_reply_for_an_abandoned_view_is_dropped` | leave-view invalidation, refetch on return |
| `a_duplicate_navigation_to_the_loading_page_does_not_refetch` | in-flight de-dupe |
| `retry_page_reissues_the_last_page_request` | `RetryPage` |
| `a_failed_page_reports_an_error_and_retry_works` | page error → toast, retry with fresh token |
| `cover_requests_are_deduplicated_while_in_flight` | one fetch per URL while loading |
| `a_failed_cover_is_not_requested_again` | known-failed URL → `CoverFailed`, no refetch |
| `a_decoded_cover_is_emitted_and_a_later_request_may_refetch` | `Cover` event + tracker release |
| `preview_mode_fails_cover_requests_immediately` | inert preview path |
| `a_failed_async_playback_is_reported_as_an_error` | async error mapping |
| `a_yt_playlist_import_navigates_and_toasts_like_the_old_ui` | import → `OpenPlaylist` + `Notice` |
| `reachability_changes_emit_online_only_on_transitions` | `Online` edge-triggering |
| `bootstrap_seeds_the_mirrors_and_runs_the_launch_update_check` | bootstrap mirrors, launch check |
| `the_reachability_probe_runs_on_the_interval_and_reports_transitions` | probe interval, no probe storm |
| `a_track_cache_insert_updates_metadata_in_place` | cache update semantics |
| `cover.rs`: 8 tests | 256 px cap (landscape/portrait/panorama), no upscale, RGBA/alpha preservation, garbage bytes → `Err`, tracker dedupe/failure/bound |
| `backend_event_tests::every_backend_event_maps_one_to_one` (feature-gated) | all 9 backend event payloads → their `ControllerEvent`; library/current mirrors |

### Test commands

Run from the repository root (Rust 1.88 pinned). The app crate's default
features include ackend; --no-default-features builds the inert-controller
UI used for renderer/immediate-mode work.

`powershell
cargo test -p rift-desktop
cargo test -p rift-desktop --no-default-features
cargo test -p rift-desktop --test live_backend -- --ignored --nocapture
cargo clippy -p rift-desktop --all-targets -- -D warnings
cargo fmt --check
`

---

## 14. Headless interaction tests (acceptance proof, `crates/rift-desktop/tests/ui_flow.rs`)

Ten end-to-end tests drive the **real** `App` exactly like `main.rs` does —
`App::tick()` then `App::frame(InputState)` with synthetic input — and assert the
resulting app state, paint list and controller commands:

1. `boot_frame_paints_the_window_bands` — preview boot at 1160×740 paints >100
   prims including the titlebar/sidebar/main/queue/player bands and the 36 px
   play circle; `state.view == Home`, sidebar 335 px.
2. `sidebar_rows_navigate` — clicking the Liked Songs row → `View::Liked`;
   clicking Home → `View::Home`.
3. `titlebar_search_types_submits_and_clears` — click focuses the box, typing
   updates `search.query` and issues the search command, Enter submits
   (`view == Search`, `shown_query`, busy), Escape clears + cancels (busy false,
   keyboard released).
4. `player_bar_clicks_reach_the_backend_bridge` — play/pause → `TogglePlay`,
   seek-bar click at 50% → `Seek(~107 s)`, volume at 75% →
   `SetVolume(0.75)` + `SaveVolume(0.75)`, like heart →
   `ToggleLike("preview-1")`.
5. `keyboard_shortcuts_work_without_text_focus` — Space → `TogglePlay`,
   Ctrl+Right → `NextTrack`, Ctrl+Left → `PrevTrack`, Escape closes the playlist
   rename editor.
6. `queue_panel_collapses_clears_and_jumps` — UP NEXT header toggles collapsed
   (and the layout re-docks to 45 px), second header click expands, row 1 click →
   `QueueJump(1)`, trash → `QueueClear`.
7. `window_controls_emit_window_commands` — minimize/maximize/close glyphs →
   the three `WindowCommand`s; dragging empty titlebar → `WindowCommand::Drag`.
8. `settings_toggles_sliders_and_probes_map_to_commands` — Discord switch →
   `SetDiscordRpc(false)`, crossfade switch → `SetCrossfade(6.0)`, dragging the
   crossfade slider (row grows when enabled) → `SetCrossfade(>6)`, "Check again"
   → `CheckYtDlp`; also asserts screen coordinates come from
   `settings::row_heights` + the row formulas.
9. `context_menu_opens_closes_and_dispatches_an_action` — hover a playlist card
   and click its kebab → menu open; Escape closes with no command; re-open and
   click "Rename" → `BeginRenamePlaylist` reaches app state.
10. `text_editing_handles_backspace_and_enter_submit` — search box append/backspace/
    clear; navigate to a playlist, open Rename from the header kebab, click the
    inline editor, type, Enter → `RenamePlaylist(id, "Playlist 1 2")` and the
    editor closes.

**Seam used:** `Controller::with_commands(Box<dyn BackendCommands>)`
(`#[doc(hidden)]`, non-live: no runtime, event bus, probe or backend is built) so
the suite can install a recording double; production behaviour is identical
(`Controller::new` / `new_preview` are the only production paths).

**Coordinate policy:** the tests mirror each screen's own geometry formulas from
its public constants (`screens::layout`, `cards::*`, `settings::row_heights`,
`queue::*`, `menu::*`); numbers private to a screen module are copied with the
source named in the helper's comment. A layout change therefore fails the
interaction that depends on it instead of clicking empty space, and the fix is to
update the one helper. `crates/rift-desktop/tests/ui_flow.rs`'s module docs explain the same
assumption, and every click is a press frame + release frame because that is the
framework's click state machine.

### Three real interaction bugs this suite found (fixed by `screens-chrome`)

The acceptance suite's first run passed 6/10 flows and exposed these defects
(reported with failing assertions, then fixed in `screens/`):

1. **Seeking was impossible** — `chrome.rs` emitted `Action::Seek` only when
   `committed` *and* `dragged_fraction` were set; `cards::slider` returns
   `dragged_fraction` only while active, but the release frame (`committed`) is
   inactive, so neither click-seek nor drag-seek ever emitted. Fix: the slider
   outcome now carries `committed_fraction` computed from the release pointer and
   chrome seeks from it.
2. **The titlebar drag region stole keyboard focus from the search box** — the
   full-width `Sense::DRAG` region was interacted after the search field on the
   same press; because `Sense::DRAG` is focusable and `active` was still `None`,
   it overwrote the field's focus, so typing into the search box did nothing.
   Fix: skip the drag region when the pointer is over the search box or window
   controls, and make it non-focusable (`Sense::new(true, true, false)`).
3. **Context menus opened from main-view content never stayed open** — `root`
   called `menu::layer` before the main view, while cards/track lists open menus
   during it; `Ui::end_frame` auto-dismisses an undrawn popup, so the menu closed
   in the same frame. Fix: keep the early layer call for click-outside
   consumption and re-run `menu::layer` after the content when a menu was opened
   this frame.

These are exactly the classes of bug screenshots cannot catch (an action never
emitted, focus stealing), which is why this suite exists.

---

## 15. Live backend smoke — `#[ignore]`-gated tests (`crates/rift-desktop/tests/live_backend.rs`)

Proves the **real** controller ↔ `rift::RiftBackend` bridge on this machine: real
backend construction (audio/media/Discord threads, Tokio runtime, shared HTTP
client), the real event bus, real YouTube Music network calls, the real yt-dlp
binary and the real cover pipeline. They are `#[ignore]`d because they need
network + yt-dlp (not CI), and they are run manually:

```powershell
# current tree: default features already include `backend`, so the flag is
# redundant (but accepted). `--test-threads=1` is unnecessary: the tests
# serialise themselves on the process-global RIFT_DATA_DIR.
cargo test -p rift-desktop --test live_backend -- --ignored --nocapture
#   test result: ok. 5 passed; 0 failed; finished in 6.21 s
```

### Safety (the user's real data is never touched)

Each test sets `RIFT_DATA_DIR` to a unique throwaway directory under `%TEMP%`,
seeds it with a `settings.json` whose only field is
`"yt_dlp_path": "C:\\Users\\<user>\\AppData\\Roaming\\dev.jerimiah.rift\\bin\\yt-dlp.exe"`
(the backend's data-dir precedence is config → `RIFT_DATA_DIR` → platform dir, so
the real `%APPDATA%\dev.jerimiah.rift` is never read or written), removes
`RIFT_PREVIEW`, and on `Drop` shuts the backend down, restores the previous env
vars and deletes the directory. Verified: **0** `rift-live-smoke-*` directories
left in `%TEMP%` after the run, and the real data dir's file mtimes predate the
run (untouched).

### The five flows

1. `live_bootstrap_event_pump_and_clean_shutdown` — `Controller::new(None)` with
   the seeded temp dir is live; `bootstrap()` is sane (empty library, `Stopped`,
   finite volume in `0..=1`, `preview == false`); a backend-local
   `Action::ToggleLike` produces a `Library` broadcast through the **real** event
   bus; an idle 300 ms of `poll()` returns promptly (no replay `Progress` is
   expected — the smoke run plays no audio; note the expected rodio
   `Dropping DeviceSink …` log line when the audio sink is dropped).
2. `live_search_through_app_state` — a real `App::new(false, live_controller)`,
   `app.dispatch_ui(Action::Search("daft punk get lucky"))`, then `app.tick()` until
   `state.search.busy` clears: 20 real songs and `shown_query` correct; the
   controller shuts down cleanly.
3. `live_stale_search_generation_is_filtered` — two live searches dispatched
   back-to-back; exactly **one** `SearchResults` is emitted, for the newest query
   (the stale reply is dropped by the controller's generation filter).
4. `live_cover_fetch_decode_and_dedupe` — a cover URL from the first live search
   result: request it twice before polling → exactly one `Cover` with ≤256 px
   RGBA8; re-requesting the same URL while loaded does not emit a duplicate; a
   real 544×544 variant of the thumbnail is fetched and decoded to **256×256**
   (downscale evidence); a dead URL yields `CoverFailed` and a second request
   re-emits it from the known-failed tracker without a new fetch.
5. `live_update_check_and_ytdlp_probe` — `check_ytdlp()` reports
   `found=true` with the seeded path and version; `check_update()` reports the
   running version (and degrades to "latest unknown" rather than hanging if
   GitHub is unreachable).
