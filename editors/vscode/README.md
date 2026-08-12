# CloudFolder VS Code reference bridge

This is intentionally a thin reference extension. It does **not** install VS Code Server remotely.

- Python / clangd / rust-analyzer use `cf lsp ...`; the language server runs in the selected CloudFolder host/container runtime.
- `cloudfolder-runtime://` documents are loaded with `cf source read`, so Go to Definition can open dependency sources outside the mounted workspace.
- Python debugging starts `cf debug python`, then asks the Microsoft Python Debugger extension to attach to the local CloudFolder tunnel.
- The Testing view uses `cf test discover` / `cf test run`, so pytest discovery and execution happen in the same selected host/container runtime instead of against a local Python installation.

Development install:

```powershell
cd editors\vscode
npm install
npm run bundle
code .
```

Press `F5` to open an Extension Development Host. The machine must already have CloudFolder installed and `cf` available on `PATH`.

Release builds also publish `CloudFolder-vscode.vsix`, which can be installed directly with:

```powershell
code --install-extension .\CloudFolder-vscode.vsix
```

The extension is only UI glue; execution routing, runtime selection, path mapping, source reads, port relays, and debugger launch remain implemented in `cf.exe`.
