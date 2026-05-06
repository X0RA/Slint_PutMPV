<#
.SYNOPSIS
    Build the Windows PutMPV installer.

.DESCRIPTION
    Builds the release executable, stages libmpv-2.dll next to it via build.bat,
    then compiles installer/putmpv.iss with Inno Setup.

.PARAMETER Version
    Optional installer version. Defaults to the version in Cargo.toml.

.PARAMETER SkipBuild
    Skip the Rust release build and use the existing target/release outputs.
#>
[CmdletBinding()]
param(
    [string]$Version,
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot = Split-Path -Parent $ScriptDir
$InstallerScript = Join-Path $RepoRoot 'installer\putmpv.iss'
$ReleaseDir = Join-Path $RepoRoot 'target\release'
$ExePath = Join-Path $ReleaseDir 'putmpv.exe'
$MpvDllPath = Join-Path $ReleaseDir 'libmpv-2.dll'

if (-not $Version) {
    $manifest = Get-Content (Join-Path $RepoRoot 'Cargo.toml')
    $versionLine = $manifest | Where-Object { $_ -match '^\s*version\s*=\s*"([^"]+)"' } | Select-Object -First 1
    if (-not $versionLine) {
        throw 'Could not find package version in Cargo.toml.'
    }
    $Version = [regex]::Match($versionLine, '^\s*version\s*=\s*"([^"]+)"').Groups[1].Value
}

function Find-InnoCompiler {
    $cmd = Get-Command 'ISCC.exe' -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }

    foreach ($path in @(
        (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe'),
        (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'),
        (Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe')
    )) {
        if ($path -and (Test-Path $path)) { return $path }
    }

    throw "Inno Setup compiler not found. Install Inno Setup 6.3 or newer, then re-run this script."
}

if (-not $SkipBuild) {
    & (Join-Path $RepoRoot 'build.bat')
    if ($LASTEXITCODE -ne 0) {
        throw "build.bat failed with exit code $LASTEXITCODE."
    }
}

foreach ($required in @($ExePath, $MpvDllPath, $InstallerScript)) {
    if (-not (Test-Path $required)) {
        throw "Required file missing: $required"
    }
}

$iscc = Find-InnoCompiler
New-Item -ItemType Directory -Force -Path (Join-Path $RepoRoot 'dist') | Out-Null

& $iscc "/DAppVersion=$Version" $InstallerScript
if ($LASTEXITCODE -ne 0) {
    throw "Inno Setup failed with exit code $LASTEXITCODE."
}

$output = Join-Path $RepoRoot "dist\PutMPV-$Version-Setup.exe"
Write-Host "Installer built:"
Write-Host "    $output"
