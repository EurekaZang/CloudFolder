[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$ServiceName,
    [Parameter(Mandatory = $true)]
    [string]$MountPoint,
    [Parameter(Mandatory = $true)]
    [string]$RemoteHost,
    [int]$RemotePort = 22,
    [string]$RemoteName = 'remote',
    [string]$RemoteBase = '',
    [Parameter(Mandatory = $true)]
    [int]$RcPort,
    [int]$RecoveryTimeoutSeconds = 120
)

$ErrorActionPreference = 'Stop'
$Rclone = 'C:\Program Files\CloudFolder\rclone.exe'
$slug = $ServiceName -replace '^CloudFolder\.', ''
$RcloneConfig = "C:\ProgramData\CloudFolder\mounts\$slug\rclone.conf"
$RcUrl = "http://127.0.0.1:$RcPort/"
$FirewallRule = "CloudFolder Fault Test $slug"

function Assert-Admin {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'Run fault-test.ps1 from an elevated PowerShell window.'
    }
}

function Get-RclonePid {
    try {
        $json = & $Rclone rc --url $RcUrl core/pid 2>$null
        if ($LASTEXITCODE -ne 0 -or -not $json) { return $null }
        return [int](($json | Out-String | ConvertFrom-Json).pid)
    } catch {
        return $null
    }
}

function Test-MountResponsive {
    $job = Start-Job -ScriptBlock {
        param($Path)
        try {
            Get-ChildItem -LiteralPath $Path -Force -ErrorAction Stop | Select-Object -First 1 | Out-Null
            return $true
        } catch {
            return $false
        }
    } -ArgumentList $MountPoint
    try {
        if (-not (Wait-Job -Job $job -Timeout 5)) {
            Stop-Job -Job $job -ErrorAction SilentlyContinue
            return $false
        }
        return [bool](Receive-Job -Job $job)
    } finally {
        Remove-Job -Job $job -Force -ErrorAction SilentlyContinue
    }
}

function Wait-NewRclonePid([int]$OldPid, [int]$TimeoutSeconds = $RecoveryTimeoutSeconds) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        $service = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
        if ($service -and $service.Status -eq 'Running') {
            $rcloneProcessId = Get-RclonePid
            if ($rcloneProcessId -and $rcloneProcessId -ne $OldPid -and (Test-MountResponsive)) { return $rcloneProcessId }
        }
        Start-Sleep -Milliseconds 500
    }
    throw "No healthy replacement rclone PID appeared within $TimeoutSeconds seconds."
}

function Wait-ProcessGone([int]$ProcessId, [int]$TimeoutSeconds = 10) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        if (-not (Get-Process -Id $ProcessId -ErrorAction SilentlyContinue)) { return }
        Start-Sleep -Milliseconds 100
    }
    throw "PID $ProcessId was still alive after $TimeoutSeconds seconds."
}

function Get-Sha256([string]$Path) {
    $sha = [Security.Cryptography.SHA256]::Create()
    $stream = [IO.File]::OpenRead($Path)
    try {
        return (-join ($sha.ComputeHash($stream) | ForEach-Object { $_.ToString('x2') })).ToUpperInvariant()
    } finally {
        $stream.Dispose()
        $sha.Dispose()
    }
}

function Assert-MountRoundTrip([string]$Label) {
    $id = [guid]::NewGuid().ToString('N')
    $name = ".cloudfolder-fault-$Label-$id.bin"
    $mountFile = Join-Path $MountPoint $name
    $download = Join-Path $env:TEMP $name
    if ([string]::IsNullOrWhiteSpace($RemoteBase)) {
        $remote = "${RemoteName}:$name"
    } elseif ($RemoteBase.EndsWith('/')) {
        $remote = "${RemoteName}:$RemoteBase$name"
    } else {
        $remote = "${RemoteName}:$RemoteBase/$name"
    }
    $rng = [Security.Cryptography.RandomNumberGenerator]::Create()
    try {
        $bytes = New-Object byte[] (256 * 1024)
        $rng.GetBytes($bytes)
        [IO.File]::WriteAllBytes($mountFile, $bytes)
        $localHash = Get-Sha256 $mountFile

        # vfs_write_back is 5s; give the cache time to commit before bypassing the mount.
        Start-Sleep -Seconds 7
        & $Rclone copyto $remote $download --config $RcloneConfig --contimeout 10s --timeout 30s --retries 2 --low-level-retries 2
        if ($LASTEXITCODE -ne 0) { throw "Direct backend download failed during $Label" }
        $backendHash = Get-Sha256 $download
        if ($localHash -ne $backendHash) {
            throw "Mount/backend SHA256 mismatch during $Label"
        }

        Remove-Item -LiteralPath $mountFile -Force
        Write-Host "PASS $Label mount/backend SHA256=$($localHash.Substring(0,12))..."
    } finally {
        $rng.Dispose()
        Remove-Item -LiteralPath $download -Force -ErrorAction SilentlyContinue
        try {
            & $Rclone deletefile $remote --config $RcloneConfig 2>$null | Out-Null
        } catch {
            # Idempotent cleanup: the mount-side delete may already have
            # removed the backend object, which is the desired end state.
        }
    }
}

