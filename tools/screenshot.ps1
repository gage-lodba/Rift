# Capture Rift screenshots for visual review.
#
# Usage:
#   pwsh tools/screenshot.ps1                 # preview home at 1160x740
#   pwsh tools/screenshot.ps1 -View settings  # a specific preview screen
#   pwsh tools/screenshot.ps1 -Live           # real backend data (needs network)
#   pwsh tools/screenshot.ps1 -Window         # launch the window and grab the
#                                             # desktop region it occupies
#
# Output goes to artifacts/screens/<view>.png. The offscreen `--screenshot`
# path is preferred: it is deterministic, needs no window manager, and renders
# exactly the UI frame.
param(
    [string]$View = "home",
    [switch]$Live,
    [switch]$Window,
    [int]$Width = 1160,
    [int]$Height = 740,
    [string]$OutDir = "artifacts/screens"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$out = Join-Path $OutDir "$View.png"

if ($Window) {
    $exe = Join-Path $root "target\debug\rift-music.exe"
    if (-not (Test-Path $exe)) {
        Write-Host "building rift-music..."
        cargo build -p rift-desktop
    }
    $proc = Start-Process -FilePath $exe -ArgumentList @("--preview") -PassThru
    Start-Sleep -Seconds 3
    Add-Type -AssemblyName System.Windows.Forms
    Add-Type -AssemblyName System.Drawing
    $screen = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bmp = New-Object System.Drawing.Bitmap $screen.Width, $screen.Height
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($screen.X, $screen.Y, 0, 0, $bmp.Size)
    $bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose()
    $bmp.Dispose()
    Stop-Process -Id $proc.Id -ErrorAction SilentlyContinue
    Write-Host "saved desktop capture to $out"
    exit 0
}

$args = @(
    "run", "-p", "rift-desktop", "--",
    "--screenshot", $out,
    "--size", "${Width}x${Height}",
    "--view", $View
)
if (-not $Live) { $args += "--preview" }
& cargo @args
if ($LASTEXITCODE -ne 0) { throw "screenshot render failed" }

if (Test-Path "Previews/image.png") {
    Write-Host ""
    Write-Host "Compare against the reference with:"
    Write-Host "  python tools/compare_images.py Previews/image.png $out --label $View"
}
