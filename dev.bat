@echo off
setlocal
cd /d "%~dp0"

powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\setup-deps.ps1"
if errorlevel 1 exit /b %errorlevel%

set "LIB=%~dp0deps;%LIB%"
set "PATH=%~dp0deps;%PATH%"
set "CARGO_INCREMENTAL=1"
rem dev-fast profile sets debug=0; force debug symbols on for the dev workflow.
set "CARGO_PROFILE_DEV_FAST_DEBUG=2"

cargo build --profile dev-fast
if errorlevel 1 exit /b %errorlevel%

if not exist "%~dp0target\dev-fast" mkdir "%~dp0target\dev-fast"
copy /Y "%~dp0deps\libmpv-2.dll" "%~dp0target\dev-fast\libmpv-2.dll" >nul

"%~dp0target\dev-fast\putmpv.exe" %*
exit /b %errorlevel%
