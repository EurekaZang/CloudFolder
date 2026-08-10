# CloudFolder

**[中文](README.md) | [English](README.en.md) | [日本語](README.ja.md)**

**Your remote Linux workspace, mounted locally. Your coding agent stays local.**

CloudFolder turns a remote SSH/SFTP workspace into a normal Windows path for Explorer, VS Code, Claude Code, Codex, Python tooling, and other local applications. Files stay on the remote machine, while a local coding agent can read and edit them through Windows without being redeployed on every server.

Under the filesystem layer, CloudFolder is built on **rclone + WinFsp**. A small Rust Windows Service keeps each mount alive, while the native **`cf.exe`** CLI bridges terminal commands back to the matching remote Linux working directory.

> Current beginner-friendly installer target: **Windows 10/11 x64 + SSH/SFTP servers**.

## Local Agent + remote Linux workflow

The intended developer workflow is:

```powershell
cd (cf path lab)

# The agent itself runs on your Windows machine.
claude
# or: codex

# Files are edited locally through the mounted workspace.
# Linux/Git/toolchain commands run on the remote machine in the matching cwd.
cf here
cf run -- git status
cf run -- pytest -q
cf run -- cargo test
cf sh -- "git status && pytest -q"
```

`cf run` is more than an SSH shortcut. Before execution it waits until pending VFS writes have reached the server, maps the current Windows subdirectory to the corresponding absolute Linux directory, executes there with strict SSH host verification, returns the remote exit code, and then invalidates the local directory cache so new remote artifacts appear locally.

This split is deliberate:

- **local Windows path:** editor/agent file reads, targeted searches, edits, creates, renames and deletes;
- **remote Linux via `cf run`:** Git, tests, builds, compilers, package managers, project interpreters and repository-wide commands that touch many small files.

Running `git status` directly against a cold SFTP-mounted `.git` directory can be slow because Git performs many small random metadata/object accesses. CloudFolder therefore treats remote execution as a first-class part of the workspace rather than pretending every local CLI workload is equally suited to SFTP.

### Teach Claude Code and Codex automatically

CloudFolder can install a small **conditional** user-level instruction block for both agents:

```powershell
cf agent setup
```

It updates only the CloudFolder-managed block in:

```text
%USERPROFILE%\.claude\CLAUDE.md
%USERPROFILE%\.codex\AGENTS.md
```

Existing instructions are preserved. The managed block tells the agent to edit mounted files normally, but to use `cf run`/`cf sh` for Git, builds, tests and large repository scans whenever it is inside a CloudFolder workspace. Setup is **opt-in**; the normal CloudFolder installer does not modify agent instructions automatically.

Check or remove the integration at any time:

```powershell
cf agent status
cf agent remove
```

## Install in three steps

1. Open the latest **GitHub Release** and download `CloudFolder-windows-x64.zip`.
2. Extract it.
3. Double-click **`Install CloudFolder.cmd`**.

CloudFolder installs its runtime, WinFsp, and rclone automatically. It then asks only for information a normal SSH user already knows:

