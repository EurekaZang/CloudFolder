@echo off
setlocal
title Uninstall CloudFolder
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0CloudFolder.ps1" -Action Uninstall
set EXITCODE=%ERRORLEVEL%
echo.
pause
exit /b %EXITCODE%
