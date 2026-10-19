#requires -Version 7
<#
.SYNOPSIS
    Local CI gate for Rift — replicates `.github/workflows/ci.yml` on Windows.

.DESCRIPTION
    One command that runs the same checks the GitHub Actions CI runs, in the same
    order, on this machine, so the final native acceptance is reproducible
    locally. Every step prints PASS/FAIL with its command and duration, a summary
    table is printed at the end, and a timestamped log is written under
    `artifacts/`.

    Steps (mirroring ci.yml):
      1. toolchain consistency (`.github/scripts/check-toolchain-consistency.sh`
         via Git Bash; falls back to an equivalent PowerShell check and says so)
      2. `cargo fmt --all -- --check`
      3. `cargo clippy -p rift -p rift-types -p rift-ytmusic -p rift-desktop
         --all-targets -- -D warnings`
      4. `cargo doc -p rift -p rift-types -p rift-ytmusic --no-deps` with
         `RUSTDOCFLAGS=-D warnings` (the library crates enforce missing_docs)
      5. `cargo test` for those packages, plus the app in **both feature
         configurations** (default backend + `--no-default-features` UI-only)
      6. `cargo build --release -p rift-desktop --features backend` (the shipped
         configuration, + binary size) and check the shipped binary rejects the
         development-only `--preview`/`--screenshot` flags (preview data is not
         compiled into release without the explicit `preview` feature)
      7. offscreen acceptance screenshot at 1160x740 from an acceptance-only
         release build with the explicit `preview` feature, compared against
         `tools/reference-old-ui.png` (comparison reports metrics
         only — a visual delta does not fail the gate)
      8. legacy-dependency guard: `cargo tree -p rift-desktop` has no webview
         shell/frontend crates, and no workspace Cargo.toml declares one
      9. duplicate-dependency baseline (`.github/scripts/check-duplicate-deps.sh`
         via Git Bash; skipped with a note when Git Bash is unavailable)
      10. supply-chain checks: `cargo deny check all` (advisories, licenses,
          bans, sources) from `deny.toml`; skipped with a note when `cargo-deny`
          is not installed

    `--locked` is passed to every cargo build/check/test by default, exactly as
    CI does, so a stale Cargo.lock fails here instead of on the remote. Use
    `-AllowLockUpdate` while dependencies are changing.

.PARAMETER SkipTests
    Skip the `cargo test` steps (fast iteration).

.PARAMETER SkipRelease
    Skip the release build and the screenshot acceptance steps.

.PARAMETER AllowLockUpdate
    Do not pass `--locked` to cargo (use while Cargo.lock is being regenerated).

.PARAMETER PythonPath
    Python interpreter used for the reference-image comparison. Defaults to
    `$env:RIFT_PYTHON`; if unset, `python`/`python3`/`py -3` are probed and the
    comparison is skipped with a note when none has Pillow + numpy.

.EXAMPLE
    pwsh tools/ci-local.ps1
    pwsh tools/ci-local.ps1 -SkipTests
    pwsh tools/ci-local.ps1 -SkipRelease -SkipTests
#>
[CmdletBinding()]
param(
    [switch]$SkipTests,
    [switch]$SkipRelease,
    [switch]$AllowLockUpdate,
    [string]$PythonPath = $env:RIFT_PYTHON
)

$ErrorActionPreference = "Stop"

# pwsh's legacy "Windows" native-argument mode mangles arrays of native
# arguments (`--`/`-D warnings` get split); this repository's gates pass cargo
# flags through arrays, so use the faithful Standard mode when the host exposes
# the switch (PowerShell 7.3+).
if (Get-Variable -Name PSNativeCommandArgumentPassing -ErrorAction SilentlyContinue) {
    $PSNativeCommandArgumentPassing = "Standard"
}

$root = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $root