Assert-Admin
if (-not (Test-Path $Rclone)) { throw "rclone not installed: $Rclone" }
if (-not (Test-Path $RcloneConfig)) { throw "rclone config not installed: $RcloneConfig" }
if ((Get-Service -Name $ServiceName).Status -ne 'Running') { throw "$ServiceName is not Running" }

Remove-NetFirewallRule -DisplayName $FirewallRule -ErrorAction SilentlyContinue
try {
    Assert-MountRoundTrip 'baseline'

    # 1. Kill only the mount engine. The Rust supervisor must replace it.
    $oldRclone = Get-RclonePid
    if (-not $oldRclone) { throw 'Could not determine baseline rclone PID.' }
    Write-Host "Fault 1: force-killing rclone PID $oldRclone"
    Stop-Process -Id $oldRclone -Force
    Wait-ProcessGone $oldRclone
    $newRclone = Wait-NewRclonePid $oldRclone
    Write-Host "Recovered rclone: $oldRclone -> $newRclone"
    Assert-MountRoundTrip 'rclone-crash-recovery'

    # 2. Block only this program's SFTP traffic, force a reconnect, then restore the network.
    Write-Host 'Fault 2: blocking rclone outbound SFTP and forcing reconnect'
    New-NetFirewallRule -DisplayName $FirewallRule -Direction Outbound -Action Block `
        -Program $Rclone -RemoteAddress $RemoteHost -Protocol TCP -RemotePort $RemotePort | Out-Null
    $blockedPid = Get-RclonePid
    if ($blockedPid) {
        Stop-Process -Id $blockedPid -Force
        Wait-ProcessGone $blockedPid
    }
    Start-Sleep -Seconds 12
    if ((Get-Service -Name $ServiceName).Status -ne 'Running') {
        throw 'Supervisor service stopped while SFTP network was unavailable.'
    }
    Remove-NetFirewallRule -DisplayName $FirewallRule -ErrorAction SilentlyContinue
    $afterNetwork = Wait-NewRclonePid $(if ($blockedPid) { $blockedPid } else { 0 })
    Write-Host "Recovered after network restore with rclone PID $afterNetwork"
    Assert-MountRoundTrip 'network-recovery'

    # 3. Kill the service itself. Job Object must kill its child; SCM recovery must restart both.
    $serviceCim = Get-CimInstance Win32_Service -Filter "Name='$ServiceName'"
    $servicePid = [int]$serviceCim.ProcessId
    $orphanCandidate = Get-RclonePid
    if (-not $servicePid -or -not $orphanCandidate) { throw 'Could not resolve service/rclone PIDs.' }
    Write-Host "Fault 3: force-killing service PID $servicePid; child rclone PID $orphanCandidate must not survive"
    Stop-Process -Id $servicePid -Force
    Wait-ProcessGone $servicePid
    Wait-ProcessGone $orphanCandidate
    $afterServiceCrash = Wait-NewRclonePid $orphanCandidate
    Write-Host "SCM + Job Object recovery complete; new rclone PID $afterServiceCrash"
    Assert-MountRoundTrip 'service-crash-recovery'

    Write-Host 'PASS: all fault-injection scenarios recovered successfully.'
} finally {
    Remove-NetFirewallRule -DisplayName $FirewallRule -ErrorAction SilentlyContinue
}
