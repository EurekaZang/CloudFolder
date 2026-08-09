[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$MountPoint,
    [int]$Iterations = 100,
    [int]$Parallel = 8
)
$ErrorActionPreference = 'Stop'
if (-not (Test-Path $MountPoint)) { throw "Mount not available: $MountPoint" }

$root = Join-Path $MountPoint ('.cloudfolder-test-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $root | Out-Null
try {
    $payload = New-Object byte[] (1024 * 1024)
    $rng = [Security.Cryptography.RandomNumberGenerator]::Create()
    $rng.GetBytes($payload)
    $sha = [Security.Cryptography.SHA256]::Create()
    $expectedHash = [BitConverter]::ToString($sha.ComputeHash($payload)).Replace('-', '')
    for ($i = 0; $i -lt $Iterations; $i++) {
        $path = Join-Path $root ("unicode-测试-$i.bin")
        [IO.File]::WriteAllBytes($path, $payload)
        $read = [IO.File]::ReadAllBytes($path)
        $actualHash = [BitConverter]::ToString($sha.ComputeHash($read)).Replace('-', '')
        if ($actualHash -ne $expectedHash) { throw "content hash mismatch at iteration $i" }
        $renamed = Join-Path $root ("renamed-$i.bin")
        Move-Item -LiteralPath $path -Destination $renamed
        Remove-Item -LiteralPath $renamed
    }

    $jobs = 1..$Parallel | ForEach-Object {
        $id = $_
        Start-Job -ScriptBlock {
            param($Root,$Id)
            1..20 | ForEach-Object {
                $p = Join-Path $Root ("parallel-$Id-$_.txt")
                [IO.File]::WriteAllText($p, ('x' * 65536))
                if ([IO.File]::ReadAllText($p).Length -ne 65536) { throw 'parallel read mismatch' }
                Remove-Item $p
            }
        } -ArgumentList $root,$id
    }
    $jobs | Wait-Job | Receive-Job
    $failed = $jobs | Where-Object State -ne 'Completed'
    $jobs | Remove-Job -Force
    if ($failed) { throw 'parallel I/O job failed' }
    Write-Host "PASS: $Iterations sequential write/read/rename/delete cycles + $Parallel parallel workers"
} finally {
    if ($sha) { $sha.Dispose() }
    if ($rng) { $rng.Dispose() }
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}
