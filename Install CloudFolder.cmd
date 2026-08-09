@echo off
setlocal
title CloudFolder Setup
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0CloudFolder.ps1" -Action Install
set EXITCODE=%ERRORLEVEL%
echo.
if not "%EXITCODE%"=="0" echo CloudFolder setup ended with error code %EXITCODE%.
pause
exit /b %EXITCODE%
