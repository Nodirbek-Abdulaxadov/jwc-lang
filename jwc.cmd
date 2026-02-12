@echo off
setlocal
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0jwc.ps1" %*
exit /b %ERRORLEVEL%
