# Changelog

## 0.7.0 - 2026-08-12

Local Workspace / Remote Runtime release. This release lands the planned v0.5, v0.6, and v0.7 capability milestones together.

- Added `cf enter` and a native Execution Router. Git, Python, test runners, package managers, compilers, build tools, and common Linux tooling are transparently routed to the matching remote cwd while local Windows tools remain local.
- Router shims are native hardlinks to `cf.exe`, preserving argv, quoting, Unicode, remote exit codes, the VFS flush barrier, and local view refresh. A live Formal Gate completes `git -> local edit -> test -> commit` without typing `cf run` once.
- Added workspace runtime configuration through `.cloudfolder.toml`, including shell wrappers, reusable init scripts, named profiles, `cf env`, `cf env use`, and `cf env reload`. Environment state is shared by routed commands, legacy explicit remote commands, shells, and persistent jobs.
- Added `cf job run/list/logs/logs -f/attach/stop`. Jobs are detached on the remote host using `setsid + nohup` (falling back to `nohup`) and keep durable state/logs under `~/.cloudfolder/jobs/`, so local SSH loss, CloudFolder restarts, and local-computer shutdown do not terminate the remote task.
- Added `cf forward`, `cf forward list`, and `cf forward stop` for localhost SSH tunnels without hand-written `ssh -L`, including collision-safe local port selection and PID/command-line verification before stopping a tunnel.
- Added `cf add <ssh-config-host>` with native Windows OpenSSH config reuse. rclone SFTP now supports the same host alias / ProxyJump / ProxyCommand path through CloudFolder's external-OpenSSH bridge instead of requiring users to re-enter bastion settings.
- SSH Config mounts keep foreground commands on the user's original OpenSSH config, but freeze the `ssh -G`-resolved target/ProxyJump chain into a mount-private LocalSystem snapshot for background SFTP. Only required identity/certificate/known-hosts material is copied; service private-key copies are SYSTEM-owned with inherited ACLs removed. Passwords are still never stored, and unattended startup must pass OpenSSH `BatchMode`.
- Added rollback for failed mount creation so a failed SSH-config/service startup cannot leave a half-created CloudFolder service or mount metadata.
- Added reusable live gates: `scripts/formal-gate.ps1` for Router/Environment/Jobs/Forwarding and `scripts/ssh-config-gate.ps1` for the rule: **If `ssh <host>` works, `cf add <host>` should work.**
- Added `config/workspace.toml.example`, `cf --version`, expanded Rust unit coverage, and updated Chinese/English/Japanese documentation for the new runtime abstraction.

## 0.4.0 - 2026-08-10

Developer/agent workflow release.

- Added a native Rust `cf.exe` CLI for local-first remote development; the core command path no longer depends on PowerShell text decoding.
- Added `cf run`, `cf sh`, and `cf shell` to execute in the corresponding remote Linux working directory without installing the coding agent remotely.
- `cf run` now waits for pending VFS writes, preserves the remote exit code, handles Unicode and quoting through native argv, and refreshes the local directory view afterward.
- Added native `cf list`, `cf path`, `cf here`, `cf status`, `cf flush`, and `cf refresh` commands.
- Added opt-in `cf agent setup|status|remove` integration for Claude Code `CLAUDE.md` and Codex `AGENTS.md`, preserving existing user instructions.
- Added a `Dev` mount profile with `full` VFS cache, 1-second write-back, 8 concurrent transfers, and an explicit flush barrier before remote commands.
- New mounts persist their resolved absolute remote root and SSH metadata so local subdirectories map deterministically to remote working directories.
- Added per-user WinFsp `FileSecurity`: the installing Windows SID is the filesystem owner with FullControl while LocalSystem and Administrators retain FullControl. This fixes normal-user overwrite failures and Git dubious-ownership errors without granting Everyone access.
- Added automatic v0.3-to-v0.4 mount migration, UTF-8 no-BOM metadata, and legacy UTF-8/UTF-16 metadata compatibility in `cf.exe`.
- Hardened mount removal by disabling service recovery and waiting for both SCM and registry deletion before removing metadata.
- CloudFolder installs `cf.exe` on the machine PATH for new terminals.
- Documented the tested performance boundary: targeted local file editing is supported, while Git, builds, tests, package managers, interpreters, and cold repository-wide scans should run remotely through `cf run`.
- Release packages include the Chinese, English, and Japanese READMEs.

## 0.3.0 - 2026-08-10

Initial public release.

- Native Windows Service supervisor written in Rust.
- SFTP folders mounted as normal Windows directories through rclone + WinFsp.
- Interactive beginner-friendly installer and manager.
- Dedicated SSH key creation and one-time interactive public-key authorization.
- Strict `known_hosts` verification and negotiated host-key algorithm pinning.
- Independent service, RC endpoint, cache and logs for every mount.
- Filesystem health probes with timeouts, crash recovery, exponential backoff and Job Object orphan cleanup.
- Safe stale-mount cleanup that refuses to hide/delete non-empty normal directories.
- GitHub Actions CI and automated Windows release packaging.
