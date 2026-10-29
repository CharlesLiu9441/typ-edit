$ErrorActionPreference = "Stop"

$classes = "HKCU:\Software\Classes"
$progId = "Typst.Document"

# Remove the application key (Open-With entry, holds our DefaultIcon fix).
Remove-Item -LiteralPath (Join-Path $classes "Applications\typst-editor.exe") -Recurse -Force -ErrorAction SilentlyContinue

# Remove the ProgID.
Remove-Item -LiteralPath (Join-Path $classes $progId) -Recurse -Force -ErrorAction SilentlyContinue

# Remove our value from .typ\OpenWithProgids, then the key if it becomes empty.
$extKey = Join-Path $classes ".typ"
$owpKey = Join-Path $extKey "OpenWithProgids"
if (Test-Path -LiteralPath $owpKey) {
    Remove-ItemProperty -LiteralPath $owpKey -Name $progId -Force -ErrorAction SilentlyContinue
    $leftover = @(Get-ChildItem -LiteralPath $owpKey -ErrorAction SilentlyContinue)
    if ($leftover.Count -eq 0) { Remove-Item -LiteralPath $owpKey -Force -ErrorAction SilentlyContinue }
}

# Remove the legacy extension-level DefaultIcon, then .typ only if truly empty.
if (Test-Path -LiteralPath $extKey) {
    Remove-Item -LiteralPath (Join-Path $extKey "DefaultIcon") -Recurse -Force -ErrorAction SilentlyContinue
    $item = Get-Item -LiteralPath $extKey -ErrorAction SilentlyContinue
    if ($item) {
        $subkeys = @(Get-ChildItem -LiteralPath $extKey -ErrorAction SilentlyContinue)
        $values = @($item.GetValueNames())
        if ($subkeys.Count -eq 0 -and $values.Count -eq 0) {
            Remove-Item -LiteralPath $extKey -Force -ErrorAction SilentlyContinue
        }
    }
}

# Remove the default-apps capabilities and its RegisteredApplications entry.
Remove-Item -LiteralPath "HKCU:\Software\TypstEditor" -Recurse -Force -ErrorAction SilentlyContinue
$raKey = "HKCU:\Software\RegisteredApplications"
if (Test-Path -LiteralPath $raKey) {
    Remove-ItemProperty -LiteralPath $raKey -Name "TypstEditor" -Force -ErrorAction SilentlyContinue
    $item = Get-Item -LiteralPath $raKey -ErrorAction SilentlyContinue
    if ($item) {
        $subkeys = @(Get-ChildItem -LiteralPath $raKey -ErrorAction SilentlyContinue)
        $values = @($item.GetValueNames())
        if ($subkeys.Count -eq 0 -and $values.Count -eq 0) {
            Remove-Item -LiteralPath $raKey -Force -ErrorAction SilentlyContinue
        }
    }
}

# Remove the installed icon copy.
Remove-Item -LiteralPath (Join-Path $env:LOCALAPPDATA "typ-edit\typst.ico") -Force -ErrorAction SilentlyContinue

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

Write-Output "unregistered: .typ icon bindings removed"
Write-Output "backups (untouched): registry-backup under the script directory"
