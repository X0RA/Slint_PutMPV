@echo off
setlocal
cd /d "%~dp0"

powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\powershell\setup-deps.ps1"
if errorlevel 1 exit /b %errorlevel%

set "LIB=%~dp0deps;%LIB%"
set "PATH=%~dp0deps;%PATH%"

cargo build --release --locked %*
if errorlevel 1 exit /b %errorlevel%

rem Stage libmpv-2.dll next to the executable so it can be launched directly.
copy /Y "%~dp0deps\libmpv-2.dll" "%~dp0target\release\libmpv-2.dll" >nul

powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\powershell\build-installer.ps1" -BuildDir "%~dp0target\release"
if errorlevel 1 exit /b %errorlevel%

echo.
echo Release build complete:
echo     %~dp0target\release\putmpv.exe
echo     %~dp0target\release\libmpv-2.dll
echo     installer emitted to %~dp0target\release
exit /b 0
