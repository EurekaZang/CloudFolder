# Security

CloudFolder is designed to run unattended as a Windows service, so it uses SSH public-key authentication rather than storing an SSH password.

## v0.9 runtime surfaces

- **Remote Change Feed:** the helper is started over the mount's existing strict SSH identity, runs only for the mount-service lifetime, needs no root, installs no permanent remote daemon, and is terminated with the service child tree. Its inotify usage is explicitly budgeted instead of intentionally consuming the user's entire watch quota.
- **Persistent Transport Broker:** listens only on Windows loopback, authenticates each local request with a random per-mount token stored in CloudFolder's protected metadata, reuses strict SSH trust/key material, and falls back to fresh SSH rather than weakening authentication. Runtime upgrade stops only CloudFolder broker processes before replacing `cf.exe`.
- **Container runtime:** `.cloudfolder.toml` is trusted executable workspace configuration. Selecting Docker/Podman gives the workspace the same authority the SSH account already has to invoke that runtime. CloudFolder does not expose the remote Docker socket to Windows.
- **Runtime relay:** binds only to remote host loopback, is tagged with a random `CLOUDFOLDER_RELAY_ID`, and is stopped only after the matching `/proc/<pid>/environ` marker is verified. The local SSH tunnel also binds only to `127.0.0.1`.
- **LSP / DAP / debugpy:** these commands intentionally execute language servers/debug adapters inside the selected remote runtime. `cloudfolder-runtime://` documents are read-only and are served through explicit `cf source read`; they are not projected as fake writable `C:\\usr\\...` files.
- **Test discovery:** `cf test discover` and `cf test run` execute the workspace's remote pytest/plugins. Treat test configuration with the same trust level as running the project itself.

## Credential model

- In direct host/IP mode, the default setup creates a dedicated `~/.ssh/cloudfolder_ed25519` key.
- The private key is intentionally created without a passphrase because a LocalSystem service cannot interactively unlock it after reboot.
- Windows ACLs grant the current user and LocalSystem access to the key. CloudFolder does not copy the key into the repository or transmit it anywhere except through normal SSH authentication.
- If the public key is not yet authorized, the setup invokes Windows OpenSSH in the foreground. OpenSSH itself reads the one-time SSH password; CloudFolder does not capture or store it.
- Host verification uses the user's `known_hosts` file with strict checking. CloudFolder also pins rclone to the host-key algorithm negotiated by Windows OpenSSH to avoid algorithm-selection mismatches.

## SSH Config / bastion mode

- `cf add <ssh-host>` keeps Windows OpenSSH responsible for aliases, `ProxyJump`, `ProxyCommand`, user/port selection, `IdentityFile`, certificates, `known_hosts`, and `Include` processing.
- rclone SFTP reaches the same OpenSSH path through CloudFolder's local external-SSH bridge. CloudFolder does not reimplement a bastion protocol or serialize private-key material into rclone configuration.
- Foreground commands (`cf run`, routed commands, jobs, forwards) continue using the user's original OpenSSH config and credentials.
- The background mount cannot safely make Windows OpenSSH consume a user-owned private-key file as LocalSystem: OpenSSH correctly rejects that file as having permissions that are too broad for the SYSTEM identity. CloudFolder therefore resolves the target and ProxyJump hops with `ssh -G` at install time and creates a mount-private snapshot under `C:\ProgramData\CloudFolder\mounts\<slug>\ssh-service\`.
- Only the identity/certificate material and `known_hosts` entries actually resolved for the target/jump chain are copied into that snapshot. Private identity copies are owned by LocalSystem, inherit no ACLs, and grant read access to LocalSystem plus FullControl to Administrators. The service config and copied `known_hosts` are likewise restricted to the mount's service context. The user's original key/config files are not modified to make SYSTEM OpenSSH accept them.
- Removing the CloudFolder mount removes this service SSH snapshot together with the rest of the mount metadata. It never goes into the repository or VFS cache.
- Unattended startup must work in OpenSSH `BatchMode`. Password-only authentication and credentials that exist only in a transient interactive ssh-agent/session cannot be safely persisted by CloudFolder; mount creation fails and rolls back instead of storing a password or leaving a partial service.

## Workspace runtime and job data

- `.cloudfolder.toml` `environment.init` is executable remote shell configuration. Treat a workspace containing it like any other repository containing build scripts: review untrusted files before running `cf enter`, routed commands, or jobs.
- Do not commit secrets directly into `.cloudfolder.toml`. Prefer remote secret stores, shell startup files with appropriate permissions, or environment injection mechanisms already used by the remote environment.
- Persistent job metadata and stdout/stderr are stored under the remote user's `~/.cloudfolder/jobs/`. Command output may itself contain secrets, so normal remote-user filesystem protections still matter.
- `cf forward` binds the local side to `127.0.0.1` by default. It does not intentionally expose the forwarded service on LAN interfaces.

## Reporting vulnerabilities

Please open a private GitHub security advisory for this repository when possible. Do not include private keys, passwords, tokens, server addresses, or production configuration files in public issues.
