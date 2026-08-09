# Changelog

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
