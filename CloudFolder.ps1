[CmdletBinding()]
param(
    [ValidateSet('Menu','Install','Add','List','Remove','Restart','Open','Logs','Doctor','Uninstall')]
    [string]$Action = 'Menu',
    [string]$Name,
    [string]$SshHost,
    [string]$RemoteHost,
    [int]$Port = 0,
    [string]$RemoteUser,
    [string]$RemotePath,
    [string]$MountPoint,
    [string]$KeyFile,
    [ValidateSet('Dev','Balanced')]
    [string]$Profile = 'Dev',
    [switch]$ReadOnly,
    [switch]$SkipKeyAuthorization,
    [switch]$NonInteractive,
    [switch]$SkipAdd,
    [switch]$NoOpen,
    [switch]$Force,
    [switch]$PurgeCache
)

$ErrorActionPreference = 'Stop'
$ScriptBoundParameters = @{} + $PSBoundParameters

$ScriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$InstallDir = 'C:\Program Files\CloudFolder'
$DataDir = 'C:\ProgramData\CloudFolder'
$MountsDir = Join-Path $DataDir 'mounts'
$LogsDir = Join-Path $DataDir 'logs'
$InstalledExe = Join-Path $InstallDir 'CloudFolderService.exe'
$InstalledCf = Join-Path $InstallDir 'cf.exe'
$InstalledRclone = Join-Path $InstallDir 'rclone.exe'
$RcloneVersion = '1.75.0'
$RcloneZipSha256 = '203581f0a7baeae873f2347483a798c79e2eaf5c384a4e9d866aa374f1c89ac0'
$WinFspVersion = '2.1.25156'
$WinFspMsiSha256 = '073a70e00f77423e34bed98b86e600def93393ba5822204fac57a29324db9f7a'
$WinFspDll = 'C:\Program Files (x86)\WinFsp\bin\winfsp-x64.dll'

function Write-Banner {
    Write-Host ''
    Write-Host '  CloudFolder' -ForegroundColor Cyan
    Write-Host '  Remote folders that behave like normal Windows folders.' -ForegroundColor DarkGray
    Write-Host ''
}

function Test-IsAdmin {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Ensure-Admin {
    if (Test-IsAdmin) { return }

    Write-Host 'CloudFolder needs Administrator permission to install/manage Windows services.' -ForegroundColor Yellow
    Write-Host 'A Windows UAC prompt will appear.' -ForegroundColor Yellow

    $argumentList = @(
        '-NoProfile',
        '-ExecutionPolicy', 'Bypass',
        '-File', ('"' + $PSCommandPath + '"')
    )
    foreach ($entry in $ScriptBoundParameters.GetEnumerator()) {
        $argumentList += ('-' + $entry.Key)
        if ($entry.Value -is [System.Management.Automation.SwitchParameter]) {
            if (-not $entry.Value.IsPresent) { $argumentList = $argumentList[0..($argumentList.Count - 2)] }
            continue
        }
        $value = [string]$entry.Value
        $argumentList += ('"' + $value.Replace('"','\"') + '"')
    }
    if (-not $ScriptBoundParameters.ContainsKey('Action')) {
        $argumentList += @('-Action', $Action)
    }
    $process = Start-Process powershell.exe -Verb RunAs -ArgumentList $argumentList -Wait -PassThru
    exit $process.ExitCode
}

function Assert-NativeExit([string]$Operation) {
    if ($LASTEXITCODE -ne 0) {
        throw "$Operation failed with exit code $LASTEXITCODE"
    }
}

function Get-PayloadExe {
    $candidates = @(
        (Join-Path $ScriptRoot 'CloudFolderService.exe'),
        (Join-Path $ScriptRoot 'dist\CloudFolderService.exe'),
        (Join-Path $ScriptRoot 'target\release\CloudFolderService.exe')
    )
    foreach ($candidate in $candidates) {
        if (Test-Path -LiteralPath $candidate) { return (Resolve-Path $candidate).Path }
    }
    throw 'CloudFolderService.exe is missing. Download the Windows release ZIP, or build the project first.'
}

function Get-PayloadCfExe {
    $candidates = @(
        (Join-Path $ScriptRoot 'cf.exe'),
        (Join-Path $ScriptRoot 'dist\cf.exe'),
        (Join-Path $ScriptRoot 'target\release\cf.exe')
    )
    foreach ($candidate in $candidates) {
        if (Test-Path -LiteralPath $candidate) { return (Resolve-Path $candidate).Path }
    }
    throw 'cf.exe is missing. Download the Windows release ZIP, or build the project first.'
}

function Assert-FileHash([string]$Path, [string]$Expected, [string]$Label) {
    $sha = [Security.Cryptography.SHA256]::Create()
    $stream = [IO.File]::OpenRead($Path)
    try {
        $actual = -join ($sha.ComputeHash($stream) | ForEach-Object { $_.ToString('x2') })
    } finally {
        $stream.Dispose()
        $sha.Dispose()
    }
    if ($actual -ne $Expected.ToLowerInvariant()) {
        throw "$Label SHA256 mismatch. Expected $Expected, got $actual"
    }
}

function Read-Value([string]$Prompt, [string]$Default = '') {
    if ([string]::IsNullOrWhiteSpace($Default)) {
        return (Read-Host $Prompt).Trim()
    }
    $value = (Read-Host "$Prompt [$Default]").Trim()
    if ([string]::IsNullOrWhiteSpace($value)) { return $Default }
    return $value
}

function Read-YesNo([string]$Prompt, [bool]$DefaultYes = $true) {
    $suffix = if ($DefaultYes) { '[Y/n]' } else { '[y/N]' }
    $answer = (Read-Host "$Prompt $suffix").Trim().ToLowerInvariant()
    if ([string]::IsNullOrWhiteSpace($answer)) { return $DefaultYes }
    return $answer -in @('y','yes')
}

function ConvertTo-Slug([string]$Value) {
    $slug = $Value.Trim().ToLowerInvariant() -replace '[^a-z0-9._-]+','-'
    $slug = $slug.Trim('-','.')
    if ([string]::IsNullOrWhiteSpace($slug)) {
        $slug = 'mount-' + [guid]::NewGuid().ToString('N').Substring(0,8)
    }
    if ($slug.Length -gt 48) { $slug = $slug.Substring(0,48).Trim('-','.') }
    return $slug
}

function ConvertTo-TomlString([string]$Value) {
    return $Value.Replace('\','\\').Replace('"','\"')
}

function ConvertTo-SafeFolderName([string]$Value) {
    $safe = $Value -replace '[<>:"/\\|?*]', '-'
    $safe = $safe.Trim().TrimEnd('.')
    if ([string]::IsNullOrWhiteSpace($safe)) { return 'Remote' }
    return $safe
}

function ConvertTo-PosixSingleQuoted([string]$Value) {
    $single = [string][char]39
    $replacement = $single + '\' + $single + $single
    return $single + $Value.Replace($single, $replacement) + $single
}

function Ensure-CloudFolderOnPath {
    $machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
    $entries = @($machinePath -split ';' | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    $alreadyPresent = @($entries | Where-Object { $_.TrimEnd('\\') -ieq $InstallDir.TrimEnd('\\') }).Count -gt 0
    if (-not $alreadyPresent) {
        [Environment]::SetEnvironmentVariable('Path', (($entries + $InstallDir) -join ';'), 'Machine')
    }
    if (@(($env:Path -split ';') | Where-Object { $_.TrimEnd('\\') -ieq $InstallDir.TrimEnd('\\') }).Count -eq 0) {
        $env:Path = $env:Path.TrimEnd(';') + ';' + $InstallDir
    }
}

function Remove-CloudFolderFromPath {
    $machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
    if ([string]::IsNullOrWhiteSpace($machinePath)) { return }
    $entries = @($machinePath -split ';' | Where-Object {
        -not [string]::IsNullOrWhiteSpace($_) -and $_.TrimEnd('\\') -ine $InstallDir.TrimEnd('\\')
    })
    [Environment]::SetEnvironmentVariable('Path', ($entries -join ';'), 'Machine')
}

function Get-MountRecords {
    if (-not (Test-Path $MountsDir)) { return @() }
    $records = @()
    Get-ChildItem -LiteralPath $MountsDir -Directory -ErrorAction SilentlyContinue | ForEach-Object {
        $metadataPath = Join-Path $_.FullName 'mount.json'
        if (Test-Path $metadataPath) {
            try {
                $records += (Get-Content -LiteralPath $metadataPath -Raw | ConvertFrom-Json)
            } catch {
                Write-Warning "Ignoring unreadable mount metadata: $metadataPath"
            }
        }
    }
    return @($records)
}

function Resolve-MountRecord([string]$RequestedName) {
    $records = @(Get-MountRecords)
    if ($records.Count -eq 0) { throw 'No CloudFolder mounts are configured.' }

    if ([string]::IsNullOrWhiteSpace($RequestedName)) {
        Show-Mounts
        $RequestedName = (Read-Host 'Mount name').Trim()
    }

    $matches = @($records | Where-Object {
        $_.name -ieq $RequestedName -or $_.slug -ieq $RequestedName -or $_.service_name -ieq $RequestedName
    })
    if ($matches.Count -ne 1) { throw "Could not uniquely find mount '$RequestedName'." }
    return $matches[0]
}

function Get-ServiceName([string]$Slug) {
    return "CloudFolder.$Slug"
}

function Get-NextRcPort {
    $used = @{}
    try {
        [Net.NetworkInformation.IPGlobalProperties]::GetIPGlobalProperties().GetActiveTcpListeners() | ForEach-Object {
            $used[[int]$_.Port] = $true
        }
    } catch {
        Write-Warning 'Could not enumerate active TCP listeners; falling back to CloudFolder config reservations only.'
    }
    if (Test-Path $MountsDir) {
        Get-ChildItem -LiteralPath $MountsDir -Recurse -Filter service.toml -ErrorAction SilentlyContinue | ForEach-Object {
            $text = Get-Content -LiteralPath $_.FullName -Raw -ErrorAction SilentlyContinue
            if ($text -match 'rc_addr\s*=\s*"127\.0\.0\.1:(\d+)"') {
                $used[[int]$Matches[1]] = $true
            }
        }
    }
    for ($portNumber = 55770; $portNumber -le 55870; $portNumber++) {
        if (-not $used.ContainsKey($portNumber)) { return $portNumber }
    }
    throw 'No free CloudFolder RC ports remain in 55770-55870.'
}

function Stop-CloudFolderService([string]$ServiceName, [int]$TimeoutSeconds = 30) {
    $service = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
    if (-not $service -or $service.Status -eq 'Stopped') { return }

    & sc.exe stop $ServiceName | Out-Null
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        $service = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
        if (-not $service -or $service.Status -eq 'Stopped') { return }
        Start-Sleep -Milliseconds 250
    }
    throw "$ServiceName did not stop within $TimeoutSeconds seconds."
}

function Wait-ServiceDeleted([string]$ServiceName, [int]$TimeoutSeconds = 20) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    $registryPath = "HKLM:\SYSTEM\CurrentControlSet\Services\$ServiceName"
    while ([DateTime]::UtcNow -lt $deadline) {
        $serviceGone = -not (Get-Service -Name $ServiceName -ErrorAction SilentlyContinue)
        $registryGone = -not (Test-Path -LiteralPath $registryPath)
        if ($serviceGone -and $registryGone) { return }
        Start-Sleep -Milliseconds 250
    }
    throw "Windows service '$ServiceName' is still registered after $TimeoutSeconds seconds. CloudFolder kept its metadata so removal can be retried safely."
}

function Wait-MountReady([object]$Record, [int]$TimeoutSeconds = 60) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    $logPath = [string]$Record.service_log
    while ([DateTime]::UtcNow -lt $deadline) {
        $service = Get-Service -Name ([string]$Record.service_name) -ErrorAction SilentlyContinue
        if (-not $service) { throw "Service $($Record.service_name) disappeared during startup." }
        if ($service.Status -ne 'Running') {
            Start-Sleep -Milliseconds 500
            continue
        }
        if ((Test-Path -LiteralPath ([string]$Record.mount_point)) -and (Test-Path -LiteralPath $logPath)) {
            if (Select-String -LiteralPath $logPath -SimpleMatch 'mount ready pid=' -Quiet) { return }
        }
        Start-Sleep -Milliseconds 500
    }
    throw "Mount '$($Record.name)' did not become ready within $TimeoutSeconds seconds. Run CloudFolder Doctor."
}

