@echo off
setlocal
title CloudFolder Manager
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0CloudFolder.ps1" -Action Menu
exit /b %ERRORLEVEL%
