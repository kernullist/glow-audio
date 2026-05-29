#requires -Version 5.1
<#
.SYNOPSIS
    Build GlowAudio Desktop as a standalone Windows executable (and optional installers).

.DESCRIPTION
    Wraps "npm run tauri build". Ensures the Rust toolchain (rustup installs to
    %USERPROFILE%\.cargo\bin, which is not always on PATH in fresh shells) is
    reachable, verifies prerequisites, runs the release build and reports the
    locations of the produced artifacts.

.PARAMETER NoBundle
    Build only the standalone .exe and skip the msi / nsis installers (faster,
    avoids the first-run WiX/NSIS toolchain download).

.PARAMETER SkipInstall
    Skip "npm install" (use when node_modules is already up to date).

.EXAMPLE
    .\build.ps1
        Full release build: standalone exe + installers.

.EXAMPLE
    .\build.ps1 -NoBundle
        Standalone exe only.
#>
[CmdletBinding()]
param(
    [switch]$NoBundle,
    [switch]$SkipInstall
)

$ErrorActionPreference = "Stop"
$root = $PSScriptRoot
Set-Location $root

function Write-Step($msg)
{
    Write-Host "==> $msg" -ForegroundColor Cyan
}

# Make cargo / rustc reachable for this session if they live under the default
# rustup bin directory but are not on PATH.
$cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
if ((Test-Path (Join-Path $cargoBin "cargo.exe")) -and ($env:Path -notlike "*$cargoBin*"))
{
    $env:Path = "$cargoBin;$env:Path"
}

# Verify required tools are present.
foreach ($tool in @("node", "npm", "cargo"))
{
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue))
    {
        throw "Required tool '$tool' was not found in PATH. Install it before building."
    }
}

Write-Step "Toolchain: Node $(node --version), npm $(npm --version), $(cargo --version)"

# Install frontend dependencies unless explicitly skipped.
if (-not $SkipInstall)
{
    Write-Step "npm install"
    npm install
    if ($LASTEXITCODE -ne 0)
    {
        throw "npm install failed with exit code $LASTEXITCODE"
    }
}

# Assemble and run the Tauri release build.
$tauriArgs = @("run", "tauri", "build")
if ($NoBundle)
{
    # Pass-through flag to the tauri CLI (build the binary without installers).
    $tauriArgs += @("--", "--no-bundle")
}

Write-Step "npm $($tauriArgs -join ' ')"
npm @tauriArgs
if ($LASTEXITCODE -ne 0)
{
    throw "tauri build failed with exit code $LASTEXITCODE"
}

# Report produced artifacts.
$releaseDir = Join-Path $root "src-tauri\target\release"
$exe = Join-Path $releaseDir "glow-audio.exe"

Write-Host ""
Write-Step "Build complete."

if (Test-Path $exe)
{
    $sizeMb = [math]::Round((Get-Item $exe).Length / 1MB, 1)
    Write-Host "    Standalone EXE : $exe ($sizeMb MB)" -ForegroundColor Green
}
else
{
    Write-Host "    WARNING: expected exe not found at $exe" -ForegroundColor Yellow
}

$bundleDir = Join-Path $releaseDir "bundle"
if (Test-Path $bundleDir)
{
    Get-ChildItem -Path $bundleDir -Recurse -Include *.msi, *.exe -ErrorAction SilentlyContinue |
        ForEach-Object {
            Write-Host "    Installer      : $($_.FullName)" -ForegroundColor Green
        }
}
