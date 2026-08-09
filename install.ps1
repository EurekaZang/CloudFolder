[CmdletBinding()]
param([switch]$SkipAdd)

$ErrorActionPreference = 'Stop'
$Repo = 'EurekaZang/CloudFolder'
$Api = "https://api.github.com/repos/$Repo/releases/latest"
$TempRoot = Join-Path $env:TEMP ('CloudFolder-setup-' + [guid]::NewGuid().ToString('N'))

function Get-Sha256([string]$Path) {
    $sha = [Security.Cryptography.SHA256]::Create()
    $stream = [IO.File]::OpenRead($Path)
    try {
        return (-join ($sha.ComputeHash($stream) | ForEach-Object { $_.ToString('x2') }))
    } finally {
        $stream.Dispose()
        $sha.Dispose()
    }
}

Write-Host 'CloudFolder bootstrap' -ForegroundColor Cyan
Write-Host 'Downloading the latest GitHub Release metadata and Windows package...' -ForegroundColor DarkGray

New-Item -ItemType Directory -Force -Path $TempRoot | Out-Null
try {
    $headers = @{ 'User-Agent' = 'CloudFolder-Installer' }
    $release = Invoke-RestMethod -UseBasicParsing -Headers $headers -Uri $Api
    $asset = @($release.assets | Where-Object { $_.name -eq 'CloudFolder-windows-x64.zip' }) | Select-Object -First 1
    $checksumAsset = @($release.assets | Where-Object { $_.name -eq 'CloudFolder-windows-x64.zip.sha256' }) | Select-Object -First 1
    if (-not $asset) { throw 'The latest CloudFolder release does not contain CloudFolder-windows-x64.zip.' }
    if (-not $checksumAsset) { throw 'The latest CloudFolder release does not contain its SHA-256 checksum.' }

    $zip = Join-Path $TempRoot 'CloudFolder-windows-x64.zip'
    $checksumFile = Join-Path $TempRoot 'CloudFolder-windows-x64.zip.sha256'
    Invoke-WebRequest -UseBasicParsing -Headers $headers -Uri $asset.browser_download_url -OutFile $zip
    Invoke-WebRequest -UseBasicParsing -Headers $headers -Uri $checksumAsset.browser_download_url -OutFile $checksumFile
    $expectedHash = ((Get-Content -LiteralPath $checksumFile -Raw).Trim() -split '\s+')[0].ToLowerInvariant()
    $actualHash = Get-Sha256 $zip
    if ($actualHash -ne $expectedHash) {
        throw "CloudFolder release ZIP SHA-256 mismatch. Expected $expectedHash, got $actualHash"
    }
    Expand-Archive -LiteralPath $zip -DestinationPath $TempRoot -Force

    $manager = Get-ChildItem -LiteralPath $TempRoot -Recurse -Filter CloudFolder.ps1 | Select-Object -First 1
    if (-not $manager) { throw 'Downloaded package is missing CloudFolder.ps1.' }

    $arguments = @(
        '-NoProfile',
        '-ExecutionPolicy', 'Bypass',
        '-File', ('"' + $manager.FullName + '"'),
        '-Action', 'Install'
    )
    if ($SkipAdd) { $arguments += '-SkipAdd' }
    $process = Start-Process powershell.exe -Verb RunAs -ArgumentList $arguments -Wait -PassThru
    if ($process.ExitCode -ne 0) { throw "CloudFolder setup exited with code $($process.ExitCode)." }
} finally {
    Remove-Item -LiteralPath $TempRoot -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host 'CloudFolder setup completed.' -ForegroundColor Green