$timestamp = Get-Date -Format "yyyyMMdd-HHmmss"
$logDir = Join-Path $root "artifacts"
New-Item -ItemType Directory -Force -Path $logDir | Out-Null
$script:logPath = Join-Path $logDir "ci-local-$timestamp.log"
$script:results = [System.Collections.Generic.List[object]]::new()

function Write-Log {
    param(
        [AllowEmptyString()][string]$Text = "",
        [ConsoleColor]$Color = [ConsoleColor]::Gray
    )
    Write-Host $Text -ForegroundColor $Color
    Add-Content -LiteralPath $script:logPath -Value $Text -Encoding utf8
}

function Add-Result {
    param(
        [string]$Step,
        [ValidateSet("PASS", "FAIL", "SKIP", "WARN")][string]$Status,
        [double]$Seconds = 0,
        [string]$Detail = ""
    )
    $script:results.Add([pscustomobject]@{
            Step    = $Step
            Status  = $Status
            Seconds = [math]::Round($Seconds, 1)
            Detail  = $Detail
        })
}

function Write-Summary {
    Write-Log ""
    Write-Log "================ SUMMARY ================" Cyan
    $nameWidth = ($script:results | ForEach-Object { $_.Step.Length } | Measure-Object -Maximum).Maximum
    if (-not $nameWidth) { $nameWidth = 10 }
    $nameWidth = [Math]::Min([int]$nameWidth + 2, 72)
    foreach ($result in $script:results) {
        $color = switch ($result.Status) {
            "PASS" { [ConsoleColor]::Green }
            "FAIL" { [ConsoleColor]::Red }
            "WARN" { [ConsoleColor]::Yellow }
            default { [ConsoleColor]::DarkGray }
        }
        $detail = if ($result.Detail) { "  $($result.Detail)" } else { "" }
        Write-Log ("{0,-7} {1,-$nameWidth} {2,7:N1}s{3}" -f $result.Status, $result.Step, $result.Seconds, $detail) $color
    }
    $failed = @($script:results | Where-Object Status -eq "FAIL")
    $warned = @($script:results | Where-Object Status -eq "WARN")
    Write-Log "=========================================" Cyan
    if ($failed.Count -gt 0) {
        Write-Log "GATE: FAILED ($($failed.Count) hard failure(s), $($warned.Count) warning(s))" Red
    } elseif ($warned.Count -gt 0) {
        Write-Log "GATE: PASSED with $($warned.Count) warning(s)" Yellow
    } else {
        Write-Log "GATE: PASSED" Green
    }
    Write-Log "log: $($script:logPath)" DarkGray
}

# Run one step. A hard step's non-zero exit (or thrown exception) fails the gate;
# a soft step's does not, but is surfaced as WARN. Output is streamed live and
# captured in the log.
function Invoke-Step {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][scriptblock]$Body,
        [ValidateSet("Hard", "Soft")][string]$Kind = "Hard"
    )
    Write-Log ""
    Write-Log "=== $Name ===" Cyan
    $global:LASTEXITCODE = 0
    $watch = [System.Diagnostics.Stopwatch]::StartNew()
    $failure = $null
    try {
        & $Body 2>&1 | ForEach-Object {
            $line = ($_ | Out-String).TrimEnd()
            Write-Log $line
        }
        if ($LASTEXITCODE -ne 0) {
            $failure = "exit code $LASTEXITCODE"
        }
    } catch {
        $failure = $_.Exception.Message
    }
    $watch.Stop()
    if ($failure) {
        if ($Kind -eq "Soft") {
            Write-Log "WARN: $Name — $failure ($([math]::Round($watch.Elapsed.TotalSeconds, 1))s)" Yellow
            Add-Result -Step $Name -Status "WARN" -Seconds $watch.Elapsed.TotalSeconds -Detail $failure
        } else {
            Write-Log "FAIL: $Name — $failure ($([math]::Round($watch.Elapsed.TotalSeconds, 1))s)" Red
            Add-Result -Step $Name -Status "FAIL" -Seconds $watch.Elapsed.TotalSeconds -Detail $failure
        }
    } else {
        Write-Log "PASS: $Name ($([math]::Round($watch.Elapsed.TotalSeconds, 1))s)" Green
        Add-Result -Step $Name -Status "PASS" -Seconds $watch.Elapsed.TotalSeconds
    }
}

