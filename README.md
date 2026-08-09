# CloudFolder

**Mount a remote Linux/SFTP folder as a normal Windows folder — and keep it alive automatically.**

CloudFolder turns a remote SSH/SFTP directory into something you can open from Explorer, VS Code, Python, Git tools, or any other normal Windows application. It is built on **rclone + WinFsp**, with a small Rust Windows Service that adds watchdogs, crash recovery, safe cleanup, and isolated multi-mount management.

> Current friendly installer target: **Windows 10/11 x64 + SFTP/SSH servers**.

## 中文快速开始

如果你只想把 Linux 服务器上的文件夹像本地目录一样放进 Windows，不需要先学习 rclone 或 WinFsp：

1. 在 **Releases** 下载 `CloudFolder-windows-x64.zip`；
2. 解压后双击 **`Install CloudFolder.cmd`**；
3. 按提示填写服务器地址、SSH 端口、用户名、远端目录和本地目录；
4. 第一次连接时确认服务器 SSH 指纹；如果服务器还没安装 CloudFolder 公钥，OpenSSH 会让你输入一次 SSH 密码。密码不会被 CloudFolder 保存；
5. 完成后直接从资源管理器、VS Code、Python 等软件访问这个本地目录。断网、rclone 崩溃或 Windows Service 被杀后，CloudFolder 会负责自动恢复。

之后可以从 **开始菜单 → CloudFolder → CloudFolder Manager** 添加、打开、重启、诊断或删除挂载。

## Install in three steps

1. Open the latest **GitHub Release** and download `CloudFolder-windows-x64.zip`.
2. Extract it.
3. Double-click **`Install CloudFolder.cmd`**.

CloudFolder will install its runtime, WinFsp and rclone, then ask only for the information a normal SSH user already knows:

