# Rift developer documentation

Rift is a native Rust desktop music player. The application is a hand-written
immediate-mode UI (`crates/rift-desktop/`) rendered with Vulkano on winit, driven
by the `rift` backend library (audio, player, library, native streaming and
downloads). There is no webview shell.

These pages document the implementation contracts and reference material for the
native app.

| Document | What it covers |
|---|---|
| [`architecture.md`](architecture.md) | Workspace layout, module layout, rendering model, paint contract, immediate-mode UI contract, windowing, preview/screenshot mode and build commands. |
| [`backend-api.md`](backend-api.md) | The `rift` backend library: `RiftBackend` construction/config, command methods with params/returns/errors/async notes, the `BackendEvent` bus, data directories and how the app drives it. |
| [`native-streaming.md`](native-streaming.md) | Clean-room native audio streaming: the validated JS-less resolver/client table, ranged downloader, per-track progress contract/UI, verification record and phase history (including the yt-dlp removal). |
| [`imgui-api.md`](imgui-api.md) | The hand-written immediate-mode GUI: layout, text, input, widgets, popups, ids and the per-frame contract used by the screens. |
| [`ui-spec.md`](ui-spec.md) | Screen-by-screen visual/behavioural specification: design tokens, geometry, states and interactions. |
| [`controller-notes.md`](controller-notes.md) | UI-to-backend bridge: command mapping table, threading model, stale-generation rules and the cover pipeline. |
| [`native-platform.md`](native-platform.md) | Native window-state persistence and the single-instance guard. |

Related repository notes:

- [`innertube-research.md`](innertube-research.md) — how streaming works through
  yt-dlp and how the YouTube Music innertube client was derived, including the
  2026-10-26 addendum that revisits its "keep yt-dlp forever" verdict. Retained
  as historical research; the shipped app no longer bundles or calls yt-dlp.
