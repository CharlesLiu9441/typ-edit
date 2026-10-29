<#
.SYNOPSIS
  Build the Windows release installers and the macOS app/DMG from one command.

.DESCRIPTION
  Windows: runs scripts/release.ps1, builds the MSI with WiX, then wraps the
  MSI in an NSIS installer EXE. Both installers go to dist/windows.
  macOS: invokes scripts/macos/build-macos.sh inside WSL in release mode,
  with the styled DMG, and writes to dist/macos.

  WiX 4.0.6 is installed into a user-local cache on first use. NSIS 3.x
  (makensis.exe) must be installed or discoverable. No release build is
  performed by -DryRun.

.PARAMETER MacArch
  macOS architecture: aarch64 (default), x86_64, or universal.

.PARAMETER Version
  Installer version. Defaults to the version in Cargo.toml.

.PARAMETER SkipWindows
  Skip the Windows release build and both Windows installers.

.PARAMETER SkipMac
  Skip the WSL macOS build and DMG.

.PARAMETER DryRun
  Print the resolved paths and commands without building or installing tools.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts/release-all.ps1

.EXAMPLE
  scripts/release-all.bat -DryRun

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts/release-all.ps1 -SkipMac
#>
[CmdletBinding()]
param(
    [ValidateSet("aarch64", "x86_64", "universal")]
    [string]$MacArch = "aarch64",

    [string]$Version = "",

    [switch]$SkipWindows,

    [switch]$SkipMac,

    [switch]$DryRun
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $Root

$WixToolVersion = "4.0.6"
$ToolCacheRoot = if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
    Join-Path $HOME ".cache/typ-edit"
} else {
    Join-Path $env:LOCALAPPDATA "typ-edit"
}
$WixToolDir = Join-Path $ToolCacheRoot "wix"
$Wix = Join-Path $WixToolDir "wix.exe"
$DistWindows = Join-Path $Root "dist/windows"
$DistMac = Join-Path $Root "dist/macos"
$WinExe = Join-Path $Root "target/release/typst-editor.exe"
$Icon = Join-Path $Root "assets/app-icon/typ-edit-icon.ico"
$MsiSource = Join-Path $Root "packaging/windows/typ-edit.wxs"
$NsiSource = Join-Path $Root "packaging/windows/installer.nsi"
$WelcomeBmp = Join-Path $Root "packaging/windows/nsis/welcome.bmp"
$HeaderBmp = Join-Path $Root "packaging/windows/nsis/header.bmp"
$ReleaseScript = Join-Path $Root "scripts/release.ps1"

function Write-Step {
    param([string]$Message)
    Write-Host "==> $Message" -ForegroundColor Cyan
}

function Assert-File {
    param(
        [string]$Path,
        [string]$Label
    )
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Label not found: $Path"
    }
}

function Resolve-Version {
    if ([string]::IsNullOrWhiteSpace($Version)) {
        $cargoToml = Join-Path $Root "Cargo.toml"
        Assert-File -Path $cargoToml -Label "Cargo.toml"
        $match = Select-String -LiteralPath $cargoToml -Pattern '^version\s*=\s*"([^"]+)"' |
            Select-Object -First 1
        if ($null -eq $match) {
            throw "Could not read the package version from $cargoToml"
        }
        $script:Version = $match.Matches[0].Groups[1].Value
    }
    if ($script:Version -notmatch '^\d+\.\d+\.\d+$') {
        throw "Version must be three numeric parts (for example 0.1.0), got: $script:Version"
    }
}

function Get-WixVersion {
    if (-not (Test-Path -LiteralPath $Wix -PathType Leaf)) {
        return $null
    }
    $output = & $Wix --version
    if ($LASTEXITCODE -ne 0) {
        return $null
    }
    return (($output -join " ").Trim())
}

