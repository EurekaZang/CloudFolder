[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Mount,
    [string]$CfPath = ''
)

$ErrorActionPreference = 'Stop'

function Assert-NativeExit([string]$Operation) {
    if ($LASTEXITCODE -ne 0) { throw "$Operation failed with exit code $LASTEXITCODE" }
}

function Get-CloudFolderRecord([string]$RequestedMount) {
    $records = @()
    $root = 'C:\ProgramData\CloudFolder\mounts'
    if (Test-Path -LiteralPath $root) {
        Get-ChildItem -LiteralPath $root -Directory -ErrorAction SilentlyContinue | ForEach-Object {
            $path = Join-Path $_.FullName 'mount.json'
            if (Test-Path -LiteralPath $path) {
                try { $records += (Get-Content -LiteralPath $path -Raw | ConvertFrom-Json) } catch {}
            }
        }
    }
    $matches = @($records | Where-Object {
        $_.name -ieq $RequestedMount -or $_.slug -ieq $RequestedMount -or $_.service_name -ieq $RequestedMount
    })
    if ($matches.Count -ne 1) { throw "Could not uniquely find CloudFolder mount '$RequestedMount'." }
    return $matches[0]
}

if ([string]::IsNullOrWhiteSpace($CfPath)) {
    $command = Get-Command cf.exe -ErrorAction Stop
    $CfPath = $command.Source
}
$CfPath = (Resolve-Path -LiteralPath $CfPath).Path
$runtimeDir = Split-Path -Parent $CfPath
$record = Get-CloudFolderRecord $Mount
$mountRoot = [string]$record.mount_point
if (-not (Test-Path -LiteralPath $mountRoot)) { throw "Mount is unavailable: $mountRoot" }

$gateName = '.cloudfolder-formal-gate-' + [guid]::NewGuid().ToString('N').Substring(0,8)
$gateRoot = Join-Path $mountRoot $gateName
$gateData = Join-Path $env:TEMP ('CloudFolder-formal-gate-' + [guid]::NewGuid().ToString('N'))
$profileState = Join-Path (Split-Path -Parent ([string]$record.rclone_config)) 'environment-profile'
$profileStateExisted = Test-Path -LiteralPath $profileState
$profileStateBytes = if ($profileStateExisted) { [IO.File]::ReadAllBytes($profileState) } else { $null }
$oldLocation = Get-Location
$oldPath = $env:PATH
$oldMount = $env:CLOUDFOLDER_ENTER_MOUNT
$oldData = $env:CLOUDFOLDER_DATA_DIR
$oldRuntime = $env:CLOUDFOLDER_RUNTIME_DIR
$jobId = $null
$httpJobId = $null
$localPort = $null

try {
    New-Item -ItemType Directory -Force -Path $gateRoot,$gateData | Out-Null
    @'
[environment]
shell = "bash -lc"
init = "export CLOUDFOLDER_GATE_BASE=base"

[environment.profiles.gate]
init = "export CLOUDFOLDER_GATE_PROFILE=gate"
'@ | Set-Content -LiteralPath (Join-Path $gateRoot '.cloudfolder.toml') -Encoding utf8

    Set-Location -LiteralPath $gateRoot
    $env:CLOUDFOLDER_DATA_DIR = $gateData
    $env:CLOUDFOLDER_RUNTIME_DIR = $runtimeDir

    Write-Host '[1/4] Execution Router: enter + git/edit/test/commit without cf run' -ForegroundColor Cyan
    $psi = New-Object Diagnostics.ProcessStartInfo
    $psi.FileName = $CfPath
    $psi.Arguments = 'enter "' + $Mount.Replace('"','\"') + '"'
    $psi.WorkingDirectory = $gateRoot
    $psi.UseShellExecute = $false
    $psi.RedirectStandardInput = $true
    $psi.EnvironmentVariables['CLOUDFOLDER_DATA_DIR'] = $gateData
    $enter = [Diagnostics.Process]::Start($psi)
    $enter.StandardInput.WriteLine('exit')
    $enter.StandardInput.Close()
    $enter.WaitForExit()
    if ($enter.ExitCode -ne 0) { throw "cf enter failed with exit code $($enter.ExitCode)" }

    $gitShim = Get-ChildItem -LiteralPath (Join-Path $gateData 'router') -Recurse -Filter 'git.exe' -File -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $gitShim) { throw 'Execution Router did not create git.exe shim.' }
    $router = Split-Path -Parent $gitShim.FullName
    $env:CLOUDFOLDER_ENTER_MOUNT = $Mount
    $env:PATH = $router + ';' + $oldPath

    & (Join-Path $router 'git.exe') init | Out-Host
    Assert-NativeExit 'routed git init'
    & (Join-Path $router 'git.exe') config user.email 'cloudfolder-gate@example.invalid'
    Assert-NativeExit 'routed git config email'
    & (Join-Path $router 'git.exe') config user.name 'CloudFolder Formal Gate'
    Assert-NativeExit 'routed git config name'
    @'