function Set-SecureDataAcl {
    if (-not (Test-Path $DataDir)) { return }
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $currentUser = $identity.Name
    & icacls.exe $DataDir /inheritance:r | Out-Null
    Assert-NativeExit 'Protecting CloudFolder data directory'
    & icacls.exe $DataDir /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' "$currentUser`:(OI`)(CI`)F" | Out-Null
    Assert-NativeExit 'Setting CloudFolder data ACL'
    $children = Get-ChildItem -LiteralPath $DataDir -Force -ErrorAction SilentlyContinue
    if ($children) {
        & icacls.exe (Join-Path $DataDir '*') /reset /T /C | Out-Null
        Assert-NativeExit 'Resetting CloudFolder child ACLs'
    }
    Repair-AllServiceSshSnapshots
}

function Upgrade-ExistingMounts {
    if (-not (Test-Path -LiteralPath $MountsDir)) { return }

    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $userSid = $identity.User.Value
    $fileSecurity = "O:${userSid}G:${userSid}D:P(A;;FA;;;$userSid)(A;;FA;;;SY)(A;;FA;;;BA)"
    $utf8NoBom = New-Object Text.UTF8Encoding($false)

    Get-ChildItem -LiteralPath $MountsDir -Directory -ErrorAction SilentlyContinue | ForEach-Object {
        $mountDir = $_.FullName
        $metadataPath = Join-Path $mountDir 'mount.json'
        if (-not (Test-Path -LiteralPath $metadataPath)) { return }

        try {
            $record = Get-Content -LiteralPath $metadataPath -Raw | ConvertFrom-Json
            $configPath = [string]$record.config_path
            if ([string]::IsNullOrWhiteSpace($configPath)) { $configPath = Join-Path $mountDir 'service.toml' }

            if (Test-Path -LiteralPath $configPath) {
                $configText = Get-Content -LiteralPath $configPath -Raw
                if ($configText -notmatch '(?m)^windows_file_security\s*=') {
                    $securityLine = 'windows_file_security = "' + (ConvertTo-TomlString $fileSecurity) + '"'
                    if ($configText -match '(?m)^read_only\s*=') {
                        $configText = [regex]::Replace(
                            $configText,
                            '(?m)^(read_only\s*=)',
                            ($securityLine + "`r`n`$1"),
                            1
                        )
                    } else {
                        $configText = $configText.TrimEnd() + "`r`n" + $securityLine + "`r`n"
                    }
                    [IO.File]::WriteAllText($configPath, $configText, $utf8NoBom)
                    Write-Host "Upgraded Windows filesystem ACLs for $($record.name)." -ForegroundColor DarkGray
                }
            }

            if (-not $record.PSObject.Properties['profile']) {
                $record | Add-Member -NotePropertyName profile -NotePropertyValue 'Balanced'
            }
            if (-not $record.PSObject.Properties['windows_user_sid']) {
                $record | Add-Member -NotePropertyName windows_user_sid -NotePropertyValue $userSid
            }
            if (-not $record.PSObject.Properties['remote_root']) {
                $remotePath = [string]$record.remote_path
                if ($remotePath.StartsWith('/')) {
                    $remoteRoot = if ($remotePath -eq '/') { '/' } else { $remotePath.TrimEnd('/') }
                    $record | Add-Member -NotePropertyName remote_root -NotePropertyValue $remoteRoot
                }
            }
            $metadataJson = $record | ConvertTo-Json -Depth 6
            [IO.File]::WriteAllText($metadataPath, $metadataJson, $utf8NoBom)
        } catch {
            Write-Warning "Could not upgrade CloudFolder mount metadata in '$mountDir': $($_.Exception.Message)"
        }
    }
}

function Ensure-OpenSshClient {
    if ((Get-Command ssh.exe -ErrorAction SilentlyContinue) -and (Get-Command ssh-keygen.exe -ErrorAction SilentlyContinue)) { return }
    Write-Host 'Installing Windows OpenSSH Client...' -ForegroundColor Cyan
    $capability = Get-WindowsCapability -Online | Where-Object { $_.Name -like 'OpenSSH.Client*' } | Select-Object -First 1
    if (-not $capability) { throw 'Windows OpenSSH Client capability was not found.' }
    if ($capability.State -ne 'Installed') {
        Add-WindowsCapability -Online -Name $capability.Name | Out-Null
    }
    if (-not (Get-Command ssh.exe -ErrorAction SilentlyContinue)) { throw 'OpenSSH Client installation did not make ssh.exe available.' }
}

