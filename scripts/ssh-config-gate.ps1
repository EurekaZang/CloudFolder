[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[A-Za-z0-9._-]+$')]
    [string]$SshHost,
    [string]$CfPath = ''
)

$ErrorActionPreference = 'Stop'

function Assert-NativeExit([string]$Operation) {
    if ($LASTEXITCODE -ne 0) { throw "$Operation failed with exit code $LASTEXITCODE" }
}

if ([string]::IsNullOrWhiteSpace($CfPath)) {
    $CfPath = (Get-Command cf.exe -ErrorAction Stop).Source
}
$CfPath = (Resolve-Path -LiteralPath $CfPath).Path
$runtimeDir = Split-Path -Parent $CfPath
$manager = Join-Path $runtimeDir 'CloudFolder.ps1'
if (-not (Test-Path -LiteralPath $manager)) { throw "Missing CloudFolder manager beside cf.exe: $manager" }

$mountsDir = 'C:\ProgramData\CloudFolder\mounts'
$existing = @()
if (Test-Path -LiteralPath $mountsDir) {
    Get-ChildItem -LiteralPath $mountsDir -Directory -ErrorAction SilentlyContinue | ForEach-Object {
        $metadata = Join-Path $_.FullName 'mount.json'
        if (Test-Path -LiteralPath $metadata) {
            try { $existing += (Get-Content -LiteralPath $metadata -Raw | ConvertFrom-Json) } catch {}
        }
    }
}
if (@($existing | Where-Object { $_.name -ieq $SshHost -or $_.slug -ieq $SshHost }).Count -gt 0) {
    throw "A CloudFolder mount named '$SshHost' already exists; the SSH Config gate refuses to reuse it."
}

$cacheDir = $null
$added = $false
try {
    Write-Host "Formal Gate: if 'ssh $SshHost' works non-interactively, 'cf add $SshHost' should work." -ForegroundColor Cyan
    $probe = (& ssh.exe -o BatchMode=yes $SshHost 'printf cloudfolder-ssh-config-gate' 2>&1 | Out-String).Trim()
    Assert-NativeExit "ssh $SshHost"
    if ($probe -ne 'cloudfolder-ssh-config-gate') { throw "ssh $SshHost returned unexpected probe output." }

    & $CfPath add $SshHost | Out-Host
    Assert-NativeExit "cf add $SshHost"
    $added = $true
    & $CfPath status $SshHost | Out-Host
    Assert-NativeExit "cf status $SshHost"
    $mountPath = (& $CfPath path $SshHost | Out-String).Trim()
    Assert-NativeExit "cf path $SshHost"
    if (-not (Test-Path -LiteralPath $mountPath)) { throw "SSH Config mount did not become available: $mountPath" }

    $record = Get-ChildItem -LiteralPath $mountsDir -Directory | ForEach-Object {
        $metadata = Join-Path $_.FullName 'mount.json'
        if (Test-Path -LiteralPath $metadata) {
            try { Get-Content -LiteralPath $metadata -Raw | ConvertFrom-Json } catch {}
        }
    } | Where-Object { $_.name -ieq $SshHost -or $_.slug -ieq $SshHost } | Select-Object -First 1
    if ($record) { $cacheDir = [string]$record.cache_dir }
    Write-Host 'SSH CONFIG / PROXYJUMP FORMAL GATE PASSED' -ForegroundColor Green
} finally {
    if ($added) {
        try {
            & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $manager -Action Remove -Name $SshHost -Force | Out-Host
            if ($LASTEXITCODE -ne 0) { Write-Warning "Could not automatically remove gate mount '$SshHost'." }
        } catch { Write-Warning "Could not automatically remove gate mount '$SshHost': $($_.Exception.Message)" }
    }
    if (-not [string]::IsNullOrWhiteSpace($cacheDir)) {
        Remove-Item -LiteralPath $cacheDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}
