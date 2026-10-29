$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

if ($env:CARGO_ENCODED_RUSTFLAGS) {
    [Console]::Error.WriteLine("error: CARGO_ENCODED_RUSTFLAGS takes precedence over RUSTFLAGS; unset it for release builds")
    exit 2
}

$Remap = "--remap-path-prefix=$HOME=/build --remap-path-prefix=$PWD=/src"
if ($env:RUSTFLAGS) {
    $env:RUSTFLAGS = "$($env:RUSTFLAGS) $Remap"
} else {
    $env:RUSTFLAGS = $Remap
}

cargo build --release @args
exit $LASTEXITCODE