function Ensure-WinFsp {
    if (Test-Path $WinFspDll) { return }
    Write-Host "Installing WinFsp $WinFspVersion..." -ForegroundColor Cyan
    $tmp = Join-Path $env:TEMP ('cloudfolder-winfsp-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Force -Path $tmp | Out-Null
    try {
        $msi = Join-Path $tmp "winfsp-$WinFspVersion.msi"
        Invoke-WebRequest -UseBasicParsing 'https://github.com/winfsp/winfsp/releases/download/v2.1/winfsp-2.1.25156.msi' -OutFile $msi
        Assert-FileHash $msi $WinFspMsiSha256 'WinFsp installer'
        $process = Start-Process msiexec.exe -ArgumentList @('/i', ('"' + $msi + '"'), '/qn', '/norestart') -Wait -PassThru
        if ($process.ExitCode -notin @(0,3010)) { throw "WinFsp installer failed with exit code $($process.ExitCode)." }
    } finally {
        Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
    if (-not (Test-Path $WinFspDll)) { throw 'WinFsp installation did not produce the expected x64 runtime.' }
}

function Ensure-Rclone {
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    if (Test-Path $InstalledRclone) {
        $versionLine = (& $InstalledRclone version 2>$null | Select-Object -First 1)
        if ($LASTEXITCODE -eq 0 -and $versionLine -eq "rclone v$RcloneVersion") { return }
    }

    Write-Host "Installing rclone $RcloneVersion..." -ForegroundColor Cyan
    $tmp = Join-Path $env:TEMP ('cloudfolder-rclone-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Force -Path $tmp | Out-Null
    try {
        $zip = Join-Path $tmp 'rclone.zip'
        Invoke-WebRequest -UseBasicParsing "https://downloads.rclone.org/v$RcloneVersion/rclone-v$RcloneVersion-windows-amd64.zip" -OutFile $zip
        Assert-FileHash $zip $RcloneZipSha256 'rclone archive'
        Expand-Archive -LiteralPath $zip -DestinationPath $tmp -Force
        $downloaded = Get-ChildItem -LiteralPath $tmp -Recurse -Filter rclone.exe | Select-Object -First 1
        if (-not $downloaded) { throw 'rclone.exe was not found in the downloaded archive.' }
        Copy-Item -LiteralPath $downloaded.FullName -Destination $InstalledRclone -Force
    } finally {
        Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Install-Runtime {
    Ensure-Admin
    Ensure-OpenSshClient

    $payloadExe = Get-PayloadExe
    $payloadCfExe = Get-PayloadCfExe
    $runningBefore = @()
    Get-Service -Name 'CloudFolder.*' -ErrorAction SilentlyContinue | ForEach-Object {
        if ($_.Status -eq 'Running') { $runningBefore += $_.Name }
        Stop-CloudFolderService $_.Name
    }

    try {
        Ensure-WinFsp
        New-Item -ItemType Directory -Force -Path $InstallDir,$DataDir,$MountsDir,$LogsDir | Out-Null
        Ensure-Rclone

        $destinationExe = Join-Path $InstallDir 'CloudFolderService.exe'
        if ((Resolve-Path $payloadExe).Path -ne $destinationExe) {
            Copy-Item -LiteralPath $payloadExe -Destination $destinationExe -Force
        }
        $destinationCfExe = Join-Path $InstallDir 'cf.exe'
        if ((Resolve-Path $payloadCfExe).Path -ne $destinationCfExe) {
            Copy-Item -LiteralPath $payloadCfExe -Destination $destinationCfExe -Force
        }
        Copy-Item -LiteralPath $PSCommandPath -Destination (Join-Path $InstallDir 'CloudFolder.ps1') -Force

        foreach ($cmdName in @('CloudFolder Manager.cmd','Uninstall CloudFolder.cmd','cf.ps1')) {
            $source = Join-Path $ScriptRoot $cmdName
            if (Test-Path $source) { Copy-Item -LiteralPath $source -Destination (Join-Path $InstallDir $cmdName) -Force }
        }

        Set-SecureDataAcl
        Upgrade-ExistingMounts
        Ensure-CloudFolderOnPath
        Install-StartMenuShortcut
    } finally {
        foreach ($serviceName in $runningBefore) {
            if (Get-Service -Name $serviceName -ErrorAction SilentlyContinue) {
                Start-Service -Name $serviceName -ErrorAction SilentlyContinue
            }
        }
    }

    Write-Host 'CloudFolder runtime is installed.' -ForegroundColor Green
    Write-Host 'Open a new terminal and use `cf` to work with mounted remote workspaces.' -ForegroundColor DarkGray
}

function Install-StartMenuShortcut {
    $programs = [Environment]::GetFolderPath('CommonPrograms')
    if ([string]::IsNullOrWhiteSpace($programs)) { return }
    $folder = Join-Path $programs 'CloudFolder'
    New-Item -ItemType Directory -Force -Path $folder | Out-Null
    $managerCmd = Join-Path $InstallDir 'CloudFolder Manager.cmd'
    if (-not (Test-Path $managerCmd)) { return }
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut((Join-Path $folder 'CloudFolder Manager.lnk'))
    $shortcut.TargetPath = $managerCmd
    $shortcut.WorkingDirectory = $InstallDir
    $shortcut.Description = 'Manage CloudFolder remote mounts'
    $shortcut.Save()
}

function Get-DedicatedKeyPath {
    $sshDir = Join-Path $env:USERPROFILE '.ssh'
    New-Item -ItemType Directory -Force -Path $sshDir | Out-Null
    return (Join-Path $sshDir 'cloudfolder_ed25519')
}

function Ensure-KeyFile([string]$RequestedKeyFile) {
    if (-not [string]::IsNullOrWhiteSpace($RequestedKeyFile)) {
        $resolved = [Environment]::ExpandEnvironmentVariables($RequestedKeyFile)
        if (-not (Test-Path -LiteralPath $resolved)) { throw "SSH private key does not exist: $resolved" }
        return (Resolve-Path $resolved).Path
    }

    $dedicated = Get-DedicatedKeyPath
    if (-not (Test-Path -LiteralPath $dedicated)) {
        Write-Host 'Creating a dedicated CloudFolder SSH key (no passphrase, protected by Windows ACLs)...' -ForegroundColor Cyan
        & ssh-keygen.exe -t ed25519 -a 64 -N '' -f $dedicated -C ("cloudfolder@" + $env:COMPUTERNAME) | Out-Null
        Assert-NativeExit 'Generating CloudFolder SSH key'
    }
    return $dedicated
}

function Get-KnownHostsPath {
    $sshDir = Join-Path $env:USERPROFILE '.ssh'
    New-Item -ItemType Directory -Force -Path $sshDir | Out-Null
    $path = Join-Path $sshDir 'known_hosts'
    if (-not (Test-Path $path)) { New-Item -ItemType File -Path $path -Force | Out-Null }
    return $path
}

function Ensure-HostTrust([string]$TargetHost, [int]$TargetPort, [string]$TargetUser, [string]$PrivateKey, [string]$KnownHosts) {
    $lookupName = if ($TargetPort -eq 22) { $TargetHost } else { "[$TargetHost]:$TargetPort" }
    & ssh-keygen.exe -F $lookupName -f $KnownHosts 2>$null | Out-Null
    if ($LASTEXITCODE -eq 0) { return }

    Write-Host ''
    Write-Host 'First connection to this server.' -ForegroundColor Cyan
    Write-Host 'OpenSSH will display the server fingerprint. Verify it, then type yes if it is correct.' -ForegroundColor Yellow

    $oldPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'Continue'
        & ssh.exe -p $TargetPort -i $PrivateKey -o IdentitiesOnly=yes -o PubkeyAuthentication=no -o PasswordAuthentication=no -o KbdInteractiveAuthentication=no -o PreferredAuthentications=none -o StrictHostKeyChecking=ask -o UserKnownHostsFile=$KnownHosts "$TargetUser@$TargetHost" 'exit 0'
    } finally {
        $ErrorActionPreference = $oldPreference
    }

    & ssh-keygen.exe -F $lookupName -f $KnownHosts 2>$null | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw 'The server host key was not accepted, so CloudFolder stopped before storing any credentials.'
    }
}

function Test-KeyLogin([string]$TargetHost, [int]$TargetPort, [string]$TargetUser, [string]$PrivateKey, [string]$KnownHosts) {
    $oldPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'SilentlyContinue'
        & ssh.exe -p $TargetPort -i $PrivateKey -o BatchMode=yes -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=$KnownHosts "$TargetUser@$TargetHost" 'printf cloudfolder-ok' 2>$null | Out-Null
        return ($LASTEXITCODE -eq 0)
    } finally {
        $ErrorActionPreference = $oldPreference
    }
}

function Authorize-Key([string]$TargetHost, [int]$TargetPort, [string]$TargetUser, [string]$PrivateKey, [string]$KnownHosts) {
    $publicKeyPath = "$PrivateKey.pub"
    if (-not (Test-Path $publicKeyPath)) {
        & ssh-keygen.exe -y -f $PrivateKey | Set-Content -LiteralPath $publicKeyPath -Encoding ascii
        Assert-NativeExit 'Deriving SSH public key'
    }
    $publicKey = (Get-Content -LiteralPath $publicKeyPath -Raw).Trim()
    if ($publicKey -match "'") { throw 'Unexpected apostrophe in SSH public key comment; use a simpler key comment.' }

    Write-Host ''
    Write-Host "One-time SSH setup for $TargetUser@$TargetHost`:$TargetPort" -ForegroundColor Cyan
    Write-Host 'OpenSSH may show the server fingerprint. Verify it, type yes if correct, then enter the SSH password.' -ForegroundColor Yellow
    Write-Host 'CloudFolder never stores or receives that password.' -ForegroundColor Yellow
    Write-Host ''

    $remoteCommand = "umask 077; mkdir -p ~/.ssh; touch ~/.ssh/authorized_keys; grep -qxF '$publicKey' ~/.ssh/authorized_keys || printf '%s\n' '$publicKey' >> ~/.ssh/authorized_keys; chmod 700 ~/.ssh; chmod 600 ~/.ssh/authorized_keys"
    & ssh.exe -p $TargetPort -o IdentitiesOnly=yes -o PubkeyAuthentication=no -o PreferredAuthentications=password,keyboard-interactive -o StrictHostKeyChecking=ask -o UserKnownHostsFile=$KnownHosts "$TargetUser@$TargetHost" $remoteCommand
    Assert-NativeExit 'Installing the CloudFolder public key on the server'
}

function Get-NegotiatedHostKeyAlgorithm([string]$TargetHost, [int]$TargetPort, [string]$TargetUser, [string]$PrivateKey, [string]$KnownHosts) {
    $oldPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'Continue'
        $debugText = (& ssh.exe -vv -p $TargetPort -i $PrivateKey -o BatchMode=yes -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=$KnownHosts "$TargetUser@$TargetHost" 'exit 0' 2>&1 | Out-String)
        $exitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $oldPreference
    }
    if ($exitCode -ne 0) { throw 'Key-only SSH verification failed after key setup.' }
    if ($debugText -match 'kex: host key algorithm: ([^\s]+)') { return $Matches[1] }
    throw 'Could not determine the SSH host-key algorithm negotiated by Windows OpenSSH.'
}

function Grant-ServiceKeyAccess([string]$PrivateKey, [string]$KnownHosts) {
    & icacls.exe $PrivateKey /grant '*S-1-5-18:R' | Out-Null
    Assert-NativeExit 'Granting LocalSystem access to the SSH private key'
    & icacls.exe $KnownHosts /grant '*S-1-5-18:R' | Out-Null
    Assert-NativeExit 'Granting LocalSystem access to known_hosts'
}

function Get-UserSshConfigPath {
    $path = Join-Path (Join-Path $env:USERPROFILE '.ssh') 'config'
    if (Test-Path -LiteralPath $path) { return (Resolve-Path -LiteralPath $path).Path }
    return ''
}

function Get-SshResolvedConfig([string]$Alias, [string]$SshConfigPath) {
    if ([string]::IsNullOrWhiteSpace($Alias) -or $Alias -notmatch '^[A-Za-z0-9._-]+$') {
        throw 'SSH config host aliases may contain only letters, numbers, dot, underscore, and hyphen.'
    }
    $sshArgs = @()
    if (-not [string]::IsNullOrWhiteSpace($SshConfigPath)) { $sshArgs += @('-F', $SshConfigPath) }
    $sshArgs += @('-G', $Alias)
    $oldPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'Continue'
        $lines = @(& ssh.exe @sshArgs 2>$null)
        $exitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $oldPreference
    }
    if ($exitCode -ne 0 -or $lines.Count -eq 0) {
        throw "OpenSSH could not resolve '$Alias'. Verify that `ssh $Alias` works first."
    }
    $values = @{}
    foreach ($line in $lines) {
        if ([string]$line -match '^([^\s]+)\s+(.+)$') {
            $key = $Matches[1].ToLowerInvariant()
            if (-not $values.ContainsKey($key)) { $values[$key] = $Matches[2].Trim() }
        }
    }
    foreach ($required in @('hostname','user','port')) {
        if (-not $values.ContainsKey($required)) { throw "OpenSSH -G did not return '$required' for '$Alias'." }
    }
    return [pscustomobject]@{
        Alias = $Alias
        HostName = [string]$values['hostname']
        User = [string]$values['user']
        Port = [int]$values['port']
        Lines = @($lines)
    }
}

