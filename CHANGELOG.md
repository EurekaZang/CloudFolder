# Changelog

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
