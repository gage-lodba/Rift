# Launch the real Rift window and capture it to a PNG (proof the Vulkan app
# actually renders through a swapchain on this machine, not just offscreen).
#
# Usage:
#   pwsh tools/window_shot.ps1                     # preview data, default output
#   pwsh tools/window_shot.ps1 -Live               # real backend (network)
#   pwsh tools/window_shot.ps1 -Out artifacts/screens/window.png
param(
    [switch]$Live,
    [string]$Out = "artifacts/screens/window.png",
    [int]$SettleSeconds = 4,
    [int]$Width = 1160,
    [int]$Height = 740
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Out) | Out-Null

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Win32Shot {
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hWnd, IntPtr after, int x, int y, int cx, int cy, uint flags);
}
"@

Write-Host "building rift-music (debug) if needed..."
cargo build -q -p rift-desktop --features backend
if ($LASTEXITCODE -ne 0) { throw "build failed" }

$exe = Join-Path $root "target\debug\rift-music.exe"
if (-not (Test-Path $exe)) { throw "missing $exe" }
$args = @("--size", "${Width}x${Height}")
if (-not $Live) { $args = @("--preview") + $args }

$proc = Start-Process -FilePath $exe -ArgumentList $args -PassThru
try {
    Start-Sleep -Seconds $SettleSeconds
    $proc.Refresh()
    $hwnd = $proc.MainWindowHandle
    if ($hwnd -eq [IntPtr]::Zero) { throw "no main window handle (did startup fail? see logs)" }
    [Win32Shot]::ShowWindow($hwnd, 9) | Out-Null   # SW_RESTORE
    [Win32Shot]::SetForegroundWindow($hwnd) | Out-Null
    Start-Sleep -Milliseconds 600
    $rect = New-Object Win32Shot+RECT
    if (-not [Win32Shot]::GetWindowRect($hwnd, [ref]$rect)) { throw "GetWindowRect failed" }
    $w = $rect.Right - $rect.Left
    $h = $rect.Bottom - $rect.Top
    Write-Host "window rect: $($rect.Left),$($rect.Top) ${w}x${h}"
    $bmp = New-Object System.Drawing.Bitmap $w, $h
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bmp.Size)
    $bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose()
    $bmp.Dispose()
    Write-Host "saved window capture to $Out"
} finally {
    if (-not $proc.HasExited) {
        $proc.CloseMainWindow() | Out-Null
        Start-Sleep -Milliseconds 500
        if (-not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    }
}