function Ensure-Wix {
    $current = Get-WixVersion
    if ($current -and $current -match '^4\.0\.6') {
        return
    }
    if (Test-Path -LiteralPath $WixToolDir) {
        Remove-Item -LiteralPath $WixToolDir -Recurse -Force
    }
    $dotnet = Get-Command dotnet -ErrorAction SilentlyContinue
    if ($null -eq $dotnet) {
        throw "dotnet was not found; install the .NET SDK to bootstrap WiX $WixToolVersion"
    }
    New-Item -ItemType Directory -Force -Path $WixToolDir | Out-Null
    Write-Step "Installing WiX $WixToolVersion into $WixToolDir"
    & $dotnet.Source tool install --tool-path $WixToolDir wix --version $WixToolVersion
    if ($LASTEXITCODE -ne 0) {
        throw "dotnet tool install failed with exit code $LASTEXITCODE"
    }
    $current = Get-WixVersion
    if (-not $current -or $current -notmatch '^4\.0\.6') {
        throw "WiX $WixToolVersion was not available after installation"
    }
}

function Find-Nsis {
    if (-not [string]::IsNullOrWhiteSpace($env:NSISDIR)) {
        $nsisDirPath = Join-Path $env:NSISDIR "makensis.exe"
        if (Test-Path -LiteralPath $nsisDirPath -PathType Leaf) {
            return $nsisDirPath
        }
    }
    $command = Get-Command makensis.exe -ErrorAction SilentlyContinue
    if ($null -ne $command) {
        return $command.Source
    }
    $candidates = @(
        (Join-Path $ToolCacheRoot "nsis/makensis.exe"),
        "C:\Program Files (x86)\NSIS\makensis.exe",
        "C:\Program Files\NSIS\makensis.exe"
    )
    foreach ($candidate in $candidates) {
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            return $candidate
        }
    }
    throw "NSIS makensis.exe was not found. Install NSIS 3.x or add it to PATH."
}

function Invoke-Native {
    param(
        [Parameter(Mandatory = $true)]
        [string]$FilePath,
        [Parameter(Mandatory = $true)]
        [string[]]$Arguments,
        [string]$Label = ""
    )
    if ([string]::IsNullOrWhiteSpace($Label)) {
        $Label = $FilePath
    }
    & $FilePath @Arguments | Out-Host
    if ($LASTEXITCODE -ne 0) {
        throw "$Label failed with exit code $LASTEXITCODE"
    }
}

function Get-WslRoot {
    $wsl = Get-Command wsl.exe -ErrorAction SilentlyContinue
    if ($null -eq $wsl) {
        throw "wsl.exe was not found; install WSL to build the macOS artifacts"
    }
    $converted = & $wsl.Source -e wslpath -a $Root 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "wslpath failed with exit code $LASTEXITCODE"
    }
    $path = ($converted | Select-Object -Last 1).ToString().Trim()
    if ([string]::IsNullOrWhiteSpace($path)) {
        throw "wslpath returned an empty Linux path"
    }
    return $path
}

function Build-WindowsInstallers {
    param([string]$NsisPath)

    Write-Step "Building Windows release"
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $ReleaseScript | Out-Host
    if ($LASTEXITCODE -ne 0) {
        throw "scripts/release.ps1 failed with exit code $LASTEXITCODE"
    }
    Assert-File -Path $WinExe -Label "Windows release executable"
    Assert-File -Path $Icon -Label "Windows icon"

    New-Item -ItemType Directory -Force -Path $DistWindows | Out-Null
    Get-ChildItem -LiteralPath $DistWindows -Filter "Typst-Editor-*" -File -ErrorAction SilentlyContinue |
        Remove-Item -Force

    $msi = Join-Path $DistWindows "Typst-Editor-$Version-x64.msi"
    $installer = Join-Path $DistWindows "Typst-Editor-$Version-x64.exe"
    $buildOptions = @(
        "build",
        "-arch", "x64",
        "-pdbtype", "none",
        "-d", "Version=$Version"
    )

    Write-Step "Building MSI"
    Invoke-Native -FilePath $Wix -Arguments ($buildOptions + @(
        "-d", "SourceExe=$WinExe",
        "-d", "IconPath=$Icon",
        "-o", $msi,
        $MsiSource
    )) -Label "WiX MSI"
    Assert-File -Path $msi -Label "MSI installer"

    Write-Step "Building NSIS installer EXE"
    Invoke-Native -FilePath $NsisPath -Arguments @(
        "/V2",
        "/DMSI_PATH=$msi",
        "/DVERSION=$Version",
        "/DOUTPUT_EXE=$installer",
        "/DAPP_ICON=$Icon",
        "/DHEADER_BMP=$HeaderBmp",
        "/DWELCOME_BMP=$WelcomeBmp",
        $NsiSource
    ) -Label "NSIS"
    Assert-File -Path $installer -Label "NSIS installer EXE"

    return [pscustomobject]@{
        Msi = $msi
        Exe = $installer
    }
}

