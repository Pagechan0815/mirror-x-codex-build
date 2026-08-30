@echo off
setlocal
cd /d "%~dp0"
set "REPORT=%USERPROFILE%\Desktop\mirror-auth-report.txt"
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0diagnose-mirror-auth.ps1" -ProbeResponses > "%REPORT%" 2>&1
type "%REPORT%"
echo.
echo Report saved to:
echo %REPORT%
echo.
pause