function Invoke-SshAliasCommand([string]$Alias, [string]$SshConfigPath, [string]$RemoteCommand, [switch]$Capture) {
    $sshArgs = @()
    if (-not [string]::IsNullOrWhiteSpace($SshConfigPath)) { $sshArgs += @('-F', $SshConfigPath) }
    $sshArgs += @('-o', 'BatchMode=yes', $Alias, $RemoteCommand)
    $oldPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'Continue'
        if ($Capture) {
            $result = (& ssh.exe @sshArgs 2>$null | Out-String).Trim()
            return [pscustomobject]@{ ExitCode = $LASTEXITCODE; Output = $result }
        }
        & ssh.exe @sshArgs
        return [pscustomobject]@{ ExitCode = $LASTEXITCODE; Output = '' }
    } finally {
        $ErrorActionPreference = $oldPreference
    }
}

function Test-SshAliasLogin([string]$Alias, [string]$SshConfigPath) {
    $result = Invoke-SshAliasCommand $Alias $SshConfigPath 'printf cloudfolder-ok' -Capture
    return ($result.ExitCode -eq 0 -and $result.Output -eq 'cloudfolder-ok')
}

function Get-ProxyJumpAliases([string[]]$ResolvedLines) {
    $aliases = @()
    foreach ($line in $ResolvedLines) {
        if ([string]$line -notmatch '^proxyjump\s+(.+)$') { continue }
        $value = $Matches[1].Trim()
        if ([string]::IsNullOrWhiteSpace($value) -or $value -ieq 'none') { continue }
        foreach ($hop in ($value -split ',')) {
            $candidate = $hop.Trim()
            if ($candidate.Contains('@')) { $candidate = $candidate.Substring($candidate.LastIndexOf('@') + 1) }
            if ($candidate.StartsWith('[') -and $candidate.Contains(']')) {
                $candidate = $candidate.Substring(1, $candidate.IndexOf(']') - 1)
            } elseif ($candidate -match '^(.+?):\d+$') {
                $candidate = $Matches[1]
            }
            if ($candidate -match '^[A-Za-z0-9._-]+$') { $aliases += $candidate }
        }
    }
    return @($aliases | Select-Object -Unique)
}

function Resolve-SshAliasRemoteRoot([string]$Alias, [string]$SshConfigPath, [string]$TargetRemotePath) {
    $path = [string]$TargetRemotePath
    if ($path -eq '~') { $path = '' }
    if ($path.StartsWith('~/')) { $path = $path.Substring(2) }
    $remoteCommand = if ([string]::IsNullOrWhiteSpace($path)) {
        'pwd -P'
    } else {
        'cd -- ' + (ConvertTo-PosixSingleQuoted $path) + ' && pwd -P'
    }
    $result = Invoke-SshAliasCommand $Alias $SshConfigPath $remoteCommand -Capture
    if ($result.ExitCode -ne 0 -or [string]::IsNullOrWhiteSpace($result.Output) -or -not $result.Output.StartsWith('/')) {
        throw "Remote folder could not be resolved through SSH config host '$Alias': '$TargetRemotePath'"
    }
    return $result.Output
}

