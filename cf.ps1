$ErrorActionPreference = 'Stop'
$script:RemoteExitCode = 0

$DataDir = 'C:\ProgramData\CloudFolder'
$MountsDir = Join-Path $DataDir 'mounts'

function Show-Usage {
    @'
CloudFolder developer CLI

  cf list
  cf path <mount>
  cf here
  cf status [mount]
  cf flush [mount]
  cf refresh [mount]
  cf run [mount] -- <program> [args...]
  cf sh [mount] -- <shell command>
  cf shell [mount]

Examples:
  cd (cf path lab)
  cf here
  cf run -- git status
  cf run -- pytest -q
  cf run -- cargo test
  cf sh -- "git status && pytest -q"
  cf shell
'@ | Write-Host
}

function Get-MountRecords {
    if (-not (Test-Path -LiteralPath $MountsDir)) { return @() }
    $records = @()
    Get-ChildItem -LiteralPath $MountsDir -Directory -ErrorAction SilentlyContinue | ForEach-Object {
        $metadataPath = Join-Path $_.FullName 'mount.json'
        if (Test-Path -LiteralPath $metadataPath) {
            try {
                $records += (Get-Content -LiteralPath $metadataPath -Raw | ConvertFrom-Json)
            } catch {
                Write-Warning "Ignoring unreadable mount metadata: $metadataPath"
            }
        }
    }
    return @($records)
}

