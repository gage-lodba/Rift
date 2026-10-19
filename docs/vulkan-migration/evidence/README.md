# Committed acceptance evidence

The historical conversion runs used raw screenshots, probe projects, throwaway
harnesses and CI logs in the gitignored `artifacts/` and `scratch/` scratch
directories; those are not shipped (the scratch space was cleaned up). What must
survive a clean checkout is committed here:

- [`final-gate.log`](final-gate.log) — full output of the final
  `pwsh tools/ci-local.ps1` acceptance run on the **final tree**: **13 executed
  steps PASS** (toolchain guard, fmt, clippy `-D warnings`, rustdoc with
  warnings denied, both test configurations, locked shipped release build,
  preview-stripped check on the shipped binary, acceptance-only
  `preview`-feature release screenshot + old-UI reference comparison, legacy-shell
  dependency guards, duplicate-deps baseline), plus the supply-chain step
  reported **SKIP** locally because `cargo-deny` is not installed on this
  machine — CI's dedicated `deny` job runs it for every push/PR. The shipped
  release binary does not contain preview mode; the screenshot is captured from
  a separate acceptance-only build with the non-default `preview` feature. Test
  counts, timings, binary size and screenshot metrics are in the log.
  Machine-specific absolute paths in the log are abbreviated (`<repo>`,
  `<python>`, `<user>`) so the evidence is portable.

Curated screenshots and comparisons are committed under
[`../screenshots/`](../screenshots/); every raw artifact is reproducible with the
commands documented in [`../migration-report.md`](../migration-report.md),
[`../verification.md`](../verification.md) and
[`../screenshot-gallery.md`](../screenshot-gallery.md).
