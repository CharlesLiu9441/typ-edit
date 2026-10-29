<#
  typ-edit app icon pipeline  (idempotent)
  ------------------------------------------------------------------
  SVG master  ->  Edge headless screenshot (1024px, transparent)
              ->  ImageMagick 7 downscale 256..16 + 16px x10 point zoom
              ->  multi-size ICO (concept A)
              ->  Edge headless screenshot of preview.html (preview sheet)

  Run:  powershell -ExecutionPolicy Bypass -File .\render.ps1
  Everything is regenerated from icon-*.svg; no other inputs are touched.
#>
param([string]$Dir = $PSScriptRoot)

$Edge   = "C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"
$Magick = (Get-Command magick -ErrorAction SilentlyContinue).Source
if (-not $Magick) { $Magick = "C:\Program Files\ImageMagick-7.1.2-Q16-HDRI\magick.exe" }
$Profile = Join-Path $env:TEMP "typ-edit-icon-edge"

if (-not (Test-Path $Edge))   { throw "Edge not found: $Edge" }
if (-not (Test-Path $Magick)) { throw "ImageMagick not found: $Magick" }

# Native calls in PS 5.1: their stderr must not trip $ErrorActionPreference.
function Invoke-Native {
  param([string]$Exe, [string[]]$Arguments)
  $prev = $ErrorActionPreference
  $ErrorActionPreference = "Continue"
  & $Exe @Arguments 2>&1 | Out-Null
  $code = $LASTEXITCODE
  $ErrorActionPreference = $prev
  return $code
}

# Edge headless screenshot. -Transparent keeps the page background clear (SVG icons).
function Invoke-EdgeShot {
  param([string]$Url, [string]$Out, [int]$W, [int]$H, [switch]$Transparent)
  $edgeArgs = @(
    "--headless=new", "--disable-gpu", "--no-first-run", "--user-data-dir=$Profile",
    "--hide-scrollbars", "--force-device-scale-factor=1", "--window-size=$W,$H",
    "--screenshot=$Out"
  )
  if ($Transparent) { $edgeArgs += "--default-background-color=00000000" }
  $edgeArgs += $Url
  Remove-Item $Out -Force -ErrorAction SilentlyContinue   # so Test-Path detects real failure
  Invoke-Native $Edge $edgeArgs | Out-Null
  if (-not (Test-Path $Out)) {                     # one retry (stale profile lock etc.)
    Start-Sleep -Milliseconds 800
    Invoke-Native $Edge $edgeArgs | Out-Null
  }
  if (-not (Test-Path $Out)) { throw "Edge failed to render: $Url" }
}

$Sizes = @(256, 128, 64, 48, 32, 16)

foreach ($svg in (Get-ChildItem -Path $Dir -Filter "icon-*.svg" | Sort-Object Name)) {
  $name = $svg.BaseName -replace '^icon-', ''
  $master = Join-Path $Dir "out-$name-1024.png"

  # 1. 1024 master, transparent background
  Invoke-EdgeShot -Url ("file:///" + ($svg.FullName -replace '\\', '/')) -Out $master -W 1024 -H 1024 -Transparent

  # 2. per-size PNGs (Lanczos) + 16px x10 nearest-neighbour zoom
  foreach ($s in $Sizes) {
    $out = Join-Path $Dir "out-$name-$s.png"
    if ((Invoke-Native $Magick @($master, "-filter", "Lanczos", "-resize", "${s}x${s}", $out)) -ne 0) {
      throw "magick resize ${s}px failed for $name"
    }
  }
  $zoomOut = Join-Path $Dir "out-$name-16-zoom.png"
  if ((Invoke-Native $Magick @((Join-Path $Dir "out-$name-16.png"), "-filter", "point", "-resize", "1000%", $zoomOut)) -ne 0) {
    throw "magick zoom failed for $name"
  }

  Write-Host "rendered $name"
}

# 3. multi-size ICO for the recommended concept A
if ((Invoke-Native $Magick @((Join-Path $Dir "out-a-code-page-1024.png"), "-define", "icon:auto-resize=256,128,64,48,32,16", (Join-Path $Dir "out-a-code-page.ico"))) -ne 0) {
  throw "magick ICO build failed"
}

# 4. preview sheet (opaque background on purpose)
Invoke-EdgeShot -Url ("file:///" + ((Join-Path $Dir "preview.html") -replace '\\', '/')) `
  -Out (Join-Path $Dir "out-preview-sheet.png") -W 1800 -H 1000

Write-Host "done -> $Dir"