function Normalize-LocalPath([string]$Path) {
    return [IO.Path]::GetFullPath($Path).TrimEnd('\')
}

function Test-PathInside([string]$Candidate, [string]$Root) {
    $candidateFull = Normalize-LocalPath $Candidate
    $rootFull = Normalize-LocalPath $Root
    return $candidateFull -ieq $rootFull -or $candidateFull.StartsWith($rootFull + '\', [StringComparison]::OrdinalIgnoreCase)
}

function Resolve-Mount([string]$RequestedName, [switch]$AllowCurrentDirectory) {
    $records = @(Get-MountRecords)
    if ($records.Count -eq 0) { throw 'No CloudFolder mounts are configured.' }

    if (-not [string]::IsNullOrWhiteSpace($RequestedName)) {
        $matches = @($records | Where-Object {
            $_.name -ieq $RequestedName -or $_.slug -ieq $RequestedName -or $_.service_name -ieq $RequestedName
        })
        if ($matches.Count -ne 1) { throw "Could not uniquely find CloudFolder mount '$RequestedName'." }
        return $matches[0]
    }

    if ($AllowCurrentDirectory) {
        $cwd = (Get-Location).ProviderPath
        $matches = @($records | Where-Object { Test-PathInside $cwd ([string]$_.mount_point) } | Sort-Object { ([string]$_.mount_point).Length } -Descending)
        if ($matches.Count -gt 0) { return $matches[0] }
        if ($records.Count -eq 1) { return $records[0] }
        throw 'The current directory is not inside a CloudFolder mount. Pass the mount name explicitly.'
    }

    if ($records.Count -eq 1) { return $records[0] }
    throw 'More than one CloudFolder mount exists. Pass the mount name explicitly.'
}

function Get-IniValue([string]$Path, [string]$Key) {
    if (-not (Test-Path -LiteralPath $Path)) { return $null }
    foreach ($line in Get-Content -LiteralPath $Path) {
        if ($line -match ('^\s*' + [regex]::Escape($Key) + '\s*=\s*(.+?)\s*$')) {
            return $Matches[1].Trim()
        }
    }
    return $null
}

function ConvertTo-PosixSingleQuoted([string]$Value) {
    $single = [string][char]39
    $replacement = $single + '\' + $single + $single
    return $single + $Value.Replace($single, $replacement) + $single
}

function Get-SshInfo([object]$Record) {
    $keyFile = [string]$Record.key_file
    $knownHosts = [string]$Record.known_hosts
    if ([string]::IsNullOrWhiteSpace($keyFile)) {
        $keyFile = Get-IniValue ([string]$Record.rclone_config) 'key_file'
    }
    if ([string]::IsNullOrWhiteSpace($knownHosts)) {
        $knownHosts = Get-IniValue ([string]$Record.rclone_config) 'known_hosts_file'
    }
    if ([string]::IsNullOrWhiteSpace($keyFile) -or -not (Test-Path -LiteralPath $keyFile)) {
        throw "SSH key is unavailable for mount '$($Record.name)'. Re-add or upgrade the mount."
    }
    if ([string]::IsNullOrWhiteSpace($knownHosts) -or -not (Test-Path -LiteralPath $knownHosts)) {
        throw "known_hosts is unavailable for mount '$($Record.name)'. Re-add or upgrade the mount."
    }
    return [pscustomobject]@{
        key_file = $keyFile
        known_hosts = $knownHosts
    }
}

function Get-SshBaseArgs([object]$Record) {
    $ssh = Get-SshInfo $Record
    return @(
        '-p', [string]$Record.port,
        '-i', [string]$ssh.key_file,
        '-o', 'BatchMode=yes',
        '-o', 'IdentitiesOnly=yes',
        '-o', 'StrictHostKeyChecking=yes',
        '-o', ('UserKnownHostsFile=' + [string]$ssh.known_hosts),
        '-o', 'ServerAliveInterval=15',
        '-o', 'ServerAliveCountMax=3'
    )
}

function Resolve-RemoteRoot([object]$Record) {
    $existing = [string]$Record.remote_root
    if (-not [string]::IsNullOrWhiteSpace($existing)) {
        if ($existing -eq '/') { return '/' }
        return $existing.TrimEnd('/')
    }

    $remotePath = [string]$Record.remote_path
    if ($remotePath -eq '~') { $remotePath = '' }
    if ($remotePath.StartsWith('~/')) { $remotePath = $remotePath.Substring(2) }
    $command = if ([string]::IsNullOrWhiteSpace($remotePath)) {
        'pwd -P'
    } else {
        'cd -- ' + (ConvertTo-PosixSingleQuoted $remotePath) + ' && pwd -P'
    }
    $baseArgs = Get-SshBaseArgs $Record
    $result = (& ssh.exe @baseArgs "$($Record.user)@$($Record.host)" $command | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($result) -or -not $result.StartsWith('/')) {
        throw "Could not resolve the remote root for '$($Record.name)'."
    }
    if ($result -eq '/') { return '/' }
    return $result.TrimEnd('/')
}

function Get-RemoteWorkingDirectory([object]$Record) {
    $localRoot = Normalize-LocalPath ([string]$Record.mount_point)
    $cwd = Normalize-LocalPath ((Get-Location).ProviderPath)
    if (-not (Test-PathInside $cwd $localRoot)) { return (Resolve-RemoteRoot $Record) }

    $relative = $cwd.Substring($localRoot.Length).TrimStart('\','/') -replace '\\','/'
    $remoteRoot = Resolve-RemoteRoot $Record
    if ([string]::IsNullOrWhiteSpace($relative)) {
        if ([string]::IsNullOrWhiteSpace($remoteRoot)) { return '/' }
        return $remoteRoot
    }
    if ($remoteRoot -eq '/') { return '/' + $relative }
    return $remoteRoot.TrimEnd('/') + '/' + $relative
}

function Invoke-Rc([object]$Record, [string]$Method, [int]$TimeoutSec = 5) {
    $port = [int]$Record.rc_port
    if ($port -le 0) { throw "Mount '$($Record.name)' has no RC port metadata." }
    $uri = "http://127.0.0.1:$port/$Method"
    return Invoke-RestMethod -UseBasicParsing -Method Post -Uri $uri -TimeoutSec $TimeoutSec
}

function Get-VfsUploadState([object]$Record) {
    $stats = Invoke-Rc $Record 'vfs/stats' 5
    $disk = $stats.diskCache
    if ($null -eq $disk) {
        return [pscustomobject]@{ queued = 0; in_progress = 0; raw = $stats }
    }
    return [pscustomobject]@{
        queued = [int64]$disk.uploadsQueued
        in_progress = [int64]$disk.uploadsInProgress
        raw = $stats
    }
}

function Wait-VfsFlush([object]$Record, [int]$TimeoutSec = 60, [switch]$Quiet) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSec)
    $last = $null
    do {
        $last = Get-VfsUploadState $Record
        if ($last.queued -eq 0 -and $last.in_progress -eq 0) {
            if (-not $Quiet) { Write-Host "Flushed: $($Record.name)" }
            return
        }
        Start-Sleep -Milliseconds 200
    } while ([DateTime]::UtcNow -lt $deadline)

    $queueText = ''
    try { $queueText = (Invoke-Rc $Record 'vfs/queue' 5 | ConvertTo-Json -Depth 8 -Compress) } catch {}
    throw "Timed out waiting for pending writes on '$($Record.name)' (queued=$($last.queued), in_progress=$($last.in_progress)). Queue: $queueText"
}

function Refresh-Vfs([object]$Record, [switch]$Quiet) {
    try {
        [void](Invoke-Rc $Record 'vfs/forget' 5)
        if (-not $Quiet) { Write-Host "Refreshed: $($Record.name)" }
    } catch {
        if (-not $Quiet) { throw }
    }
}

function Show-Mounts {
    $rows = foreach ($record in @(Get-MountRecords)) {
        $service = Get-Service -Name ([string]$record.service_name) -ErrorAction SilentlyContinue
        [pscustomobject]@{
            Name = [string]$record.name
            Profile = if ([string]::IsNullOrWhiteSpace([string]$record.profile)) { 'Legacy' } else { [string]$record.profile }
            Status = if ($service) { [string]$service.Status } else { 'Missing' }
            Local = [string]$record.mount_point
            Remote = ("{0}@{1}:{2}" -f $record.user,$record.host,$record.port)
        }
    }
    if (@($rows).Count -eq 0) { Write-Host 'No CloudFolder mounts are configured.'; return }
    $rows | Format-Table -AutoSize
}

function Show-Status([object]$Record) {
    $service = Get-Service -Name ([string]$Record.service_name) -ErrorAction SilentlyContinue
    $mounted = Test-Path -LiteralPath ([string]$Record.mount_point)
    $upload = $null
    try { $upload = Get-VfsUploadState $Record } catch {}
    [pscustomobject]@{
        Name = [string]$Record.name
        Profile = if ([string]::IsNullOrWhiteSpace([string]$Record.profile)) { 'Legacy' } else { [string]$Record.profile }
        Service = if ($service) { [string]$service.Status } else { 'Missing' }
        Mounted = $mounted
        PendingWrites = if ($upload) { $upload.queued + $upload.in_progress } else { 'RC unavailable' }
        LocalRoot = [string]$Record.mount_point
        RemoteRoot = Resolve-RemoteRoot $Record
    } | Format-List
}

function Split-RunArguments([string[]]$Tail) {
    $delimiter = [Array]::IndexOf($Tail, '--')
    if ($delimiter -lt 0) { throw 'Expected -- before the remote command.' }
    $before = @($Tail[0..($delimiter - 1)] | Where-Object { $null -ne $_ })
    if ($delimiter -eq 0) { $before = @() }
    $after = @()
    if ($delimiter + 1 -lt $Tail.Count) { $after = @($Tail[($delimiter + 1)..($Tail.Count - 1)]) }
    if ($before.Count -gt 1) { throw 'Pass at most one mount name before --.' }
    if ($after.Count -eq 0) { throw 'A remote command is required after --.' }
    return [pscustomobject]@{
        mount = if ($before.Count -eq 1) { [string]$before[0] } else { '' }
        command = $after
    }
}

function Invoke-RemoteArgv([object]$Record, [string[]]$Command) {
    Wait-VfsFlush $Record 60 -Quiet
    $remoteCwd = Get-RemoteWorkingDirectory $Record
    $quoted = @($Command | ForEach-Object { ConvertTo-PosixSingleQuoted ([string]$_) })
    $remoteScript = 'cd -- ' + (ConvertTo-PosixSingleQuoted $remoteCwd) + ' && exec ' + ($quoted -join ' ')
    $baseArgs = Get-SshBaseArgs $Record
    $oldOutputEncoding = $global:OutputEncoding
    $oldConsoleOutputEncoding = [Console]::OutputEncoding
    try {
        $utf8 = New-Object Text.UTF8Encoding($false)
        $global:OutputEncoding = $utf8
        [Console]::OutputEncoding = $utf8
        $remoteScript | & ssh.exe @baseArgs "$($Record.user)@$($Record.host)" sh -s
        $script:RemoteExitCode = $LASTEXITCODE
    } finally {
        $global:OutputEncoding = $oldOutputEncoding
        [Console]::OutputEncoding = $oldConsoleOutputEncoding
        Refresh-Vfs $Record -Quiet
    }
}

function Invoke-RemoteShell([object]$Record, [string]$Command) {
    Wait-VfsFlush $Record 60 -Quiet
    $remoteCwd = Get-RemoteWorkingDirectory $Record
    $remoteScript = 'cd -- ' + (ConvertTo-PosixSingleQuoted $remoteCwd) + ' && ' + $Command
    $baseArgs = Get-SshBaseArgs $Record
    $oldOutputEncoding = $global:OutputEncoding
    $oldConsoleOutputEncoding = [Console]::OutputEncoding
    try {
        $utf8 = New-Object Text.UTF8Encoding($false)
        $global:OutputEncoding = $utf8
        [Console]::OutputEncoding = $utf8
        $remoteScript | & ssh.exe @baseArgs "$($Record.user)@$($Record.host)" sh -s
        $script:RemoteExitCode = $LASTEXITCODE
    } finally {
        $global:OutputEncoding = $oldOutputEncoding
        [Console]::OutputEncoding = $oldConsoleOutputEncoding
        Refresh-Vfs $Record -Quiet
    }
}

function Open-RemoteShell([object]$Record) {
    Wait-VfsFlush $Record 60 -Quiet
    $remoteCwd = Get-RemoteWorkingDirectory $Record
    $remoteCommand = 'cd -- ' + (ConvertTo-PosixSingleQuoted $remoteCwd) + ' && exec ${SHELL:-/bin/sh} -l'
    $baseArgs = Get-SshBaseArgs $Record
    $oldConsoleInputEncoding = [Console]::InputEncoding
    $oldConsoleOutputEncoding = [Console]::OutputEncoding
    try {
        $utf8 = New-Object Text.UTF8Encoding($false)
        [Console]::InputEncoding = $utf8
        [Console]::OutputEncoding = $utf8
        & ssh.exe -t @baseArgs "$($Record.user)@$($Record.host)" $remoteCommand
        $script:RemoteExitCode = $LASTEXITCODE
    } finally {
        [Console]::InputEncoding = $oldConsoleInputEncoding
        [Console]::OutputEncoding = $oldConsoleOutputEncoding
        Refresh-Vfs $Record -Quiet
    }
}

$argv = @($args)
if ($argv.Count -eq 0 -or $argv[0] -in @('-h','--help','help')) {
    Show-Usage
    exit 0
}

$commandName = ([string]$argv[0]).ToLowerInvariant()
$tail = if ($argv.Count -gt 1) { @($argv[1..($argv.Count - 1)]) } else { @() }

try {
    switch ($commandName) {
        'list' { Show-Mounts }
        'path' {
            $name = if ($tail.Count -gt 0) { [string]$tail[0] } else { '' }
            $record = Resolve-Mount $name -AllowCurrentDirectory
            [Console]::Out.WriteLine([string]$record.mount_point)
        }
        'here' {
            $record = Resolve-Mount '' -AllowCurrentDirectory
            $cwd = (Get-Location).ProviderPath
            Write-Host "Mount:      $($record.name)"
            Write-Host "Profile:    $(if ([string]::IsNullOrWhiteSpace([string]$record.profile)) { 'Legacy' } else { $record.profile })"
            Write-Host "Local root: $($record.mount_point)"
            Write-Host "Local cwd:  $cwd"
            Write-Host "Remote cwd: $(Get-RemoteWorkingDirectory $record)"
        }
        'status' {
            if ($tail.Count -gt 0) {
                Show-Status (Resolve-Mount ([string]$tail[0]))
            } else {
                $record = $null
                try { $record = Resolve-Mount '' -AllowCurrentDirectory } catch {}
                if ($record) { Show-Status $record } else { Show-Mounts }
            }
        }
        'flush' {
            $name = if ($tail.Count -gt 0) { [string]$tail[0] } else { '' }
            Wait-VfsFlush (Resolve-Mount $name -AllowCurrentDirectory) 60
        }
        'refresh' {
            $name = if ($tail.Count -gt 0) { [string]$tail[0] } else { '' }
            Refresh-Vfs (Resolve-Mount $name -AllowCurrentDirectory)
        }
        'run' {
            $parts = Split-RunArguments $tail
            $record = Resolve-Mount $parts.mount -AllowCurrentDirectory
            Invoke-RemoteArgv $record @($parts.command)
            exit $script:RemoteExitCode
        }
        'sh' {
            $parts = Split-RunArguments $tail
            $record = Resolve-Mount $parts.mount -AllowCurrentDirectory
            Invoke-RemoteShell $record (@($parts.command) -join ' ')
            exit $script:RemoteExitCode
        }
        'shell' {
            $name = if ($tail.Count -gt 0) { [string]$tail[0] } else { '' }
            $record = Resolve-Mount $name -AllowCurrentDirectory
            Open-RemoteShell $record
            exit $script:RemoteExitCode
        }
        default { throw "Unknown command '$commandName'. Run cf help." }
    }
} catch {
    [Console]::Error.WriteLine('cf: ' + $_.Exception.Message)
    exit 2
}
