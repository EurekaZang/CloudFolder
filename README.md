# CloudFolder

**[中文](README.md) | [English](README.en.md) | [日本語](README.ja.md)**

**把远端 Linux 工作区带到本地 Windows。Agent 留在本地，构建与执行留在云端。**

CloudFolder 可以把远端 SSH/SFTP 工作区变成资源管理器、VS Code、Claude Code、Codex 和其他本地 Windows 软件都能直接访问的普通路径。文件仍然保存在远端服务器，但本地 Agent 可以像操作本地项目一样读取和修改它们，因此不需要在每一台云端服务器上重新部署 Claude Code、Codex 或其他 Agent。

文件系统层由 **rclone + WinFsp** 提供；轻量 Rust Windows Service 负责让每个挂载长期存活；原生 **`cf.exe`** CLI 则把终端命令自动桥接回与当前本地目录对应的远端 Linux 目录。

> 当前面向普通用户的一键安装目标：**Windows 10/11 x64 + SSH/SFTP 服务器**。

## 核心工作流：本地 Agent + 远端 Linux

推荐的开发方式是：

```powershell
cd (cf path lab)

# Agent 本体运行在你的 Windows 上。
claude
# 或：codex

# 文件通过本地挂载直接编辑；Git / 测试 / 构建等命令在远端对应目录执行。
cf here
cf run -- git status
cf run -- pytest -q
cf run -- cargo test
cf sh -- "git status && pytest -q"
```

`cf run` 不只是给 SSH 套了一层别名。执行前，它会等待 VFS 中尚未提交的本地写入真正到达服务器；随后把当前 Windows 子目录精确映射成远端绝对 Linux 目录，在严格 SSH host verification 下执行命令，原样返回远端退出码，最后刷新本地目录缓存，让远端生成的新文件尽快出现在本地挂载中。

因此 CloudFolder 故意把开发工作分成两类：

- **本地 Windows 路径：** 编辑器/Agent 的文件读取、定点搜索、修改、新建、重命名和删除；
- **远端 Linux（通过 `cf run`）：** Git、测试、构建、编译器、包管理器、项目解释器，以及会触碰大量小文件的全仓命令。

直接在冷的 SFTP 挂载上执行本地 `git status` 并不理想，因为 Git 会对 `.git` 中的 metadata/object 做大量细碎随机访问。CloudFolder 不会假装这种负载也能获得本地 NTFS 一样的延迟，而是把“远端执行”本身做成工作区的一等能力。

### 自动教会 Claude Code / Codex 使用 CloudFolder

CloudFolder 可以为两个 Agent 安装一小段**有条件生效**的用户级指令：

```powershell
cf agent setup
```

它只维护下面两个文件中的 CloudFolder managed block：

```text
%USERPROFILE%\.claude\CLAUDE.md
%USERPROFILE%\.codex\AGENTS.md
```

你原本的指令会被保留。CloudFolder 指令会告诉 Agent：在 CloudFolder 工作区里正常使用本地文件工具进行编辑，但 Git、构建、测试和大范围仓库扫描优先通过 `cf run` / `cf sh` 在远端执行。这个步骤是**显式 opt-in**；普通 CloudFolder 安装不会自动修改你的 Agent 配置。

可以随时检查或移除：

```powershell
cf agent status
cf agent remove
```

## 三步安装

1. 打开最新的 **GitHub Release**，下载 `CloudFolder-windows-x64.zip`。
2. 解压。
3. 双击 **`Install CloudFolder.cmd`**。

CloudFolder 会自动安装运行环境、WinFsp 和 rclone。之后只需要填写普通 SSH 用户已经知道的信息：

- 一个便于识别的名称，例如 `Lab Server`；
- 服务器 IP 或域名；
- SSH 端口，默认 `22`；
- SSH 用户名；
- 远端目录，留空表示该 SSH 用户的 home 目录；
- 本地 Windows 目录，安装器会提供合理的默认路径。

如果服务器还没有信任 CloudFolder 的公钥，Windows OpenSSH 会先显示服务器指纹，然后只要求输入 **一次** SSH 密码。密码由 OpenSSH 直接读取，CloudFolder 不会捕获或保存。之后 Windows 服务只使用公钥认证。

安装完成后，可以从 **开始菜单 → CloudFolder → CloudFolder Manager** 添加、打开、重启、诊断或移除挂载。新打开的终端还会直接获得原生 `cf` 命令。

### PowerShell 在线安装

如果不想手动下载 ZIP：