function Build-MacDmg {
    param([string]$WslRoot)

    $command = "set -euo pipefail; cd `"$WslRoot`"; exec bash scripts/macos/build-macos.sh --arch $MacArch --profile release --package --dmg --clean"
    Write-Step "Building macOS app and DMG in WSL ($MacArch)"
    Invoke-Native -FilePath "wsl.exe" -Arguments @("-e", "bash", "-lc", $command) -Label "WSL macOS build"

    $dmg = Join-Path $DistMac "Typst Editor.dmg"
    Assert-File -Path $dmg -Label "macOS DMG"
    return $dmg
}

function Write-DryRun {
    Write-Step "Dry run: no build and no tool installation"
    Write-Host "Repository: $Root"
    Write-Host "Version:    $Version"

    if (-not $SkipWindows) {
        $wixVersion = Get-WixVersion
        if ($wixVersion) {
            $wixState = "present ($wixVersion)"
        }
        else {
            $wixState = "missing (will install WiX $WixToolVersion into $WixToolDir)"
        }
        try {
            $nsisPath = Find-Nsis
            $nsisState = "present ($nsisPath)"
        }
        catch {
            $nsisState = "missing (install NSIS 3.x / makensis.exe)"
        }
        Write-Host "Windows build: powershell -File scripts/release.ps1"
        Write-Host "WiX:          $wixState"
        Write-Host "NSIS:         $nsisState"
        Write-Host "MSI:          $(Join-Path $DistWindows "Typst-Editor-$Version-x64.msi")"
        Write-Host "EXE:          $(Join-Path $DistWindows "Typst-Editor-$Version-x64.exe")"
    }

    if (-not $SkipMac) {
        try {
            $wslRoot = Get-WslRoot
            Write-Host "WSL root:     $wslRoot"
            Write-Host "macOS build:  bash scripts/macos/build-macos.sh --arch $MacArch --profile release --package --dmg --clean"
            Write-Host "DMG:          $(Join-Path $DistMac "Typst Editor.dmg")"
        }
        catch {
            Write-Host "WSL:          unavailable ($($_.Exception.Message))"
        }
    }
}

try {
    if ($SkipWindows -and $SkipMac) {
        throw "Both -SkipWindows and -SkipMac were specified; nothing to do."
    }

    Resolve-Version

    if ($DryRun) {
        Write-DryRun
        exit 0
    }

    Assert-File -Path $MsiSource -Label "WiX MSI source"
    Assert-File -Path $NsiSource -Label "NSIS installer source"
    Assert-File -Path $WelcomeBmp -Label "NSIS welcome bitmap"
    Assert-File -Path $HeaderBmp -Label "NSIS header bitmap"
    Assert-File -Path $ReleaseScript -Label "Windows release wrapper"

    Write-Step "Typst Editor release pipeline (version $Version)"

    $windowsResult = $null
    if (-not $SkipWindows) {
        Ensure-Wix
        $nsisPath = Find-Nsis
        $windowsResult = Build-WindowsInstallers -NsisPath $nsisPath
    }

    $macDmg = $null
    if (-not $SkipMac) {
        $wslRoot = Get-WslRoot
        $macDmg = Build-MacDmg -WslRoot $wslRoot
    }

    Write-Step "Artifacts"
    if ($null -ne $windowsResult) {
        Write-Host "Windows MSI: $($windowsResult.Msi)"
        Write-Host "Windows EXE: $($windowsResult.Exe)"
    }
    if ($macDmg) {
        Write-Host "macOS DMG:   $macDmg"
    }
    Write-Host "Development artifacts stay in: $(Join-Path $Root 'dev-dist')"
}
catch {
    [Console]::Error.WriteLine("release-all failed: $($_.Exception.Message)")
    exit 1
}