- a friendly name, such as `Lab Server`;
- server IP/hostname;
- SSH port (`22` by default);
- SSH username;
- remote folder (blank means the user's SSH home directory);
- local Windows folder (a sensible default is provided).

If the server does not already trust the CloudFolder key, Windows OpenSSH will show the server fingerprint and ask for the SSH password **once**. OpenSSH reads that password directly; CloudFolder does not capture or store it. From then on, the Windows service uses public-key authentication.

After installation, open **Start menu → CloudFolder → CloudFolder Manager** to add, open, restart, diagnose, or remove mounts.

### PowerShell bootstrap

If you prefer not to download the ZIP manually:

```powershell
iwr https://raw.githubusercontent.com/EurekaZang/CloudFolder/main/install.ps1 -OutFile "$env:TEMP\install-cloudfolder.ps1"
powershell -ExecutionPolicy Bypass -File "$env:TEMP\install-cloudfolder.ps1"
```

The bootstrap downloads the latest GitHub Release and launches the same setup flow.

## What it looks like

For example, you can map:

```text
alice@server.example.com:/home/alice/projects
```

to:

```text
C:\Users\Alice\CloudFolder\Lab Server
```

Then Windows applications simply see normal paths such as:

```text
C:\Users\Alice\CloudFolder\Lab Server\robotics\train.py
```

There is no separate FTP-style file browser and no manual reconnect command to remember.

## Why CloudFolder exists

`rclone mount` + WinFsp can already mount remote storage on Windows. The annoying part is making that mount behave like dependable machine infrastructure instead of a terminal command that eventually dies.

CloudFolder adds the lifecycle/reliability layer:

- **one Windows Service per mount**, so one broken server does not take other mounts down;
- child-process liveness checks roughly every second;
- a separate **killable filesystem health probe**, so a hung filesystem call cannot hang the watchdog itself;
- automatic rclone replacement after crashes;
- bounded exponential reconnect backoff with jitter;
- Windows SCM recovery after the supervisor itself is killed;
- a Windows **Job Object with `KILL_ON_JOB_CLOSE`**, preventing orphaned mount processes;
- graceful rclone RC shutdown with PID verification;
- stale reparse-point cleanup;
- refusal to hide/delete a non-empty normal directory at the mount path;
- independent RC port, cache and logs for every mount;
- bounded VFS cache and minimum-free-space protection;
- strict SSH `known_hosts` verification;
- host-key algorithm pinning based on the algorithm Windows OpenSSH actually negotiated;
- safe runtime upgrades that stop/restart all CloudFolder mount services around the shared binary update.

## Architecture

```text
Explorer / VS Code / Python / normal Windows apps
                     │
                     ▼
                Windows path
                     │
                  WinFsp
                     │
                     ▼
               rclone mount
                     │
                  SFTP/SSH
                     │
                     ▼
               Linux server

CloudFolderService.exe supervises each rclone mount from the side:
health probes · crash recovery · backoff · logs · safe cleanup · SCM recovery
```

CloudFolder does **not** replace WinFsp or rclone. WinFsp provides the Windows userspace-filesystem bridge; rclone provides the SFTP/VFS mount engine; CloudFolder makes the whole combination persistent and self-healing.

## CloudFolder Manager

The interactive manager intentionally keeps the UI small:

```text
1. Add a remote folder
2. Open a folder
3. Restart a mount
4. Remove a mount
5. Doctor / troubleshoot
6. Open logs
7. Exit
```

Removing a CloudFolder mount removes the **local mount/service configuration only**. It does not delete remote files. The local VFS cache is preserved by default because, after a network failure, it may be the last place containing an uncommitted write. `Uninstall -PurgeCache` explicitly removes CloudFolder cache roots.

## Defaults chosen for normal users

- Local folder: `%USERPROFILE%\CloudFolder\<name>`
- Dedicated key: `%USERPROFILE%\.ssh\cloudfolder_ed25519`
- Authentication: SSH public key; password is never stored
- VFS cache: `full`, maximum `8 GiB`
- Minimum free space: `5 GiB`
- Write-back delay: `5s`
- Health probe: every `10s`, `5s` timeout, recycle after 3 consecutive failures
- rclone idle SFTP connections: `20s`
- Windows service startup: automatic (delayed)

Advanced users can edit the generated TOML/INI files under `C:\ProgramData\CloudFolder\mounts\<name>\` and restart the corresponding `CloudFolder.<name>` service.

## Security model

An unattended Windows service cannot type a key passphrase after every reboot. CloudFolder therefore creates a dedicated **un-encrypted SSH private key** by default and protects access using Windows ACLs. LocalSystem receives read access because it runs the mount service.

The public key is installed on the server only after Windows OpenSSH presents the host fingerprint. `known_hosts` verification remains strict for all later connections. CloudFolder never writes an SSH password into rclone config, TOML, logs, environment variables, or command-line arguments.

See [SECURITY.md](SECURITY.md) for details.

## Limitations

- The friendly manager currently configures **SFTP** mounts. rclone supports many other backends, but exposing those safely in the beginner UI is future work.
- CloudFolder is a live remote filesystem, **not an offline-sync mirror**. Network latency and server performance still matter.
- POSIX permissions, ownership and symlink identity cannot always map perfectly onto Windows filesystem semantics.
- Exact symlink semantics are not preserved as native Windows symlinks by the current rclone SFTP projection.
- Releases are currently **not Authenticode code-signed**, so Windows SmartScreen may show an unknown-publisher warning. Release ZIP SHA-256 checksums are published alongside each release.

## Troubleshooting

Open **CloudFolder Manager → Doctor / troubleshoot**. Doctor checks:

- CloudFolder service engine;
- rclone;
- WinFsp;
- Windows OpenSSH;
- every configured Windows service;
- every local mountpoint;
- fresh strict SFTP connectivity for each mount.

Logs are under:

```text
C:\ProgramData\CloudFolder\logs\
```

## For developers

End users do **not** need Rust.

To build from source on Windows:

```powershell
.\scripts\build.ps1
```

The local build script uses the Windows GNU Rust target and an ASCII-only Cargo target directory so it also works when the repository path contains Unicode characters. GitHub Actions builds release binaries on `windows-latest` using the standard MSVC toolchain.

Useful validation commands:

```powershell
.\scripts\smoke-test.ps1 -MountPoint 'C:\Users\Alice\CloudFolder\Lab Server'

# Destructive resilience testing; run elevated and only against a disposable/test mount.
.\scripts\fault-test.ps1 `
  -ServiceName 'CloudFolder.lab-server' `
  -MountPoint 'C:\Users\Alice\CloudFolder\Lab Server' `
  -RemoteHost 'server.example.com' `
  -RemotePort 22 `
  -RcPort 55770
```

CI runs Rust formatting, tests, Clippy and Windows PowerShell 5.1 parser checks. A `v*` tag builds `CloudFolder-windows-x64.zip` and publishes it as a GitHub Release automatically.

## Credits

CloudFolder stands on excellent existing projects:

- [rclone](https://rclone.org/) — remote storage and VFS mount engine;
- [WinFsp](https://winfsp.dev/) — Windows userspace filesystem infrastructure;
- [windows-service](https://crates.io/crates/windows-service) — Rust Windows Service integration.

## License

MIT. See [LICENSE](LICENSE).