```powershell
iwr https://raw.githubusercontent.com/EurekaZang/CloudFolder/main/install.ps1 -OutFile "$env:TEMP\install-cloudfolder.ps1"
powershell -ExecutionPolicy Bypass -File "$env:TEMP\install-cloudfolder.ps1"
```

该 bootstrap 会下载最新 GitHub Release、校验文件并进入相同的安装流程。

## 使用效果

例如，把：

```text
alice@server.example.com:/home/alice/projects
```

映射为：

```text
C:\Users\Alice\CloudFolder\Lab Server
```

那么 Windows 软件看到的就是普通路径：

```text
C:\Users\Alice\CloudFolder\Lab Server\robotics\train.py
```

不需要单独的 FTP 风格文件浏览器，也不需要记住断线后该执行什么重连命令。

## 为什么需要 CloudFolder

`rclone mount` + WinFsp 本来就可以把远端存储挂载到 Windows。真正麻烦的是，如何让它像系统基础设施一样长期可靠运行，而不是一个迟早会因为网络变化、进程异常或系统重启而失效的终端命令。

CloudFolder 增加的是生命周期与可靠性层：

- **每个挂载独立一个 Windows Service**，单个服务器异常不会拖垮其他挂载；
- 约每秒检查子进程存活状态；
- 独立的、可超时终止的**文件系统健康探针**，避免文件系统调用卡死后连 watchdog 自己也被挂住；
- rclone 崩溃后自动替换；
- 有上限的指数退避与随机抖动重连；
- supervisor 自身被杀后，由 Windows SCM 继续执行恢复；
- 使用带 `KILL_ON_JOB_CLOSE` 的 Windows **Job Object**，避免遗留孤儿挂载进程；
- 通过 rclone RC 优雅退出，并先验证 PID；
- 自动清理失效的 reparse point；
- 如果挂载路径是非空普通目录，拒绝覆盖或隐藏它；
- 每个挂载拥有独立 RC 端口、缓存和日志；
- VFS 缓存容量限制与最小剩余磁盘空间保护；
- 严格执行 SSH `known_hosts` 校验；
- 根据 Windows OpenSSH 实际协商结果固定 host-key algorithm；
- 更新共享运行时前后安全停止并恢复所有 CloudFolder 挂载服务。

## 架构

```text
Claude Code / Codex / VS Code / 资源管理器
                 │
                 ├──── 普通文件 I/O ────┐
                 │                       ▼
                 │                  Windows 路径
                 │                       │
                 │                     WinFsp
                 │                       │
                 │                  rclone VFS
                 │                       │
                 │                     SFTP
                 │                       │
                 │                       ▼
                 └── cf run / cf sh ── SSH ──► Linux 工作区

CloudFolderService.exe 负责监督每个 rclone mount：
健康探针 → 崩溃恢复 → 退避重连 → 日志 → 安全清理 → SCM 恢复

cf.exe 负责终端桥接：
等待写入提交 → 映射 cwd → 远端执行 → 保留退出码 → 刷新本地视图
```

CloudFolder **不是** WinFsp 或 rclone 的替代品。WinFsp 提供 Windows 用户态文件系统桥接能力，rclone 提供 SFTP/VFS 挂载引擎，CloudFolder 负责让这套组合长期运行、自愈并便于管理。

## CloudFolder Manager

交互管理器刻意保持简单：

```text
1. 添加远端文件夹
2. 打开文件夹
3. 重启挂载
4. 移除挂载
5. Doctor / 故障诊断
6. 打开日志
7. 退出
```

移除 CloudFolder 挂载只会删除**本地挂载与服务配置**，不会删除远端文件。VFS 缓存默认保留，因为网络故障后它可能仍包含尚未提交到服务器的写入。只有显式执行 `Uninstall -PurgeCache` 才会清理 CloudFolder 缓存根目录。

## 面向普通用户的默认配置

- 本地目录：`%USERPROFILE%\CloudFolder\<name>`
- 专用密钥：`%USERPROFILE%\.ssh\cloudfolder_ed25519`
- 认证方式：SSH 公钥；永不保存 SSH 密码
- VFS 缓存：`full`，最大 `8 GiB`
- 最小剩余空间：`5 GiB`
- 新建挂载默认 profile：`Dev`
- 开发模式写回延迟：`1s`；`cf run` 在远端执行前仍会使用显式 flush barrier
- VFS 并发上传：`8`
- Windows 文件系统 ACL：安装 CloudFolder 的 Windows 用户 SID 是文件系统 owner，并拥有 FullControl；LocalSystem 与 Administrators 同样保留 FullControl
- 健康检查：每 `10s` 一次，超时 `5s`，连续失败 3 次后重建挂载
- rclone SFTP 空闲连接：`20s`
- Windows 服务：自动启动（延迟启动）

