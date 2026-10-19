# Native Rift developer documentation

Rift is a native Rust desktop music player. The application is a hand-written
immediate-mode UI (`crates/rift-desktop/`) rendered with Vulkano on winit, driven
by the `rift` backend library (audio, player, library, yt-dlp streaming and
downloads). There is no webview shell.

These documents are the implementation contracts and reference material for the
native app. Start with [`architecture.md`](architecture.md) for the workspace
layout and the per-frame paint/UI contracts; the UI and data paths are described
by [`ui-spec.md`](ui-spec.md) + [`imgui-api.md`](imgui-api.md) and
[`backend-api.md`](backend-api.md) + [`controller-notes.md`](controller-notes.md).

| Document | What it covers |
|---|---|
| [`architecture.md`](architecture.md) | Workspace layout, module ownership, rendering model, paint contract, immediate-mode UI contract, windowing, preview/screenshot mode, build commands, milestones. |
| [`backend-api.md`](backend-api.md) | The `rift` backend library: `RiftBackend` construction/config, command methods with params/returns/errors/async notes, the `BackendEvent` bus, data directories, how the app drives it. |
| [`imgui-api.md`](imgui-api.md) | The hand-written immediate-mode GUI: layout, text, input, widgets, popups, ids and the per-frame contract used by the screens. |
| [`ui-spec.md`](ui-spec.md) | Screen-by-screen visual/behavioural specification: tokens, geometry, states and interactions, measured against the archived reference screenshot. |
| [`controller-notes.md`](controller-notes.md) | UI↔backend bridge: command mapping table, threading model, stale-generation rules, cover pipeline, live smoke-test evidence. |
| [`native-platform.md`](native-platform.md) | Native window-state persistence and the single-instance guard. |
| [`verification.md`](verification.md) | Renderer/acceptance verification: resize, DPI, windowed, validation, performance and final-tree results. |
| [`vulkano-recipe.md`](vulkano-recipe.md) | Practical Vulkano 0.35 setup notes the renderer is based on (instance/device/swapchain/pipelines/screenshot). |
| [`screenshot-gallery.md`](screenshot-gallery.md) | Curated native screenshots (all views, HiDPI, live window), reference comparison, contact sheet and the visual-regression procedure. |
| [`evidence/`](evidence/) | Committed acceptance evidence: the final `tools/ci-local.ps1` gate log. |
| [`migration-report.md`](migration-report.md) | Project report: parity summary, final verification table and known limitations. |

Related repository docs:

- [`docs/innertube-research.md`](../innertube-research.md) — how streaming works
  through yt-dlp and how the innertube client was derived.
