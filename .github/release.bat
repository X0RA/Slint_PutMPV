@echo off
setlocal EnableExtensions

set /p "VERSION=Version (e.g. 1.0.0): "

echo %VERSION%| findstr /R "^[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*$" >nul
if errorlevel 1 (
    echo Error: version must be x.x.x, got: %VERSION% 1>&2
    exit /b 1
)

where gh >nul 2>&1
if errorlevel 1 (
    echo Error: GitHub CLI ^(gh^) not found on PATH. Install from https://cli.github.com/ 1>&2
    exit /b 1
)

echo Triggering release %VERSION%...
gh workflow run release.yml --ref main -f version="%VERSION%"
if errorlevel 1 exit /b %errorlevel%

echo Workflow dispatched. Watch it with:
echo    gh run watch
exit /b 0
