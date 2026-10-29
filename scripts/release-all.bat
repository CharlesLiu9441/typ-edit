@echo off
setlocal
set "SCRIPT=%~dp0release-all.ps1"
if not exist "%SCRIPT%" (
    echo release-all.ps1 not found: %SCRIPT% 1>&2
    exit /b 1
)
where powershell.exe >nul 2>nul
if errorlevel 1 (
    echo powershell.exe not found 1>&2
    exit /b 1
)
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%SCRIPT%" %*
exit /b %ERRORLEVEL%
