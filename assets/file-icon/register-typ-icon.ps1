param(
    [Parameter(Mandatory = $true)][string]$IconPath,
    [string]$BackupRoot,
    [string]$ExePath
)

$ErrorActionPreference = "Stop"
if ([string]::IsNullOrEmpty($BackupRoot)) {
    $scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
    $BackupRoot = Join-Path $scriptDir "registry-backup"
}
if (-not (Test-Path -LiteralPath $IconPath)) { throw "icon not found: $IconPath" }

# Resolve the editor executable. Default: <repo root>\target\release\typst-editor.exe
# (script lives in assets\file-icon, so the repo root is two levels up from $PSScriptRoot).
if ([string]::IsNullOrEmpty($ExePath)) {
    $repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
    $ExePath = Join-Path $repoRoot "target\release\typst-editor.exe"
}
if (-not (Test-Path -LiteralPath $ExePath)) {
    throw "editor executable not found: $ExePath (build it or pass -ExePath)"
}
$ExePath = (Resolve-Path -LiteralPath $ExePath).Path

$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$backup = Join-Path $BackupRoot $stamp
New-Item -ItemType Directory -Force -Path $backup | Out-Null

reg export "HKCU\Software\Classes" (Join-Path $backup "HKCU-Software-Classes.reg") /y | Out-Null
reg export "HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.typ" (Join-Path $backup "FileExts-.typ.reg") /y | Out-Null
$notes = foreach ($probe in @(
        "HKCU\Software\Classes\.typ",
        "HKCU\Software\Classes\Typst.Document",
        "HKCU\Software\Classes\Applications\typst-editor.exe",
        "HKCU\Software\TypstEditor\Capabilities",
        "HKCU\Software\RegisteredApplications",
        "HKLM\Software\Classes\.typ",
        "HKLM\Software\Classes\Typst.Document"
    )) {
    $psPath = if ($probe.StartsWith("HKCU\")) { "HKCU:\" + $probe.Substring(5) }
              elseif ($probe.StartsWith("HKLM\")) { "HKLM:\" + $probe.Substring(5) }
              else { $probe }
    if (Test-Path -LiteralPath $psPath) { "$probe : EXISTED (before registration)" } else { "$probe : absent (before registration)" }
}
$notes | Set-Content -Encoding UTF8 (Join-Path $backup "pre-state.txt")

$destDir = Join-Path $env:LOCALAPPDATA "typ-edit"
New-Item -ItemType Directory -Force -Path $destDir | Out-Null
$dest = Join-Path $destDir "typst.ico"
Copy-Item -LiteralPath $IconPath -Destination $dest -Force
$dest = (Resolve-Path -LiteralPath $dest).Path

# --- small registry helpers (writes only under HKCU) ---
function Set-DefaultValue {
    param([string]$Key, [string]$Value)
    if (-not (Test-Path -LiteralPath $Key)) { New-Item -Path $Key -Force | Out-Null }
    Set-Item -LiteralPath $Key -Value $Value
}
function Set-StringValue {
    param([string]$Key, [string]$Name, [string]$Value)
    if (-not (Test-Path -LiteralPath $Key)) { New-Item -Path $Key -Force | Out-Null }
    New-ItemProperty -LiteralPath $Key -Name $Name -Value $Value -PropertyType String -Force | Out-Null
}

$classes = "HKCU:\Software\Classes"
$progId = "Typst.Document"
$extKey = Join-Path $classes ".typ"
$progIdKey = Join-Path $classes $progId
$appKey = Join-Path $classes "Applications\typst-editor.exe"
$command = '"{0}" "%1"' -f $ExePath

# Programmatic identifier (ProgID) for the document type: covers the no-UserChoice path.
Set-DefaultValue -Key $progIdKey -Value "Typst 文档"
Set-DefaultValue -Key (Join-Path $progIdKey "DefaultIcon") -Value $dest
Set-DefaultValue -Key (Join-Path $progIdKey "shell\open\command") -Value $command

# Associate the .typ extension with the ProgID.
Set-DefaultValue -Key $extKey -Value $progId
Set-StringValue -Key (Join-Path $extKey "OpenWithProgids") -Name $progId -Value ""
# Legacy: the extension-level DefaultIcon is NOT an association-array element and is
# effectively ignored by the shell, but keep it for compatibility.
Set-DefaultValue -Key (Join-Path $extKey "DefaultIcon") -Value $dest

# KEY FIX: the effective ProgID here is Applications\typst-editor.exe (per
# FileExts\.typ\UserChoiceLatest), and the shell reads its DefaultIcon FIRST.
Set-DefaultValue -Key (Join-Path $appKey "DefaultIcon") -Value $dest
Set-DefaultValue -Key $appKey -Value "Typst Editor"
Set-StringValue -Key $appKey -Name "FriendlyAppName" -Value "Typst Editor"
Set-DefaultValue -Key (Join-Path $appKey "shell\open\command") -Value $command
Set-StringValue -Key (Join-Path $appKey "SupportedTypes") -Name ".typ" -Value ""

# Default-apps listing (Settings > Default apps).
$caps = "HKCU:\Software\TypstEditor\Capabilities"
Set-DefaultValue -Key $caps -Value "Typst Editor"
Set-StringValue -Key $caps -Name "ApplicationName" -Value "Typst Editor"
Set-StringValue -Key $caps -Name "ApplicationDescription" -Value "Typst 桌面编辑器"
Set-StringValue -Key (Join-Path $caps "FileAssociations") -Name ".typ" -Value $progId
Set-StringValue -Key "HKCU:\Software\RegisteredApplications" -Name "TypstEditor" -Value "Software\TypstEditor\Capabilities"

# UserChoice/UserChoiceLatest are hash-protected: detect + warn, never write, never fail.
$acceptedProgIds = @("Applications\typst-editor.exe", $progId)
$warnUserChoice = $false
foreach ($ucName in @("UserChoice", "UserChoiceLatest")) {
    $ucKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.typ\$ucName"
    if (Test-Path -LiteralPath $ucKey) {
        $chosen = (Get-ItemProperty -LiteralPath $ucKey -Name "ProgId" -ErrorAction SilentlyContinue).ProgId
        if ([string]::IsNullOrEmpty($chosen)) {
            # UserChoiceLatest stores the ProgID in a child key "ProgId" whose value is also "ProgId".
            $childKey = Join-Path $ucKey "ProgId"
            if (Test-Path -LiteralPath $childKey) {
                $chosen = (Get-ItemProperty -LiteralPath $childKey -Name "ProgId" -ErrorAction SilentlyContinue).ProgId
            }
        }
        if ($acceptedProgIds -notcontains $chosen) {
            Write-Warning ("{0} for .typ points at '{1}' (expected Applications\typst-editor.exe or {2})." -f $ucName, $chosen, $progId)
            Write-Warning "UserChoice is hash-protected and cannot be changed by this script. If .typ does not open with Typst Editor, re-choose via 打开方式 -> 始终使用."
            $warnUserChoice = $true
        } else {
            Write-Output ("userchoice: {0} = {1} (ok)" -f $ucName, $chosen)
        }
    }
}

# Tell the shell associations changed (no Explorer restart, no cache deletion).
if (-not ("Native.Shell32Refresh" -as [type])) {
    Add-Type -Namespace Native -Name Shell32Refresh -MemberDefinition @'
[System.Runtime.InteropServices.DllImport("shell32.dll")]
public static extern void SHChangeNotify(int wEventId, uint uFlags, System.IntPtr dwItem1, System.IntPtr dwItem2);
'@
}
[Native.Shell32Refresh]::SHChangeNotify(0x08000000, 0x0000, [IntPtr]::Zero, [IntPtr]::Zero)  # SHCNE_ASSOCCHANGED, SHCNF_IDLIST
Start-Process -FilePath "ie4uinit.exe" -ArgumentList "-show" -WindowStyle Hidden -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 800

Write-Output "registered: .typ -> $progId"
Write-Output "open:       $command"
Write-Output "icon:       $dest"
Write-Output "appicon:    Applications\typst-editor.exe\DefaultIcon -> $dest"
Write-Output "app:        Applications\typst-editor.exe (FriendlyAppName: Typst Editor)"
Write-Output "caps:       HKCU\Software\TypstEditor\Capabilities (RegisteredApplications: TypstEditor)"
if ($warnUserChoice) { Write-Output "warning:    UserChoice/UserChoiceLatest for .typ point elsewhere (see above)" }
Write-Output "backup:     $backup"
