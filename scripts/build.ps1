[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $PSScriptRoot
$TargetDir = Join-Path $env:LOCALAPPDATA 'CloudFolder\cargo-target'
$DistDir = Join-Path $Root 'dist'
$Toolchain = 'stable-x86_64-pc-windows-gnu'

Push-Location $Root
try {
    rustup toolchain install $Toolchain --profile minimal
    if ($LASTEXITCODE -ne 0) { throw "rustup failed with exit code $LASTEXITCODE" }
    $env:CARGO_TARGET_DIR = $TargetDir
    cargo "+$Toolchain" fmt -- --check
    if ($LASTEXITCODE -ne 0) { throw "cargo fmt failed with exit code $LASTEXITCODE" }
    cargo "+$Toolchain" test
    if ($LASTEXITCODE -ne 0) { throw "cargo test failed with exit code $LASTEXITCODE" }
    cargo "+$Toolchain" clippy --all-targets -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "cargo clippy failed with exit code $LASTEXITCODE" }
    cargo "+$Toolchain" build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build --release failed with exit code $LASTEXITCODE" }
    New-Item -ItemType Directory -Force -Path $DistDir | Out-Null
    Copy-Item (Join-Path $TargetDir 'release\CloudFolderService.exe') (Join-Path $DistDir 'CloudFolderService.exe') -Force
    Copy-Item (Join-Path $TargetDir 'release\cf.exe') (Join-Path $DistDir 'cf.exe') -Force
    Write-Host "Built: $(Join-Path $DistDir 'CloudFolderService.exe')"
    Write-Host "Built: $(Join-Path $DistDir 'cf.exe')"
} finally {
    Pop-Location
}