def add(a, b):
    return a + b
'@ | Set-Content -LiteralPath (Join-Path $gateRoot 'calc.py') -Encoding utf8
    @'
import unittest
from calc import add

class CalcTest(unittest.TestCase):
    def test_add(self):
        self.assertEqual(add(20, 22), 42)

if __name__ == "__main__":
    unittest.main()
'@ | Set-Content -LiteralPath (Join-Path $gateRoot 'test_calc.py') -Encoding utf8
    & (Join-Path $router 'python.exe') -m unittest -q
    Assert-NativeExit 'routed Python test'
    & (Join-Path $router 'git.exe') add calc.py test_calc.py .cloudfolder.toml
    Assert-NativeExit 'routed git add'
    & (Join-Path $router 'git.exe') commit -m 'CloudFolder formal gate'
    Assert-NativeExit 'routed git commit'
    $commit = (& (Join-Path $router 'git.exe') log -1 --oneline | Out-String).Trim()
    Assert-NativeExit 'routed git log'
    if ([string]::IsNullOrWhiteSpace($commit)) { throw 'Router gate did not create a commit.' }

    Write-Host '[2/4] Workspace Environment: one profile selection, inherited everywhere' -ForegroundColor Cyan
    & $CfPath env use gate | Out-Host
    Assert-NativeExit 'cf env use gate'
    $envText = (& (Join-Path $router 'python.exe') -c "import os; print(os.getenv('CLOUDFOLDER_GATE_BASE')); print(os.getenv('CLOUDFOLDER_GATE_PROFILE'))" | Out-String)
    Assert-NativeExit 'routed environment probe'
    if ($envText -notmatch '(?m)^base\s*$' -or $envText -notmatch '(?m)^gate\s*$') { throw 'Workspace environment was not inherited by routed Python.' }

    Write-Host '[3/4] Persistent Jobs: detach, outlive local SSH command, recover status/logs' -ForegroundColor Cyan
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $jobOut = (& $CfPath job run -- python -c "import os,time; print('JOB_ENV='+str(os.getenv('CLOUDFOLDER_GATE_PROFILE')), flush=True); time.sleep(3); print('JOB_DONE', flush=True)" 2>&1 | Out-String)
    $sw.Stop()
    Assert-NativeExit 'cf job run'
    $match = [regex]::Match($jobOut, 'cf-[0-9a-f]{8}')
    if (-not $match.Success) { throw 'Persistent job did not return a CloudFolder job id.' }
    $jobId = $match.Value
    if ($sw.Elapsed.TotalSeconds -ge 3) { throw 'Persistent job launch blocked instead of detaching.' }
    Start-Sleep -Seconds 5
    $jobList = (& $CfPath job list | Out-String)
    Assert-NativeExit 'cf job list'
    $jobLogs = (& $CfPath job logs $jobId | Out-String)
    Assert-NativeExit 'cf job logs'
    if ($jobList -notmatch [regex]::Escape($jobId) -or $jobList -notmatch 'exited\(0\)') { throw 'Persistent job state was not recovered as exited(0).' }
    if ($jobLogs -notmatch 'JOB_ENV=gate' -or $jobLogs -notmatch 'JOB_DONE') { throw 'Persistent job log recovery failed.' }

    Write-Host '[4/4] Port Forwarding: remote HTTP service -> localhost without raw ssh -L' -ForegroundColor Cyan
    $remotePortText = (& (Join-Path $router 'python.exe') -c "import socket; s=socket.socket(); s.bind(('127.0.0.1',0)); print(s.getsockname()[1]); s.close()" | Out-String).Trim()
    Assert-NativeExit 'remote free-port probe'
    $remotePort = [int]$remotePortText
    $httpOut = (& $CfPath job run -- python -m http.server $remotePort --bind 127.0.0.1 2>&1 | Out-String)
    Assert-NativeExit 'remote HTTP job'
    $httpMatch = [regex]::Match($httpOut, 'cf-[0-9a-f]{8}')
    if (-not $httpMatch.Success) { throw 'HTTP test job did not return a job id.' }
    $httpJobId = $httpMatch.Value
    Start-Sleep -Seconds 2
    $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    $listener.Start()
    $localPort = $listener.LocalEndpoint.Port
    $listener.Stop()
    & $CfPath forward $remotePort $localPort
    Assert-NativeExit 'cf forward'
    Start-Sleep -Milliseconds 500
    $response = Invoke-WebRequest -UseBasicParsing -Uri ("http://127.0.0.1:$localPort/") -TimeoutSec 10
    if ($response.StatusCode -ne 200 -or $response.Content -notmatch 'calc\.py') { throw 'Forwarded HTTP endpoint did not reach the remote gate workspace.' }
    & $CfPath forward stop $localPort | Out-Host
    Assert-NativeExit 'cf forward stop'
    $localPort = $null
    & $CfPath job stop $httpJobId | Out-Host
    Assert-NativeExit 'cf job stop HTTP service'

    Write-Host 'FORMAL GATE PASSED: Router + Environment + Persistent Jobs + Port Forwarding' -ForegroundColor Green
} finally {
    try {
        if ($null -ne $localPort) { & $CfPath forward stop $localPort 2>$null | Out-Null }
    } catch {}
    try {
        if ($null -ne $httpJobId) { & $CfPath job stop $httpJobId 2>$null | Out-Null }
    } catch {}
    try {
        $ids = @($jobId,$httpJobId) | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
        if ($ids.Count -gt 0) {
            $remoteCleanup = 'rm -rf ' + (($ids | ForEach-Object { '"$HOME/.cloudfolder/jobs/' + $_ + '"' }) -join ' ')
            & $CfPath sh -- $remoteCleanup 2>$null | Out-Null
        }
    } catch {}
    try {
        if ($profileStateExisted) { [IO.File]::WriteAllBytes($profileState, $profileStateBytes) }
        else { Remove-Item -LiteralPath $profileState -Force -ErrorAction SilentlyContinue }
    } catch {}
    try {
        Set-Location $oldLocation
        & $CfPath sh $Mount -- ("rm -rf -- '" + $gateName + "'") 2>$null | Out-Null
        Remove-Item -LiteralPath $gateData -Recurse -Force -ErrorAction SilentlyContinue
    } catch {}
    $env:PATH = $oldPath
    if ($null -eq $oldMount) { Remove-Item Env:CLOUDFOLDER_ENTER_MOUNT -ErrorAction SilentlyContinue } else { $env:CLOUDFOLDER_ENTER_MOUNT = $oldMount }
    if ($null -eq $oldData) { Remove-Item Env:CLOUDFOLDER_DATA_DIR -ErrorAction SilentlyContinue } else { $env:CLOUDFOLDER_DATA_DIR = $oldData }
    if ($null -eq $oldRuntime) { Remove-Item Env:CLOUDFOLDER_RUNTIME_DIR -ErrorAction SilentlyContinue } else { $env:CLOUDFOLDER_RUNTIME_DIR = $oldRuntime }
}