# Git Bash is needed for the repository's shell guard scripts. WSL's bash is not
# usable here (no cargo/registry access), so only real Git for Windows paths are
# accepted.
function Find-GitBash {
    $candidates = @(
        (Join-Path ${env:ProgramFiles} "Git\bin\bash.exe"),
        (Join-Path ${env:ProgramFiles} "Git\usr\bin\bash.exe"),
        (Join-Path ${env:ProgramFiles(x86)} "Git\bin\bash.exe"),
        "C:\Program Files\Git\bin\bash.exe",
        "C:\Program Files\Git\usr\bin\bash.exe"
    )
    foreach ($candidate in $candidates) {
        if ($candidate -and (Test-Path -LiteralPath $candidate)) {
            return $candidate
        }
    }
    return $null
}

$script:gitBash = Find-GitBash
# Keep this an array even when empty/enabled: an `if` expression that yields
# `@("--locked")` enumerates to a scalar string, and splatting a scalar mangles
# the native command line.
$script:lockedArgs = [string[]]@()
if (-not $AllowLockUpdate) {
    $script:lockedArgs = [string[]]@("--locked")
}
$script:packages = @("-p", "rift", "-p", "rift-types", "-p", "rift-ytmusic", "-p", "rift-desktop")

# A Python with Pillow + numpy is needed for the image comparison only. The
# bundled DSH runtime has both; a stock Python usually has neither, hence the
# probing and the RIFT_PYTHON escape hatch.
function Find-Python {
    param([string]$Explicit)
    $candidates = [System.Collections.Generic.List[object]]::new()
    if ($Explicit) {
        $candidates.Add([pscustomobject]@{ Exe = $Explicit; Prefix = @() })
    }
    foreach ($name in @("python", "python3")) {
        $command = Get-Command $name -ErrorAction SilentlyContinue
        if ($command) {
            $candidates.Add([pscustomobject]@{ Exe = $command.Source; Prefix = @() })
        }
    }
    $py = Get-Command "py" -ErrorAction SilentlyContinue
    if ($py) {
        $candidates.Add([pscustomobject]@{ Exe = $py.Source; Prefix = @("-3") })
    }
    foreach ($candidate in $candidates) {
        try {
            $prefix = $candidate.Prefix
            & $candidate.Exe @prefix -c "import numpy, PIL" 2>&1 | Out-Null
            if ($LASTEXITCODE -eq 0) {
                return $candidate
            }
        } catch {
            # Candidate not runnable; try the next one.
        }
    }
    return $null
}

$script:python = Find-Python -Explicit $PythonPath

# ---------------------------------------------------------------------------
Write-Log "Rift local CI gate — $timestamp" Cyan
Write-Log "root: $root"
Write-Log "log : $($script:logPath)"
$rustc = (& rustc --version 2>&1 | Out-String).Trim()
$cargoVersion = (& cargo --version 2>&1 | Out-String).Trim()
Write-Log "rustc: $rustc"
Write-Log "cargo: $cargoVersion"
Write-Log "locked: $(-not $AllowLockUpdate); skip-tests: $SkipTests; skip-release: $SkipRelease"
Write-Log "git bash: $(if ($script:gitBash) { $script:gitBash } else { 'NOT FOUND (shell guards fall back/skip)' })"
Write-Log "python (image compare): $(if ($script:python) { "$($script:python.Exe) $($script:python.Prefix -join ' ')" } else { 'NOT FOUND (comparison will be skipped)' })"
$head = (& git rev-parse --short HEAD 2>&1 | Out-String).Trim()
$dirty = @(git status --porcelain 2>$null).Count
Write-Log "git HEAD: $head; changed files: $dirty (informational — the gate checks the working tree, not a commit)"

