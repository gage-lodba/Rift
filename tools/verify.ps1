# Reference verification suite for the Rift → Vulkan conversion.
#
# Usage: pwsh tools/verify.ps1 [-SkipScreens] [-SkipTests]
#
# Runs, from the repo root:
#   1. cargo fmt --all -- --check            (formatting)
#   2. cargo check -p rift-desktop --no-default-features
#                                             (UI/renderer compile only; the
#                                              default features include backend)
#   3. cargo check -p rift-desktop --features backend  (full app compile)
#   4. cargo test -p rift                    (backend unit/integration tests)
#   5. cargo test -p rift-desktop            (state/controller logic tests)
#   6. cargo run -p rift-desktop -- --preview --screenshot ... for every view
#   7. python tools/compare_images.py docs/vulkan-migration/reference-old-ui.png artifacts/screens/home.png
#
# Exits non-zero on the first failing step (except the screenshot comparisons,
# which report only).
param(
    [switch]$SkipScreens,
    [switch]$SkipTests
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$failed = @()

function Step([string]$Name, [scriptblock]$Body) {
    Write-Host ""
    Write-Host "===== $Name =====" -ForegroundColor Cyan
    try {
        & $Body
        if ($LASTEXITCODE -ne 0) { throw "exit code $LASTEXITCODE" }
        Write-Host "PASS: $Name" -ForegroundColor Green
    } catch {
        Write-Host "FAIL: $Name — $_" -ForegroundColor Red
        $script:failed += $Name
    }
}

Step "fmt" { cargo fmt --all -- --check }
Step "check (no backend feature)" { cargo check -p rift-desktop --no-default-features }
Step "check (with backend feature)" { cargo check -p rift-desktop --features backend }
if (-not $SkipTests) {
    Step "test backend" { cargo test -p rift }
    Step "test app" { cargo test -p rift-desktop }
}

if (-not $SkipScreens) {
    $out = "artifacts/screens"
    New-Item -ItemType Directory -Force -Path $out | Out-Null
    $views = @("home", "liked", "playlist", "search", "artist", "artist-songs", "album", "settings")
    foreach ($view in $views) {
        Step "screenshot $view" {
            cargo run -q -p rift-desktop -- --preview --screenshot "$out/$view.png" --size 1160x740 --view $view
        }
    }
    if (Test-Path "docs/vulkan-migration/reference-old-ui.png") {
        Write-Host ""
        Write-Host "===== compare home against the archived old-UI reference =====" -ForegroundColor Cyan
        python tools/compare_images.py docs/vulkan-migration/reference-old-ui.png "$out/home.png" --label home || $true
    }
}

Write-Host ""
if ($failed.Count -eq 0) {
    Write-Host "ALL CHECKS PASSED" -ForegroundColor Green
    exit 0
}
Write-Host "FAILED: $($failed -join ', ')" -ForegroundColor Red
exit 1