高级用户可以编辑 `C:\ProgramData\CloudFolder\mounts\<name>\` 下生成的 TOML/INI 配置，然后重启对应的 `CloudFolder.<name>` 服务。

## 安全模型

无人值守的 Windows 服务无法在每次重启后交互式输入 SSH 密钥口令。因此 CloudFolder 默认创建一个专用的**无 passphrase SSH 私钥**，并依靠 Windows ACL 保护访问权限。运行挂载服务的 LocalSystem 会获得读取权限。

挂载服务本身仍以 LocalSystem 运行，以保证自启动、自愈和 SCM recovery。为了避免 SYSTEM-owned WinFsp 文件系统导致普通用户写入受限以及 Git ownership 判断异常，CloudFolder 会生成精确到安装用户 SID 的 WinFsp `FileSecurity`：安装用户成为文件系统 owner 并拥有 FullControl，同时 SYSTEM 和 Administrators 保留 FullControl。CloudFolder **不会**为了省事给 Everyone 开 FullControl。

只有在 Windows OpenSSH 展示并确认服务器指纹后，CloudFolder 才会安装公钥。之后所有连接继续严格校验 `known_hosts`。CloudFolder 不会把 SSH 密码写入 rclone 配置、TOML、日志、环境变量或命令行参数。

详细说明见 [SECURITY.md](SECURITY.md)。

## 当前限制

- 当前面向小白的管理器只配置 **SFTP** 挂载。rclone 支持更多后端，但还没有全部暴露到简单 UI 中。
- CloudFolder 是实时远程文件系统，**不是离线同步镜像**。延迟和吞吐仍受网络与服务器性能影响。
- 直接在 SFTP 挂载上执行本地 Git，以及冷缓存下的大范围全仓扫描，可能因为大量小文件/metadata 往返而很慢。建议通过 `cf run -- git ...`、`cf run -- rg ...` 在远端 Linux 执行 Git、全仓搜索、构建、测试、包管理器等高 fan-out 负载。
- POSIX 权限、ownership 与 symlink 语义无法总是完美映射到 Windows 文件系统语义。
- 当前 rclone SFTP 投影不会把 Linux symlink 完整保留为原生 Windows symlink。
- Release 目前**没有 Authenticode 代码签名**，因此 Windows SmartScreen 可能显示“未知发布者”。每个 Release 同时发布 ZIP 的 SHA-256 校验文件。

## 故障诊断

打开 **CloudFolder Manager → Doctor / troubleshoot**。Doctor 会检查：

- CloudFolder 服务引擎；
- rclone；
- WinFsp；
- Windows OpenSSH；
- 所有已配置的 Windows Service；
- 所有本地挂载点；
- 每个挂载的全新、严格 SFTP 连通性。

日志位于：

```text
C:\ProgramData\CloudFolder\logs\
```

## 开发者

普通用户**不需要安装 Rust**。

Windows 源码构建：

```powershell
.\scripts\build.ps1
```

本地构建脚本使用 Windows GNU Rust target 和纯 ASCII Cargo target 目录，因此仓库路径包含 Unicode 字符时也能工作。GitHub Actions 则在 `windows-latest` 上使用标准 MSVC toolchain 构建 Release。

常用验证命令：

```powershell
.\scripts\smoke-test.ps1 -MountPoint 'C:\Users\Alice\CloudFolder\Lab Server'

# 破坏性可靠性测试。请使用管理员权限，并且只针对可丢弃的测试挂载。
.\scripts\fault-test.ps1 `
  -ServiceName 'CloudFolder.lab-server' `
  -MountPoint 'C:\Users\Alice\CloudFolder\Lab Server' `
  -RemoteHost 'server.example.com' `
  -RemotePort 22 `
  -RcPort 55770
```

CI 会执行 Rust format、tests、Clippy 和 Windows PowerShell 5.1 parser 检查。推送 `v*` tag 后，会自动构建 `CloudFolder-windows-x64.zip` 并发布为 GitHub Release。

## 致谢

CloudFolder 建立在这些优秀项目之上：

- [rclone](https://rclone.org/) — 远程存储与 VFS mount 引擎；
- [WinFsp](https://winfsp.dev/) — Windows 用户态文件系统基础设施；
- [windows-service](https://crates.io/crates/windows-service) — Rust Windows Service 集成。

## License

MIT，见 [LICENSE](LICENSE)。