# 1. Toolchain consistency ------------------------------------------------
if ($script:gitBash) {
    Invoke-Step "toolchain consistency" {
        & $script:gitBash .github/scripts/check-toolchain-consistency.sh
        if ($LASTEXITCODE -ne 0) { throw "check-toolchain-consistency.sh failed" }
    }
} else {
    Invoke-Step "toolchain consistency (PowerShell fallback)" {
        Write-Log "NOTE: Git Bash unavailable — replicating the toolchain check in PowerShell."
        $cargoText = Get-Content -LiteralPath "Cargo.toml" -Raw
        $msrv = [regex]::Match($cargoText, '(?m)^rust-version\s*=\s*"([^"]+)"').Groups[1].Value
        $toolchainText = Get-Content -LiteralPath "rust-toolchain.toml" -Raw
        $channel = [regex]::Match($toolchainText, '(?m)^channel\s*=\s*"([^"]+)"').Groups[1].Value
        if (-not $msrv -or -not $channel) { throw "could not read rust-version or toolchain channel" }
        $normalize = { param([string]$v) $parts = $v.Split("."); "$($parts[0]).$($parts[1])" }
        Write-Log "declared rust-version = $msrv; rust-toolchain channel = $channel"
        if ((& $normalize $msrv) -ne (& $normalize $channel)) {
            throw "rust-toolchain.toml pins $channel but Cargo.toml declares $msrv"
        }
        foreach ($workflow in Get-ChildItem -Path ".github/workflows" -Filter "*.yml") {
            $wfText = Get-Content -LiteralPath $workflow.FullName -Raw
            $uses = ([regex]::Matches($wfText, '(?m)^\s*(-\s+)?uses:\s+dtolnay/rust-toolchain@')).Count
            $pins = ([regex]::Matches($wfText, '(?m)^\s*toolchain:\s*(.+)$'))
            if ($uses -ne $pins.Count) {
                throw "$($workflow.Name) uses dtolnay/rust-toolchain $uses time(s) but has $($pins.Count) toolchain: pin(s)"
            }
            foreach ($pin in $pins) {
                $version = $pin.Groups[1].Value.Trim()
                if ($version -notmatch '^\d+\.\d+') {
                    throw "$($workflow.Name) pins toolchain '$version', which is not a version"
                }
                if ((& $normalize $version) -ne (& $normalize $msrv)) {
                    throw "$($workflow.Name) pins toolchain $version but Cargo.toml declares $msrv"
                }
            }
        }
        Write-Log "toolchain version is consistent across Cargo.toml, rust-toolchain.toml, and the workflows"
    }
}

# 2. Formatting -----------------------------------------------------------
Invoke-Step "cargo fmt --all -- --check" {
    & cargo fmt --all -- --check
    if ($LASTEXITCODE -ne 0) { throw "formatting differences (see diff above; do not reformat other owners' files)" }
}

# 3. Clippy ---------------------------------------------------------------
Invoke-Step "cargo clippy (-D warnings)" {
    $packages = $script:packages
    $locked = $script:lockedArgs
    & cargo clippy @packages @locked --all-targets -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "clippy reported warnings/errors above" }
}

# 3b. Documentation (library crates; rustdoc warnings are errors) ----------
# The three reusable crates opt into `#![warn(missing_docs)]`, so a doc
# regression must fail the gate rather than silently rot. rift-desktop is an
# internal app crate and is deliberately excluded (see README conventions).
Invoke-Step "cargo doc -p rift -p rift-types -p rift-ytmusic" {
    $locked = $script:lockedArgs
    $previous = $env:RUSTDOCFLAGS
    $env:RUSTDOCFLAGS = "-D warnings"
    try {
        & cargo doc -p rift -p rift-types -p rift-ytmusic --no-deps @locked
        if ($LASTEXITCODE -ne 0) { throw "rustdoc warnings/errors above" }
    } finally {
        if ($null -eq $previous) {
            Remove-Item Env:RUSTDOCFLAGS -ErrorAction SilentlyContinue
        } else {
            $env:RUSTDOCFLAGS = $previous
        }
    }
}

