# Security

CloudFolder is designed to run unattended as a Windows service, so it uses SSH public-key authentication rather than storing an SSH password.

## Credential model

- The default setup creates a dedicated `~/.ssh/cloudfolder_ed25519` key.
- The private key is intentionally created without a passphrase because a LocalSystem service cannot interactively unlock it after reboot.
- Windows ACLs grant the current user and LocalSystem access to the key. CloudFolder does not copy the key into the repository or transmit it anywhere except through normal SSH authentication.
- If the public key is not yet authorized, the setup invokes Windows OpenSSH in the foreground. OpenSSH itself reads the one-time SSH password; CloudFolder does not capture or store it.
- Host verification uses the user's `known_hosts` file with strict checking. CloudFolder also pins rclone to the host-key algorithm negotiated by Windows OpenSSH to avoid algorithm-selection mismatches.

## Reporting vulnerabilities

Please open a private GitHub security advisory for this repository when possible. Do not include private keys, passwords, tokens, server addresses, or production configuration files in public issues.
