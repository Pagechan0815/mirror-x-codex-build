@echo off
setlocal
cd /d "%~dp0"
echo Keep Codex on the "thinking" screen. Do not close Codex yet.
echo This read-only check can take about 3 minutes.
echo.
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0diagnose-codex-hang.ps1"
echo.
echo Send the desktop file mirror-codex-hang-report.txt back for analysis.
pause