function ConvertFrom-SshHomePath([string]$Value) {
    $value = $Value.Trim().Trim('"')
    if ($value -eq '~') { return $env:USERPROFILE }
    if ($value.StartsWith('~/') -or $value.StartsWith('~\')) {
        return (Join-Path $env:USERPROFILE $value.Substring(2))
    }
    return [Environment]::ExpandEnvironmentVariables($value)
}

function Protect-ServiceSshPrivateFile([string]$Path) {
    & icacls.exe $Path /grant:r '*S-1-5-18:R' '*S-1-5-32-544:F' | Out-Null
    Assert-NativeExit "Restricting service SSH material '$Path'"
    & icacls.exe $Path /inheritance:r | Out-Null
    Assert-NativeExit "Removing inherited ACLs from '$Path'"
    & icacls.exe $Path /setowner '*S-1-5-18' | Out-Null
    Assert-NativeExit "Setting LocalSystem owner on '$Path'"
}

function Protect-ServiceSshPublicFile([string]$Path, [switch]$WritableBySystem) {
    $systemRight = if ($WritableBySystem) { '*S-1-5-18:M' } else { '*S-1-5-18:R' }
    & icacls.exe $Path /grant:r $systemRight '*S-1-5-32-544:F' | Out-Null
    Assert-NativeExit "Restricting service SSH file '$Path'"
    & icacls.exe $Path /inheritance:r | Out-Null
    Assert-NativeExit "Removing inherited ACLs from '$Path'"
}

function Get-SshConfigTokens([string]$Value) {
    $items = @()
    foreach ($match in [regex]::Matches($Value, '"[^"]+"|\S+')) {
        $items += ([string]$match.Value).Trim('"')
    }
    return @($items)
}

function New-ServiceSshSnapshot([string]$MountDir, [object[]]$ResolvedHosts) {
    $sshDir = Join-Path $MountDir 'ssh-service'
    New-Item -ItemType Directory -Force -Path $sshDir | Out-Null
    $serviceConfigPath = Join-Path $sshDir 'config'

    $knownHostsPath = Join-Path $sshDir 'known_hosts'
    $knownLines = New-Object System.Collections.Generic.HashSet[string]([StringComparer]::Ordinal)
    foreach ($resolved in $ResolvedHosts) {
        foreach ($line in @($resolved.Lines)) {
            if ([string]$line -notmatch '^userknownhostsfile\s+(.+)$') { continue }
            foreach ($token in @(Get-SshConfigTokens $Matches[1])) {
                if ($token -ieq 'none' -or $token -ieq 'NUL') { continue }
                $candidate = ConvertFrom-SshHomePath $token
                if (-not (Test-Path -LiteralPath $candidate -PathType Leaf)) { continue }
                foreach ($knownLine in Get-Content -LiteralPath $candidate -ErrorAction SilentlyContinue) {
                    if (-not [string]::IsNullOrWhiteSpace([string]$knownLine)) { [void]$knownLines.Add([string]$knownLine) }
                }
            }
        }
    }
    [IO.File]::WriteAllLines($knownHostsPath, @($knownLines), (New-Object Text.UTF8Encoding($false)))
    Protect-ServiceSshPublicFile $knownHostsPath -WritableBySystem

    $privateCopies = @{}
    $certificateCopies = @{}
    $privateIndex = 0
    $certificateIndex = 0
    $serviceConfigLines = New-Object System.Collections.Generic.List[string]
    $excluded = @(
        'host','localforward','remoteforward','dynamicforward','remotecommand','localcommand',
        'permitlocalcommand','requesttty','sessiontype','stdinnull','forkafterauthentication',
        'controlmaster','controlpath','controlpersist','clearallforwardings','forwardagent',
        'forwardx11','forwardx11trusted','forwardx11timeout','gatewayports'
    )

    foreach ($resolved in $ResolvedHosts) {
        [void]$serviceConfigLines.Add(('Host ' + [string]$resolved.Alias))
        $wroteKnownHosts = $false
        foreach ($line in @($resolved.Lines)) {
            $text = [string]$line
            if ($text -notmatch '^([^\s]+)\s+(.+)$') { continue }
            $key = $Matches[1].ToLowerInvariant()
            $value = $Matches[2].Trim()
            if ($key -in $excluded) { continue }

            if ($key -eq 'identityfile') {
                $source = ConvertFrom-SshHomePath $value
                if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { continue }
                if (-not $privateCopies.ContainsKey($source)) {
                    $privateIndex++
                    $destination = Join-Path $sshDir ("identity-$privateIndex")
                    Copy-Item -LiteralPath $source -Destination $destination -Force
                    Protect-ServiceSshPrivateFile $destination
                    $privateCopies[$source] = $destination
                }
                [void]$serviceConfigLines.Add(('  identityfile "' + ([string]$privateCopies[$source]).Replace('\','/') + '"'))
                continue
            }

            if ($key -eq 'certificatefile') {
                if ($value -ieq 'none') { continue }
                $source = ConvertFrom-SshHomePath $value
                if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { continue }
                if (-not $certificateCopies.ContainsKey($source)) {
                    $certificateIndex++
                    $destination = Join-Path $sshDir ("certificate-$certificateIndex")
                    Copy-Item -LiteralPath $source -Destination $destination -Force
                    Protect-ServiceSshPublicFile $destination
                    $certificateCopies[$source] = $destination
                }
                [void]$serviceConfigLines.Add(('  certificatefile "' + ([string]$certificateCopies[$source]).Replace('\','/') + '"'))
                continue
            }

            if ($key -eq 'userknownhostsfile') {
                if (-not $wroteKnownHosts) {
                    [void]$serviceConfigLines.Add(('  userknownhostsfile "' + $knownHostsPath.Replace('\','/') + '"'))
                    $wroteKnownHosts = $true
                }
                continue
            }

            if ($key -eq 'proxycommand' -and $value -match '(?i)^ssh(?:\.exe)?(\s+.*)$') {
                $sshExe = 'C:/Windows/System32/OpenSSH/ssh.exe'
                $serviceConfigForProxy = $serviceConfigPath.Replace('\','/')
                $text = 'proxycommand "' + $sshExe + '" -F "' + $serviceConfigForProxy + '"' + $Matches[1]
            }

            [void]$serviceConfigLines.Add(('  ' + $text))
        }
        if (-not $wroteKnownHosts) {
            [void]$serviceConfigLines.Add(('  userknownhostsfile "' + $knownHostsPath.Replace('\','/') + '"'))
        }
        [void]$serviceConfigLines.Add('')
    }

    [IO.File]::WriteAllLines($serviceConfigPath, @($serviceConfigLines), (New-Object Text.UTF8Encoding($false)))
    Protect-ServiceSshPublicFile $serviceConfigPath

    return $serviceConfigPath
}

function Protect-ServiceSshSnapshot([string]$ServiceConfigPath) {
    if ([string]::IsNullOrWhiteSpace($ServiceConfigPath)) { return }
    $sshDir = Split-Path -Parent $ServiceConfigPath
    if (-not (Test-Path -LiteralPath $sshDir)) { return }
    $configPath = Join-Path $sshDir 'config'
    $knownHostsPath = Join-Path $sshDir 'known_hosts'
    if (Test-Path -LiteralPath $configPath) { Protect-ServiceSshPublicFile $configPath }
    if (Test-Path -LiteralPath $knownHostsPath) { Protect-ServiceSshPublicFile $knownHostsPath -WritableBySystem }
    Get-ChildItem -LiteralPath $sshDir -Filter 'identity-*' -File -ErrorAction SilentlyContinue | ForEach-Object {
        Protect-ServiceSshPrivateFile $_.FullName
    }
    Get-ChildItem -LiteralPath $sshDir -Filter 'certificate-*' -File -ErrorAction SilentlyContinue | ForEach-Object {
        Protect-ServiceSshPublicFile $_.FullName
    }
}

function Repair-AllServiceSshSnapshots {
    if (-not (Test-Path -LiteralPath $MountsDir)) { return }
    Get-ChildItem -LiteralPath $MountsDir -Directory -ErrorAction SilentlyContinue | ForEach-Object {
        $serviceConfigPath = Join-Path $_.FullName 'ssh-service\config'
        if (Test-Path -LiteralPath $serviceConfigPath) {
            Protect-ServiceSshSnapshot $serviceConfigPath
        }
    }
}

function ConvertTo-RcloneSshToken([string]$Value) {
    $normalized = $Value.Replace('\','/')
    if ($normalized -match '[\s"]') { return '"' + $normalized.Replace('"','\"') + '"' }
    return $normalized
}

function Get-RcloneExternalSshCommand([string]$Alias, [string]$SshConfigPath) {
    $parts = @($InstalledCf, 'ssh-proxy', '--home', $env:USERPROFILE)
    if (-not [string]::IsNullOrWhiteSpace($SshConfigPath)) { $parts += @('--config', $SshConfigPath) }
    $parts += @('--target', $Alias, '--')
    return (($parts | ForEach-Object { ConvertTo-RcloneSshToken ([string]$_) }) -join ' ')
}

function Resolve-RemoteRoot(
    [string]$TargetHost,
    [int]$TargetPort,
    [string]$TargetUser,
    [string]$PrivateKey,
    [string]$KnownHosts,
    [string]$TargetRemotePath
) {
    $path = [string]$TargetRemotePath
    if ($path -eq '~') { $path = '' }
    if ($path.StartsWith('~/')) { $path = $path.Substring(2) }
    $remoteCommand = if ([string]::IsNullOrWhiteSpace($path)) {
        'pwd -P'
    } else {
        'cd -- ' + (ConvertTo-PosixSingleQuoted $path) + ' && pwd -P'
    }
    $oldPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'Continue'
        $result = (& ssh.exe -p $TargetPort -i $PrivateKey -o BatchMode=yes -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=$KnownHosts "$TargetUser@$TargetHost" $remoteCommand 2>$null | Out-String).Trim()
        $exitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $oldPreference
    }
    if ($exitCode -ne 0 -or [string]::IsNullOrWhiteSpace($result) -or -not $result.StartsWith('/')) {
        throw "Remote folder could not be resolved to an absolute Linux path: '$TargetRemotePath'"
    }
    return $result
}

function Assert-SafeMountPoint([string]$Path) {
    $root = [IO.Path]::GetPathRoot($Path)
    if ([string]::IsNullOrWhiteSpace($root) -or -not (Test-Path $root)) { throw "Mount drive does not exist: $Path" }
    if (-not (Test-Path -LiteralPath $Path)) { return }
    $item = Get-Item -LiteralPath $Path -Force
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { return }
    if (-not $item.PSIsContainer) { throw "Mount point exists and is not a directory: $Path" }
    if (Get-ChildItem -LiteralPath $Path -Force | Select-Object -First 1) {
        throw "Mount point is a non-empty normal directory. CloudFolder refuses to hide or delete those files: $Path"
    }
}

function Add-Mount {
    Ensure-Admin
    if (-not (Test-Path $InstalledExe) -or -not (Test-Path $InstalledRclone)) {
        Install-Runtime
    }

    Write-Host 'Add a remote SFTP folder' -ForegroundColor Cyan
    Write-Host 'Press Enter to accept values shown in [brackets].' -ForegroundColor DarkGray
    Write-Host ''

    $usingSshConfig = -not [string]::IsNullOrWhiteSpace($SshHost)
    if ($usingSshConfig) {
        $SshHost = $SshHost.Trim()
        if ($SshHost -notmatch '^[A-Za-z0-9._-]+$') { throw 'SSH config host alias contains unsupported characters.' }
    }
    $displayName = if (-not [string]::IsNullOrWhiteSpace($Name)) {
        $Name.Trim()
    } elseif ($usingSshConfig) {
        $SshHost
    } else {
        Read-Value 'Friendly name (example: Lab Server)'
    }
    if ([string]::IsNullOrWhiteSpace($displayName)) { throw 'A mount name is required.' }
    $slug = ConvertTo-Slug $displayName
    $serviceName = Get-ServiceName $slug

    if (Get-Service -Name $serviceName -ErrorAction SilentlyContinue) {
        throw "A CloudFolder mount named '$displayName' already exists. Remove it first or choose another name."
    }

    $sshConfigPath = ''
    $sshResolved = $null
    if ($usingSshConfig) {
        $sshConfigPath = Get-UserSshConfigPath
        $sshResolved = Get-SshResolvedConfig $SshHost $sshConfigPath
        $targetHost = $sshResolved.HostName
        $targetPort = $sshResolved.Port
        $targetUser = $sshResolved.User
        Write-Host ("Using OpenSSH config host '{0}' -> {1}@{2}:{3}" -f $SshHost,$targetUser,$targetHost,$targetPort) -ForegroundColor DarkGray
    } else {
        $targetHost = if ([string]::IsNullOrWhiteSpace($RemoteHost)) { Read-Value 'Server address (IP or hostname)' } else { $RemoteHost.Trim() }
        if ([string]::IsNullOrWhiteSpace($targetHost)) { throw 'Server address is required.' }
        if ($targetHost -notmatch '^[A-Za-z0-9._:-]+$') { throw 'Server address contains unsupported characters.' }
        $targetPort = if ($Port -gt 0) { $Port } else { [int](Read-Value 'SSH port' '22') }
        if ($targetPort -lt 1 -or $targetPort -gt 65535) { throw 'SSH port must be between 1 and 65535.' }
        $targetUser = if ([string]::IsNullOrWhiteSpace($RemoteUser)) { Read-Value 'SSH username' } else { $RemoteUser.Trim() }
        if ([string]::IsNullOrWhiteSpace($targetUser)) { throw 'SSH username is required.' }
        if ($targetUser -notmatch '^[A-Za-z0-9._-]+$') { throw 'SSH username contains unsupported characters.' }
    }

    $targetRemotePath = $RemotePath
    if ($null -eq $targetRemotePath -and -not $NonInteractive) {
        $targetRemotePath = Read-Value 'Remote folder (blank = SSH home directory)' ''
    }
    if ($null -eq $targetRemotePath) { $targetRemotePath = '' }
    $targetRemotePath = [string]$targetRemotePath
    if ($targetRemotePath -match '[\r\n]') { throw 'Remote folder cannot contain line breaks.' }
    if ($targetRemotePath -eq '~') { $targetRemotePath = '' }
    if ($targetRemotePath.StartsWith('~/')) { $targetRemotePath = $targetRemotePath.Substring(2) }

    $defaultMountPoint = Join-Path (Join-Path $env:USERPROFILE 'CloudFolder') (ConvertTo-SafeFolderName $displayName)
    $localMountPoint = if (-not [string]::IsNullOrWhiteSpace($MountPoint)) {
        $MountPoint.Trim()
    } elseif ($NonInteractive) {
        $defaultMountPoint
    } else {
        Read-Value 'Local Windows folder' $defaultMountPoint
    }
    $localMountPoint = [Environment]::ExpandEnvironmentVariables($localMountPoint)
    if ($localMountPoint -match '[\r\n]') { throw 'Local mount point cannot contain line breaks.' }
    if (-not [IO.Path]::IsPathRooted($localMountPoint)) { throw 'Local mount point must be an absolute Windows path.' }
    Assert-SafeMountPoint $localMountPoint

    $privateKey = ''
    $knownHosts = ''
    $hostKeyAlgorithm = ''
    $serviceSshResolved = @()
    $serviceSshConfigPath = ''
    if ($usingSshConfig) {
        if (-not (Test-SshAliasLogin $SshHost $sshConfigPath)) {
            throw "Formal Gate failed: 'ssh $SshHost' must work non-interactively before 'cf add $SshHost' can adopt it."
        }
        $serviceSshResolved = @($sshResolved)
        $seenJumpHosts = New-Object System.Collections.Generic.HashSet[string]([StringComparer]::OrdinalIgnoreCase)
        $jumpQueue = New-Object System.Collections.Generic.Queue[string]
        foreach ($jumpHost in @(Get-ProxyJumpAliases $sshResolved.Lines)) { $jumpQueue.Enqueue($jumpHost) }
        while ($jumpQueue.Count -gt 0) {
            $jumpHost = $jumpQueue.Dequeue()
            if (-not $seenJumpHosts.Add($jumpHost)) { continue }
            $jumpResolved = Get-SshResolvedConfig $jumpHost $sshConfigPath
            $serviceSshResolved += $jumpResolved
            foreach ($nestedJump in @(Get-ProxyJumpAliases $jumpResolved.Lines)) { $jumpQueue.Enqueue($nestedJump) }
        }
        $remoteRoot = Resolve-SshAliasRemoteRoot $SshHost $sshConfigPath $targetRemotePath
    } else {
        $privateKey = Ensure-KeyFile $KeyFile
        $knownHosts = Get-KnownHostsPath

        Ensure-HostTrust $targetHost $targetPort $targetUser $privateKey $knownHosts

        $alreadyWorks = Test-KeyLogin $targetHost $targetPort $targetUser $privateKey $knownHosts
        if (-not $alreadyWorks) {
            if ($SkipKeyAuthorization) {
                throw 'The selected SSH key is not authorized on the server.'
            }
            Authorize-Key $targetHost $targetPort $targetUser $privateKey $knownHosts
        }
        if (-not (Test-KeyLogin $targetHost $targetPort $targetUser $privateKey $knownHosts)) {
            throw 'CloudFolder could not establish strict key-only SSH after authorization.'
        }

        $hostKeyAlgorithm = Get-NegotiatedHostKeyAlgorithm $targetHost $targetPort $targetUser $privateKey $knownHosts
        Grant-ServiceKeyAccess $privateKey $knownHosts
        $remoteRoot = Resolve-RemoteRoot $targetHost $targetPort $targetUser $privateKey $knownHosts $targetRemotePath
    }
    $windowsIdentity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $windowsUserSid = $windowsIdentity.User.Value
    $windowsFileSecurity = "O:${windowsUserSid}G:${windowsUserSid}D:P(A;;FA;;;$windowsUserSid)(A;;FA;;;SY)(A;;FA;;;BA)"

    if ($Profile -eq 'Dev') {
        $vfsCacheMode = 'full'
        $vfsWriteBack = '1s'
        $vfsCachePollInterval = '2s'
        $transfers = 8
        $filePerms = '0777'
        $dirPerms = '0777'
    } else {
        $vfsCacheMode = 'full'
        $vfsWriteBack = '5s'
        $vfsCachePollInterval = '30s'
        $transfers = 4
        $filePerms = '0666'
        $dirPerms = '0777'
    }

    $mountDir = Join-Path $MountsDir $slug
    $configPath = Join-Path $mountDir 'service.toml'
    $rcloneConfigPath = Join-Path $mountDir 'rclone.conf'
    $metadataPath = Join-Path $mountDir 'mount.json'
    $serviceLog = Join-Path $LogsDir ("service-$slug.log")
    $rcloneLog = Join-Path $LogsDir ("rclone-$slug.log")
    $mountRoot = [IO.Path]::GetPathRoot($localMountPoint)
    $cacheDir = Join-Path $mountRoot (".CloudFolderCache\$slug")
    $rcPort = Get-NextRcPort

    $serviceCreated = $false
    try {
        New-Item -ItemType Directory -Force -Path $mountDir,$LogsDir,$cacheDir | Out-Null
        Set-SecureDataAcl
        $mountParent = Split-Path -Parent $localMountPoint
        if (-not (Test-Path -LiteralPath $mountParent)) {
            New-Item -ItemType Directory -Force -Path $mountParent | Out-Null
        }

        if ($usingSshConfig) {
            $serviceSshConfigPath = New-ServiceSshSnapshot $mountDir $serviceSshResolved
            $externalSsh = Get-RcloneExternalSshCommand $SshHost $serviceSshConfigPath
            $rcloneConfig = @"
[remote]
type = sftp
ssh = $externalSsh
known_hosts_file = none
shell_type = unix
idle_timeout = 20s
"@
        } else {
            $rcloneConfig = @"
[remote]
type = sftp
host = $targetHost
user = $targetUser
port = $targetPort
key_file = $privateKey
known_hosts_file = $knownHosts
host_key_algorithms = $hostKeyAlgorithm
shell_type = unix
idle_timeout = 20s
"@
        }
        $rcloneConfig | Set-Content -LiteralPath $rcloneConfigPath -Encoding utf8

    $remoteSpec = if ([string]::IsNullOrWhiteSpace($targetRemotePath)) { 'remote:' } else { 'remote:' + $targetRemotePath }
    $serviceConfig = @"
[mount]
rclone_exe = "$(ConvertTo-TomlString $InstalledRclone)"
rclone_config = "$(ConvertTo-TomlString $rcloneConfigPath)"
remote = "$(ConvertTo-TomlString $remoteSpec)"
mount_point = "$(ConvertTo-TomlString $localMountPoint)"
cache_dir = "$(ConvertTo-TomlString $cacheDir)"
volume_name = "$(ConvertTo-TomlString $displayName)"
vfs_cache_mode = "$vfsCacheMode"
vfs_cache_max_size = "8Gi"
vfs_cache_max_age = "168h"
vfs_cache_min_free_space = "5Gi"
vfs_write_back = "$vfsWriteBack"
dir_cache_time = "30s"
attr_timeout = "1s"
buffer_size = "16Mi"
vfs_read_ahead = "64Mi"
vfs_cache_poll_interval = "$vfsCachePollInterval"
transfers = $transfers
file_perms = "$filePerms"
dir_perms = "$dirPerms"
windows_file_security = "$(ConvertTo-TomlString $windowsFileSecurity)"
read_only = $($ReadOnly.IsPresent.ToString().ToLowerInvariant())
rc_addr = "127.0.0.1:$rcPort"

[health]
probe_interval_secs = 10
probe_timeout_secs = 5
startup_timeout_secs = 60
failure_threshold = 3
backoff_initial_secs = 1
backoff_max_secs = 60
stable_reset_secs = 180
graceful_stop_secs = 15

[logging]
service_log = "$(ConvertTo-TomlString $serviceLog)"
rclone_log = "$(ConvertTo-TomlString $rcloneLog)"
max_bytes = 20971520
keep_files = 5
"@
    $serviceConfig | Set-Content -LiteralPath $configPath -Encoding utf8

    $metadata = [ordered]@{
        name = $displayName
        slug = $slug
        service_name = $serviceName
        host = $targetHost
        port = $targetPort
        user = $targetUser
        remote_path = $targetRemotePath
        remote_root = $remoteRoot
        mount_point = $localMountPoint
        profile = $Profile
        config_path = $configPath
        rclone_config = $rcloneConfigPath
        key_file = $privateKey
        known_hosts = $knownHosts
        ssh_alias = if ($usingSshConfig) { $SshHost } else { '' }
        ssh_config = if ($usingSshConfig) { $sshConfigPath } else { '' }
        service_ssh_config = if ($usingSshConfig) { $serviceSshConfigPath } else { '' }
        host_key_algorithm = $hostKeyAlgorithm
        windows_user_sid = $windowsUserSid
        cache_dir = $cacheDir
        service_log = $serviceLog
        rclone_log = $rcloneLog
        rc_port = $rcPort
        read_only = $ReadOnly.IsPresent
    }
        $metadataJson = $metadata | ConvertTo-Json -Depth 4
        [IO.File]::WriteAllText($metadataPath, $metadataJson, (New-Object Text.UTF8Encoding($false)))

        & $InstalledExe check $configPath
        Assert-NativeExit 'CloudFolder connection preflight'

        Remove-Item -LiteralPath $serviceLog -Force -ErrorAction SilentlyContinue
        $binPath = '"' + $InstalledExe + '" service ' + $serviceName + ' "' + $configPath + '"'
        New-Service -Name $serviceName -BinaryPathName $binPath -DisplayName ("CloudFolder - $displayName") -StartupType Automatic | Out-Null
        $serviceCreated = $true
        & sc.exe description $serviceName "CloudFolder mount: $targetUser@$targetHost`:$targetPort -> $localMountPoint" | Out-Null
        & sc.exe failure $serviceName reset= 86400 actions= restart/5000/restart/15000/restart/60000 | Out-Null
        Assert-NativeExit 'Configuring Windows service recovery'
        & sc.exe failureflag $serviceName 1 | Out-Null
        Assert-NativeExit 'Enabling Windows service recovery'
        & sc.exe config $serviceName start= delayed-auto | Out-Null
        Assert-NativeExit 'Configuring delayed automatic start'

        Start-Service -Name $serviceName
        $record = Get-Content -LiteralPath $metadataPath -Raw | ConvertFrom-Json
        Wait-MountReady $record
    } catch {
        $failure = $_
        if ($serviceCreated -and (Get-Service -Name $serviceName -ErrorAction SilentlyContinue)) {
            try { Stop-CloudFolderService $serviceName 10 } catch {}
            & sc.exe delete $serviceName | Out-Null
        }
        if ((Test-Path $InstalledExe) -and (Test-Path -LiteralPath $configPath)) {
            & $InstalledExe cleanup $configPath 2>$null | Out-Null
        }
        Remove-Item -LiteralPath $mountDir -Recurse -Force -ErrorAction SilentlyContinue
        if ((Test-Path -LiteralPath $cacheDir) -and -not (Get-ChildItem -LiteralPath $cacheDir -Force -ErrorAction SilentlyContinue | Select-Object -First 1)) {
            Remove-Item -LiteralPath $cacheDir -Force -ErrorAction SilentlyContinue
        }
        throw $failure
    }

    Write-Host ''
    Write-Host "Ready: $localMountPoint" -ForegroundColor Green
    Write-Host "Remote root: $remoteRoot" -ForegroundColor DarkGray
    Write-Host "Profile: $Profile" -ForegroundColor DarkGray
    Write-Host "It will reconnect automatically after crashes, network drops, and Windows restarts." -ForegroundColor DarkGray
    if ($Profile -eq 'Dev') {
        Write-Host "Developer workflow: cd '$localMountPoint', run 'cf enter', then use normal git/python/test commands." -ForegroundColor Cyan
    }
    if (-not $NoOpen -and (Read-YesNo 'Open the folder now?' $true)) {
        Start-Process explorer.exe -ArgumentList ('"' + $localMountPoint + '"')
    }
}

function Show-Mounts {
    $records = @(Get-MountRecords)
    if ($records.Count -eq 0) {
        Write-Host 'No CloudFolder mounts are configured.' -ForegroundColor DarkGray
        return
    }
    $rows = foreach ($record in $records) {
        $service = Get-Service -Name ([string]$record.service_name) -ErrorAction SilentlyContinue
        $status = if ($service) { [string]$service.Status } else { 'Missing' }
        [pscustomobject]@{
            Name = [string]$record.name
            Status = $status
            Remote = if (-not [string]::IsNullOrWhiteSpace([string]$record.ssh_alias)) {
                ("ssh:{0}{1}" -f $record.ssh_alias,$record.remote_path)
            } else {
                ("{0}@{1}:{2}{3}" -f $record.user,$record.host,$record.port,$record.remote_path)
            }
            LocalFolder = [string]$record.mount_point
        }
    }
    $rows | Format-Table -AutoSize
}

function Remove-Mount([string]$RequestedName) {
    Ensure-Admin
    $record = Resolve-MountRecord $RequestedName
    if (-not $Force -and -not (Read-YesNo "Remove '$($record.name)'? Remote files will NOT be deleted." $false)) { return }

    $serviceName = [string]$record.service_name
    if (Get-Service -Name $serviceName -ErrorAction SilentlyContinue) {
        # A service scheduled for SCM recovery can otherwise race removal and
        # reappear between Stop and Delete. Disable future starts/recovery first.
        & sc.exe config $serviceName start= disabled | Out-Null
        Assert-NativeExit "Disabling $serviceName before removal"
        & sc.exe failureflag $serviceName 0 | Out-Null
        Assert-NativeExit "Disabling failure recovery for $serviceName"
        Stop-CloudFolderService $serviceName
        & sc.exe delete $serviceName | Out-Null
        Assert-NativeExit "Deleting Windows service $serviceName"
        Wait-ServiceDeleted $serviceName
    }
    if ((Test-Path $InstalledExe) -and (Test-Path ([string]$record.config_path))) {
        & $InstalledExe cleanup ([string]$record.config_path) 2>$null | Out-Null
    }
    Remove-Item -LiteralPath (Split-Path -Parent ([string]$record.config_path)) -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host "Removed '$($record.name)'. Remote files were untouched." -ForegroundColor Green
    if (Test-Path -LiteralPath ([string]$record.cache_dir)) {
        Write-Host "Local VFS cache was preserved for safety: $($record.cache_dir)" -ForegroundColor DarkGray
        Write-Host 'Delete it only after you are sure no cached write still needs recovery.' -ForegroundColor DarkGray
    }
}

function Restart-Mount([string]$RequestedName) {
    Ensure-Admin
    $record = Resolve-MountRecord $RequestedName
    Stop-CloudFolderService ([string]$record.service_name)
    Remove-Item -LiteralPath ([string]$record.service_log) -Force -ErrorAction SilentlyContinue
    Start-Service -Name ([string]$record.service_name)
    Wait-MountReady $record
    Write-Host "Restarted '$($record.name)'." -ForegroundColor Green
}

function Open-Mount([string]$RequestedName) {
    $record = Resolve-MountRecord $RequestedName
    if (-not (Test-Path -LiteralPath ([string]$record.mount_point))) { throw "Mount is not currently available: $($record.mount_point)" }
    Start-Process explorer.exe -ArgumentList ('"' + [string]$record.mount_point + '"')
}

function Open-Logs([string]$RequestedName) {
    if ([string]::IsNullOrWhiteSpace($RequestedName)) {
        if (-not (Test-Path $LogsDir)) { throw 'CloudFolder log directory does not exist yet.' }
        Start-Process explorer.exe -ArgumentList ('"' + $LogsDir + '"')
        return
    }
    $record = Resolve-MountRecord $RequestedName
    Start-Process explorer.exe -ArgumentList ('/select,"' + [string]$record.service_log + '"')
}

function Invoke-Doctor {
    Write-Host 'Doctor' -ForegroundColor Cyan
    $failed = $false

    $checks = @(
        @{ Label='CloudFolder service engine'; Ok=(Test-Path $InstalledExe) },
        @{ Label='rclone'; Ok=(Test-Path $InstalledRclone) },
        @{ Label='WinFsp'; Ok=(Test-Path $WinFspDll) },
        @{ Label='Windows OpenSSH'; Ok=[bool](Get-Command ssh.exe -ErrorAction SilentlyContinue) }
    )
    foreach ($check in $checks) {
        if ($check.Ok) { Write-Host ("[OK]   " + $check.Label) -ForegroundColor Green }
        else { Write-Host ("[FAIL] " + $check.Label) -ForegroundColor Red; $failed = $true }
    }

    foreach ($record in @(Get-MountRecords)) {
        Write-Host ''
        Write-Host ("Mount: " + $record.name) -ForegroundColor Cyan
        $service = Get-Service -Name ([string]$record.service_name) -ErrorAction SilentlyContinue
        if ($service -and $service.Status -eq 'Running') { Write-Host '[OK]   Windows service is running' -ForegroundColor Green }
        else { Write-Host '[FAIL] Windows service is not running' -ForegroundColor Red; $failed = $true }

        if (Test-Path -LiteralPath ([string]$record.mount_point)) { Write-Host '[OK]   Local folder is mounted' -ForegroundColor Green }
        else { Write-Host '[FAIL] Local folder is not mounted' -ForegroundColor Red; $failed = $true }

        if ((Test-Path $InstalledExe) -and (Test-Path -LiteralPath ([string]$record.config_path))) {
            $oldPreference = $ErrorActionPreference
            try {
                $ErrorActionPreference = 'Continue'
                & $InstalledExe check-remote ([string]$record.config_path)
                if ($LASTEXITCODE -eq 0) { Write-Host '[OK]   SFTP connection and host-key verification' -ForegroundColor Green }
                else { Write-Host '[FAIL] SFTP connection check' -ForegroundColor Red; $failed = $true }
            } finally {
                $ErrorActionPreference = $oldPreference
            }
        }
    }

    Write-Host ''
    if ($failed) {
        Write-Host "Some checks failed. Logs: $LogsDir" -ForegroundColor Yellow
        return $false
    }
    Write-Host 'Everything looks healthy.' -ForegroundColor Green
    return $true
}

function Uninstall-CloudFolder {
    Ensure-Admin
    if (-not $Force -and -not (Read-YesNo 'Uninstall CloudFolder? Remote files will NOT be deleted.' $false)) { return }
    foreach ($record in @(Get-MountRecords)) {
        Stop-CloudFolderService ([string]$record.service_name)
        if (Get-Service -Name ([string]$record.service_name) -ErrorAction SilentlyContinue) {
            & sc.exe delete ([string]$record.service_name) | Out-Null
        }
        if ((Test-Path $InstalledExe) -and (Test-Path -LiteralPath ([string]$record.config_path))) {
            & $InstalledExe cleanup ([string]$record.config_path) 2>$null | Out-Null
        }
        if ($PurgeCache -and (Test-Path -LiteralPath ([string]$record.cache_dir))) {
            Remove-Item -LiteralPath ([string]$record.cache_dir) -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
    if ($PurgeCache) {
        Get-PSDrive -PSProvider FileSystem -ErrorAction SilentlyContinue | ForEach-Object {
            $cacheRoot = Join-Path $_.Root '.CloudFolderCache'
            if (Test-Path -LiteralPath $cacheRoot) {
                Remove-Item -LiteralPath $cacheRoot -Recurse -Force -ErrorAction SilentlyContinue
            }
        }
    }
    Start-Sleep -Seconds 1
    Remove-Item -LiteralPath $DataDir -Recurse -Force -ErrorAction SilentlyContinue
    Remove-CloudFolderFromPath
    Remove-Item -LiteralPath $InstallDir -Recurse -Force -ErrorAction SilentlyContinue
    $shortcutFolder = Join-Path ([Environment]::GetFolderPath('CommonPrograms')) 'CloudFolder'
    Remove-Item -LiteralPath $shortcutFolder -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host 'CloudFolder uninstalled. Remote files were untouched.' -ForegroundColor Green
}

function Invoke-Menu {
    while ($true) {
        Clear-Host
        Write-Banner
        if (-not (Test-Path $InstalledExe)) {
            Write-Host 'CloudFolder is not installed yet.' -ForegroundColor Yellow
            if (Read-YesNo 'Install it now?' $true) {
                Install-Runtime
            } else {
                return
            }
        }

        Show-Mounts
        Write-Host ''
        Write-Host '1. Add a remote folder'
        Write-Host '2. Open a folder'
        Write-Host '3. Restart a mount'
        Write-Host '4. Remove a mount'
        Write-Host '5. Doctor / troubleshoot'
        Write-Host '6. Open logs'
        Write-Host '7. Exit'
        Write-Host ''
        $choice = (Read-Host 'Choose').Trim()
        try {
            switch ($choice) {
                '1' { Add-Mount }
                '2' { Open-Mount '' }
                '3' { Restart-Mount '' }
                '4' { Remove-Mount '' }
                '5' { [void](Invoke-Doctor); Read-Host 'Press Enter to continue' | Out-Null }
                '6' { Open-Logs '' }
                '7' { return }
                default { Write-Host 'Unknown choice.' -ForegroundColor Yellow; Start-Sleep -Seconds 1 }
            }
        } catch {
            Write-Host ''
            Write-Host ('Error: ' + $_.Exception.Message) -ForegroundColor Red
            Read-Host 'Press Enter to continue' | Out-Null
        }
    }
}

Write-Banner

switch ($Action) {
    'Install' {
        Ensure-Admin
        Install-Runtime
        if (-not $SkipAdd -and @(Get-MountRecords).Count -eq 0) {
            Write-Host ''
            if (Read-YesNo 'Add your first remote folder now?' $true) { Add-Mount }
        }
    }
    'Add' { Add-Mount }
    'List' { Show-Mounts }
    'Remove' { Remove-Mount $Name }
    'Restart' { Restart-Mount $Name }
    'Open' { Open-Mount $Name }
    'Logs' { Open-Logs $Name }
    'Doctor' { if (-not (Invoke-Doctor)) { exit 2 } }
    'Uninstall' { Uninstall-CloudFolder }
    default { Ensure-Admin; Invoke-Menu }
}