# 4. Tests ----------------------------------------------------------------
if ($SkipTests) {
    Write-Log ""
    Write-Log "SKIP: tests (-SkipTests)" Yellow
    Add-Result -Step "cargo test (skipped)" -Status "SKIP" -Detail "-SkipTests"
} else {
    Invoke-Step "cargo test" {
        $packages = $script:packages
        $locked = $script:lockedArgs
        & cargo test @packages @locked
        if ($LASTEXITCODE -ne 0) { throw "test failures above" }
    }
    # The README promises a UI-only build (`--no-default-features`) for
    # renderer/immediate-mode work; test that configuration too instead of
    # re-running the default (backend) one, which step 4 already covers.
    Invoke-Step "cargo test -p rift-desktop --no-default-features" {
        $locked = $script:lockedArgs
        & cargo test -p rift-desktop --no-default-features @locked
        if ($LASTEXITCODE -ne 0) { throw "UI-only feature test failures above" }
    }
}

# 5. Release build + 6. screenshot acceptance -----------------------------
if ($SkipRelease) {
    Write-Log ""
    Write-Log "SKIP: release build + screenshot (-SkipRelease)" Yellow
    Add-Result -Step "release build (skipped)" -Status "SKIP" -Detail "-SkipRelease"
} else {
    $releaseBinary = Join-Path $root "target/release/rift-music.exe"
    Invoke-Step "release build (-p rift-desktop --features backend)" {
        $locked = $script:lockedArgs
        & cargo build --release -p rift-desktop --features backend @locked
        if ($LASTEXITCODE -ne 0) { throw "release build failed" }
        if (-not (Test-Path -LiteralPath $releaseBinary)) { throw "release binary missing: $releaseBinary" }
        $size = (Get-Item -LiteralPath $releaseBinary).Length
        Write-Log ("binary: {0} ({1:N0} bytes / {2:N1} MiB)" -f $releaseBinary, $size, ($size / 1MB))
    }

    $screenshot = Join-Path $root "artifacts/screens/ci-home.png"
    Invoke-Step "preview mode is stripped from the release build" {
        # The shipped build does not enable the `preview` feature, so the
        # development-only flags must fail at argument parsing. A GUI-subsystem
        # executable still exits with a code when arguments are rejected, so
        # Start-Process -Wait + redirected stderr gives an actionable check.
        $stdout = Join-Path $root "artifacts/screens/preview-stripped.stdout.log"
        $stderr = Join-Path $root "artifacts/screens/preview-stripped.stderr.log"
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $stdout) | Out-Null
        Remove-Item -LiteralPath $stdout, $stderr -ErrorAction SilentlyContinue
        $process = Start-Process -FilePath $releaseBinary -ArgumentList @("--preview") -Wait -PassThru -NoNewWindow `
            -RedirectStandardOutput $stdout -RedirectStandardError $stderr
        $text = ((Get-Content -LiteralPath $stdout, $stderr -ErrorAction SilentlyContinue) -join "`n").Trim()
        Write-Log $text
        if ($process.ExitCode -eq 0) { throw "release binary accepted --preview (preview mode is shipping)" }
        if ($text -notmatch "development-only") { throw "release binary rejected --preview without the expected development-only message" }
    }

    Invoke-Step "offscreen release screenshot" {
        # The gate's screenshot acceptance needs the development-only preview
        # data. Build the acceptance-only release binary with the explicit
        # `preview` feature (the shipped configuration is built and checked
        # above; the release workflow never enables this feature).
        $locked = $script:lockedArgs
        & cargo build --release -p rift-desktop --features backend,preview @locked
        if ($LASTEXITCODE -ne 0) { throw "preview-enabled acceptance build failed" }
        # The release binary is a GUI-subsystem executable, so a pipeline `&`
        # invocation can return before the process actually exits; and a second
        # instance is refused by the single-instance guard (it exits 0 without
        # writing the PNG). Start-Process -Wait handles the first, and the
        # pre-check turns the second into an actionable failure instead of a
        # mysterious missing file.
        $running = @(Get-Process -Name "rift-music" -ErrorAction SilentlyContinue)
        if ($running.Count -gt 0) {
            $pids = ($running | ForEach-Object { $_.Id }) -join ", "
            throw "another rift-music instance is running (PID $pids); the single-instance guard makes an offscreen capture exit without writing the PNG. Close it and re-run."
        }
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $screenshot) | Out-Null
        Remove-Item -LiteralPath $screenshot -ErrorAction SilentlyContinue
        $stdout = Join-Path $root "artifacts/screens/ci-home.stdout.log"
        $stderr = Join-Path $root "artifacts/screens/ci-home.stderr.log"
        $arguments = @("--preview", "--screenshot", $screenshot, "--size", "1160x740", "--view", "home")
        $process = Start-Process -FilePath $releaseBinary -ArgumentList $arguments -Wait -PassThru -NoNewWindow `
            -RedirectStandardOutput $stdout -RedirectStandardError $stderr
        foreach ($capture in @($stdout, $stderr)) {
            if (Test-Path -LiteralPath $capture) {
                Write-Log "--- $([System.IO.Path]::GetFileName($capture)) ---"
                Get-Content -LiteralPath $capture | ForEach-Object { Write-Log $_ }
            }
        }
        if ($process.ExitCode -ne 0) { throw "release binary exited $($process.ExitCode)" }
        if (-not (Test-Path -LiteralPath $screenshot)) { throw "screenshot missing: $screenshot" }
        $png = Get-Item -LiteralPath $screenshot
        Write-Log "screenshot: $($png.FullName) ($($png.Length) bytes)"
        # Leave target/release holding the shipped configuration again, so a
        # later hand-run of the binary does not silently accept preview flags.
        & cargo build --release -p rift-desktop --features backend @locked
        if ($LASTEXITCODE -ne 0) { throw "could not restore the shipped release build after the acceptance capture" }
        Write-Log "restored shipped release build (preview feature disabled)"
    }

    $reference = Join-Path $root "tools/reference-old-ui.png"
    if (-not $script:python) {
        Write-Log ""
        Write-Log "SKIP: screenshot vs reference (no Python with Pillow+numpy found; set RIFT_PYTHON)" Yellow
        Add-Result -Step "screenshot vs reference (skipped)" -Status "SKIP" -Detail "no Pillow+numpy Python"
    } else {
        Invoke-Step "screenshot vs reference (metrics only)" {
            if (-not (Test-Path -LiteralPath $reference)) {
                throw "reference image missing: $reference"
            }
            $python = $script:python
            $prefix = $python.Prefix
            & $python.Exe @prefix tools/compare_images.py $reference $screenshot --label ci-home
            if ($LASTEXITCODE -ne 0) { throw "compare_images.py exited $LASTEXITCODE" }
        } -Kind Soft
    }
}

# 7. Legacy-dependency guards ---------------------------------------------
# The forbidden dependency names are assembled from fragments so the retired
# framework's brand does not appear in the project text; the guard itself stays.
$legacyPrefix = ("tau" + "ri|y" + "ew|wasm-bindgen|wasm_bindgen")
Invoke-Step "no legacy webview crates in the app dependency tree" {
    $locked = $script:lockedArgs
    $tree = & cargo tree -p rift-desktop @locked 2>&1
    if ($LASTEXITCODE -ne 0) { throw "cargo tree failed" }
    $legacyHits = @($tree | Select-String -Pattern $legacyPrefix)
    if ($legacyHits.Count -gt 0) {
        $legacyHits | ForEach-Object { Write-Log $_.Line Red }
        throw "the app dependency tree still contains legacy webview crates"
    }
    Write-Log "cargo tree -p rift-desktop: no legacy webview crates"
}

Invoke-Step "no legacy webview/WASM manifests in the workspace" {
    # `git ls-files` keeps this fast and respects .gitignore (no target/ or
    # node_modules/ traversal); fall back to a bounded recursive scan if git is
    # unavailable.
    $manifests = @()
    if (Get-Command git -ErrorAction SilentlyContinue) {
        $manifests = @(& git ls-files --cached --others --exclude-standard -- "*Cargo.toml" |
            ForEach-Object { Join-Path $root $_ } |
            Where-Object { $_ -notmatch "[\\/]ui[\\/]" } |
            Where-Object { Test-Path -LiteralPath $_ })
    } else {
        $manifests = @(Get-ChildItem -Path $root -Filter "Cargo.toml" -Recurse -File | Where-Object {
                $_.FullName -notmatch "[\\/]target[^\\/]*[\\/]" -and
                $_.FullName -notmatch "[\\/]node_modules[\\/]" -and
                $_.FullName -notmatch "[\\/]ui[\\/]"
            } | ForEach-Object { $_.FullName })
    }
    $bad = [System.Collections.Generic.List[string]]::new()
    foreach ($manifest in $manifests) {
        $lineNumber = 0
        foreach ($line in Get-Content -LiteralPath $manifest) {
            $lineNumber++
            $code = $line -replace "#.*$", ""
            if ($code -match '^\s*"?(?<key>[A-Za-z0-9_.-]+)"?\s*=') {
                $key = $matches["key"].ToLowerInvariant()
                if ($key -match ("^(" + $legacyPrefix + ")")) {
                    $bad.Add(("{0}:{1}: {2}" -f $manifest, $lineNumber, $line.Trim()))
                }
            }
        }
    }
    if ($bad.Count -gt 0) {
        $bad | ForEach-Object { Write-Log $_ Red }
        throw "forbidden dependency declarations found (see lines above)"
    }
    Write-Log "scanned $($manifests.Count) workspace Cargo.toml file(s): clean"
}

# 8. Duplicate-dependency baseline ---------------------------------------
if ($script:gitBash) {
    Invoke-Step "duplicate dependency baseline" {
        & $script:gitBash .github/scripts/check-duplicate-deps.sh
        if ($LASTEXITCODE -ne 0) { throw "duplicate-dependency guard failed" }
    }
} else {
    Write-Log ""
    Write-Log "SKIP: duplicate dependency baseline (Git Bash not available)" Yellow
    Add-Result -Step "duplicate dependency baseline" -Status "SKIP" -Detail "Git Bash not available"
}

# 9. Supply-chain checks ---------------------------------------------------
# The CI `deny` job is the contract; locally the tool is optional. When
# `cargo-deny` is on PATH its failure is hard (the gate must not pass with a
# known-vulnerable or license/ban/source-violating dependency). When it is not
# installed, say SKIP instead of failing, matching how the Git Bash steps
# degrade on a machine that cannot run them.
if (-not (Get-Command cargo-deny -ErrorAction SilentlyContinue)) {
    Write-Log ""
    Write-Log "SKIP: supply-chain checks (cargo-deny not installed; `cargo install --locked cargo-deny` to enable)" Yellow
    Add-Result -Step "cargo deny check (skipped)" -Status "SKIP" -Detail "cargo-deny not installed"
} else {
    Invoke-Step "cargo deny check all" {
        # The policy, target/feature graph and license allow-list come from
        # `deny.toml`; no extra flags here so CI and this gate run the same
        # configuration.
        & cargo deny check all
        if ($LASTEXITCODE -ne 0) {
            throw "cargo-deny reported a vulnerability, license, ban or source problem (see diagnostics above and deny.toml)"
        }
    }
}

# Final summary / exit code ----------------------------------------------
Write-Summary
$failures = @($script:results | Where-Object Status -eq "FAIL")
if ($failures.Count -gt 0) {
    exit 1
}
exit 0