- a friendly name, such as `Lab Server`;
- server IP address or hostname;
- SSH port (`22` by default);
- SSH username;
- remote directory (leave it blank to use the SSH user's home directory);
- local Windows folder (a sensible default is provided).

If the server does not already trust the CloudFolder key, Windows OpenSSH first shows the server fingerprint and then asks for the SSH password **once**. OpenSSH reads that password directly; CloudFolder does not capture or store it. From then on, the Windows service uses public-key authentication only.

After installation, open **Start menu → CloudFolder → CloudFolder Manager** to add, open, restart, diagnose, or remove mounts. New terminals also get the native `cf` command on `PATH`.

### PowerShell bootstrap

If you prefer not to download the ZIP manually:

```powershell
iwr https://raw.githubusercontent.com/EurekaZang/CloudFolder/main/install.ps1 -OutFile "$env:TEMP\install-cloudfolder.ps1"
powershell -ExecutionPolicy Bypass -File "$env:TEMP\install-cloudfolder.ps1"
```

The bootstrap downloads the latest GitHub Release, verifies it, and launches the same setup flow.

## What it looks like

For example, map:

```text
alice@server.example.com:/home/alice/projects
```

to:

```text
C:\Users\Alice\CloudFolder\Lab Server
```

Windows applications then see ordinary paths such as:

```text
C:\Users\Alice\CloudFolder\Lab Server\robotics\train.py
```

There is no separate FTP-style file browser and no reconnect command to remember after a network interruption. A coding agent can open this same path as its local workspace.

## Why CloudFolder exists

`rclone mount` + WinFsp can already mount remote storage on Windows. The difficult part is making that mount behave like dependable machine infrastructure instead of a terminal command that eventually fails because of a network change, process crash, or system restart.

CloudFolder adds the lifecycle and reliability layer:

- **one Windows Service per mount**, so one unhealthy server does not take other mounts down;
- child-process liveness checks roughly every second;
- a separate **killable filesystem health probe**, so a hung filesystem call cannot hang the watchdog itself;
- automatic rclone replacement after crashes;
- bounded exponential reconnect backoff with jitter;
- Windows SCM recovery if the supervisor itself is killed;
- a Windows **Job Object with `KILL_ON_JOB_CLOSE`**, preventing orphaned mount processes;
- graceful rclone RC shutdown with PID verification;
- stale reparse-point cleanup;
- refusal to hide or overwrite a non-empty normal directory at the mount path;
- independent RC port, cache, and logs for every mount;
- bounded VFS cache and minimum-free-space protection;
- strict SSH `known_hosts` verification;
- host-key algorithm pinning based on what Windows OpenSSH actually negotiated;
- safe runtime upgrades that stop and restore all CloudFolder mount services around the shared binary update.

## Architecture

```text
Claude Code / Codex / VS Code / Explorer
                 │
                 ├──── normal file I/O ────┐
                 │                          ▼
                 │                    Windows path
                 │                          │
                 │                        WinFsp
                 │                          │
                 │                     rclone VFS
                 │                          │
                 │                        SFTP
                 │                          │
                 │                          ▼
                 └── cf run / cf sh ── SSH ──► Linux workspace

CloudFolderService.exe supervises every rclone mount:
health probes → crash recovery → backoff → logs → safe cleanup → SCM recovery

cf.exe supplies the terminal bridge:
flush pending writes → map cwd → execute remotely → preserve exit code → refresh local view
```

CloudFolder does **not** replace WinFsp or rclone. WinFsp provides the Windows userspace-filesystem bridge, rclone provides the SFTP/VFS mount engine, and CloudFolder makes the combination persistent, self-healing, and manageable.

## CloudFolder Manager

The interactive manager intentionally stays small:

```text
1. Add a remote folder
2. Open a folder
3. Restart a mount
4. Remove a mount
5. Doctor / troubleshoot
6. Open logs
7. Exit
```

Removing a CloudFolder mount removes the **local mount and service configuration only**. It does not delete remote files. The local VFS cache is preserved by default because, after a network failure, it may be the last place containing an uncommitted write. `Uninstall -PurgeCache` explicitly removes CloudFolder cache roots.

## Defaults for normal users

- Local folder: `%USERPROFILE%\CloudFolder\<name>`
- Dedicated key: `%USERPROFILE%\.ssh\cloudfolder_ed25519`
- Authentication: SSH public key; SSH passwords are never stored
- VFS cache: `full`, maximum `8 GiB`
- Minimum free space: `5 GiB`
- New mount profile: `Dev`
- Developer write-back delay: `1s`; `cf run` still uses an explicit flush barrier before remote execution
- Concurrent VFS uploads: `8`
- Windows filesystem ACL: the installing user's SID is the filesystem owner with FullControl; LocalSystem and Administrators also retain FullControl
- Health probe: every `10s`, `5s` timeout, recycle after 3 consecutive failures
- rclone idle SFTP connections: `20s`
- Windows service startup: automatic (delayed)

Advanced users can edit the generated TOML/INI files under `C:\ProgramData\CloudFolder\mounts\<name>\` and restart the corresponding `CloudFolder.<name>` service.

## Security model

An unattended Windows service cannot type a key passphrase after every reboot. CloudFolder therefore creates a dedicated **unencrypted SSH private key** by default and protects it with Windows ACLs. LocalSystem receives read access because it runs the mount service.

Mount services themselves run as LocalSystem for reliability. To avoid the normal-user permission and Git-ownership problems that a SYSTEM-owned WinFsp filesystem would otherwise create, CloudFolder generates a per-user WinFsp `FileSecurity` descriptor: the installing Windows SID becomes the filesystem owner with FullControl, while SYSTEM and Administrators retain FullControl. It does **not** grant Everyone full access.

The public key is installed on the server only after Windows OpenSSH presents the host fingerprint. `known_hosts` verification remains strict for all later connections. CloudFolder never writes an SSH password into rclone configuration, TOML, logs, environment variables, or command-line arguments.

See [SECURITY.md](SECURITY.md) for details.

## Limitations

- The beginner-friendly manager currently configures **SFTP** mounts. rclone supports many other backends, but exposing them safely in the simple UI is future work.
- CloudFolder is a live remote filesystem, **not an offline-sync mirror**. Network latency and server performance still matter.
- Direct local Git operations and cold repository-wide scans can be slow over SFTP. Prefer `cf run -- git ...`, `cf run -- rg ...`, builds, tests, package managers and similar high-fan-out workloads on the remote Linux host.
- POSIX permissions, ownership, and symlink identity cannot always map perfectly onto Windows filesystem semantics.
- Exact Linux symlink semantics are not preserved as native Windows symlinks by the current rclone SFTP projection.
- Releases are currently **not Authenticode code-signed**, so Windows SmartScreen may show an unknown-publisher warning. A SHA-256 checksum is published next to every Release ZIP.

## Troubleshooting

Open **CloudFolder Manager → Doctor / troubleshoot**. Doctor checks:

- the CloudFolder service engine;
- rclone;
- WinFsp;
- Windows OpenSSH;
- every configured Windows Service;
- every local mount point;
- fresh strict SFTP connectivity for each mount.

Logs are stored under:

```text
C:\ProgramData\CloudFolder\logs\
```

## For developers

End users do **not** need Rust.

To build from source on Windows:

```powershell
.\scripts\build.ps1
```

The local build script uses the Windows GNU Rust target and an ASCII-only Cargo target directory, so it also works when the repository path contains Unicode characters. GitHub Actions builds Release binaries on `windows-latest` with the standard MSVC toolchain.

Useful validation commands:

```powershell
.\scripts\smoke-test.ps1 -MountPoint 'C:\Users\Alice\CloudFolder\Lab Server'

# Destructive resilience test. Run elevated and only against a disposable test mount.
.\scripts\fault-test.ps1 `
  -ServiceName 'CloudFolder.lab-server' `
  -MountPoint 'C:\Users\Alice\CloudFolder\Lab Server' `
  -RemoteHost 'server.example.com' `
  -RemotePort 22 `
  -RcPort 55770
```

CI runs Rust formatting, tests, Clippy, and Windows PowerShell 5.1 parser checks. A `v*` tag automatically builds `CloudFolder-windows-x64.zip` and publishes it as a GitHub Release.

## Credits

CloudFolder stands on excellent existing projects:

- [rclone](https://rclone.org/) — remote storage and VFS mount engine;
- [WinFsp](https://winfsp.dev/) — Windows userspace filesystem infrastructure;
- [windows-service](https://crates.io/crates/windows-service) — Rust Windows Service integration.

## License

MIT. See [LICENSE](LICENSE).
