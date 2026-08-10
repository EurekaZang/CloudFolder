# CloudFolder

**[中文](README.md) | [English](README.en.md) | [日本語](README.ja.md)**

**リモート Linux ワークスペースをローカル Windows に。コーディング Agent はローカルに置いたまま、実行環境はリモートに残せます。**

CloudFolder は、SSH/SFTP 上のリモートワークスペースを、エクスプローラー、VS Code、Claude Code、Codex などのローカル Windows アプリからそのまま使える通常のパスとして提供します。ファイル自体はリモートサーバーに置いたまま、ローカル Agent が Windows 上から読み書きできるため、サーバーごとに Claude Code や Codex を再デプロイする必要はありません。

ファイルシステム層には **rclone + WinFsp** を使用し、軽量な Rust 製 Windows Service が各マウントを常時維持します。さらにネイティブの **`cf.exe`** CLI が、ローカルの現在ディレクトリに対応するリモート Linux ディレクトリへターミナルコマンドを橋渡しします。

> 現在の初心者向けインストーラー対象：**Windows 10/11 x64 + SSH/SFTP サーバー**。

## 中心ワークフロー：ローカル Agent + リモート Linux

推奨する開発フローは次のとおりです。

```powershell
cd (cf path lab)

# Agent 本体は Windows 上で動かします。
claude
# または: codex

# ファイルはローカルのマウント経由で編集し、
# Git / テスト / ビルドなどは対応するリモート cwd で実行します。
cf here
cf run -- git status
cf run -- pytest -q
cf run -- cargo test
cf sh -- "git status && pytest -q"
```

`cf run` は単なる SSH の別名ではありません。実行前に VFS の未送信書き込みがサーバーへ到達するまで待機し、現在の Windows サブディレクトリを対応する絶対 Linux パスへ正確に変換します。その場所で strict SSH host verification を使ってコマンドを実行し、リモートの終了コードをそのまま返し、最後にローカルのディレクトリキャッシュを更新します。

CloudFolder では意図的に作業を次の 2 種類へ分けます。

- **ローカル Windows パス：** エディタ / Agent によるファイルの読み取り、限定的な検索、編集、作成、名前変更、削除。
- **`cf run` 経由のリモート Linux：** Git、テスト、ビルド、コンパイラ、パッケージマネージャー、プロジェクトのインタプリタ、そして大量の小さなファイルへ触れるリポジトリ全体の処理。

冷たい SFTP マウント上の `.git` に対してローカルで `git status` を実行すると、Git が metadata/object へ大量の小さなランダムアクセスを行うため遅くなることがあります。CloudFolder はこの負荷まで NTFS と同じ遅延で動くとは扱わず、リモート実行そのものをワークスペースの第一級機能として提供します。

### Claude Code / Codex に CloudFolder の使い方を教える

CloudFolder は、両 Agent に対して小さな**条件付きユーザーレベル指示ブロック**を明示的に追加できます。

```powershell
cf agent setup
```

CloudFolder が管理するのは、次のファイル内の managed block だけです。

```text
%USERPROFILE%\.claude\CLAUDE.md
%USERPROFILE%\.codex\AGENTS.md
```

既存の指示は保持されます。CloudFolder のブロックは、CloudFolder ワークスペースでは通常のローカルファイルツールで編集しつつ、Git、ビルド、テスト、大規模なリポジトリ検索は `cf run` / `cf sh` でリモート実行するよう Agent に伝えます。この設定は **opt-in** であり、通常の CloudFolder インストールだけで Agent の設定を自動変更することはありません。

状態確認と削除はいつでも行えます。

```powershell
cf agent status
cf agent remove
```

## 3 ステップでインストール

1. 最新の **GitHub Release** を開き、`CloudFolder-windows-x64.zip` をダウンロードします。
2. ZIP を展開します。
3. **`Install CloudFolder.cmd`** をダブルクリックします。

CloudFolder はランタイム、WinFsp、rclone を自動で導入します。その後に必要なのは、通常の SSH 利用者がすでに知っている情報だけです。

- わかりやすい名前（例：`Lab Server`）
- サーバーの IP アドレスまたはホスト名
- SSH ポート（既定値は `22`）
- SSH ユーザー名
- リモートディレクトリ（空欄なら SSH ユーザーのホームディレクトリ）
- ローカルの Windows フォルダー（適切な既定値が提示されます）

サーバー側がまだ CloudFolder の公開鍵を信頼していない場合、Windows OpenSSH が最初にサーバーのフィンガープリントを表示し、その後 **1 回だけ** SSH パスワードの入力を求めます。パスワードは OpenSSH が直接読み取り、CloudFolder は取得も保存もしません。それ以降、Windows Service は公開鍵認証のみを使用します。

インストール後は、**スタートメニュー → CloudFolder → CloudFolder Manager** から、マウントの追加、オープン、再起動、診断、削除を行えます。新しく開いたターミナルでは、ネイティブの `cf` コマンドも直接利用できます。

### PowerShell からオンラインインストール

ZIP を手動でダウンロードしたくない場合：

```powershell
iwr https://raw.githubusercontent.com/EurekaZang/CloudFolder/main/install.ps1 -OutFile "$env:TEMP\install-cloudfolder.ps1"
powershell -ExecutionPolicy Bypass -File "$env:TEMP\install-cloudfolder.ps1"
```

この bootstrap は最新の GitHub Release を取得・検証し、同じセットアップフローを開始します。

## 利用イメージ

たとえば、次のリモートディレクトリ：

```text
alice@server.example.com:/home/alice/projects
```

を次の Windows フォルダーに割り当てます：

```text
C:\Users\Alice\CloudFolder\Lab Server
```

すると Windows アプリからは、次のような通常のパスとして見えます。

```text
C:\Users\Alice\CloudFolder\Lab Server\robotics\train.py
```

FTP クライアントのような専用ファイルブラウザーは不要で、切断時の再接続コマンドを覚える必要もありません。

## CloudFolder が必要な理由

`rclone mount` + WinFsp だけでも、Windows にリモートストレージをマウントできます。難しいのは、それを「いつか落ちるターミナルコマンド」ではなく、ネットワーク変化、プロセスクラッシュ、Windows 再起動に耐える常設インフラとして運用することです。

CloudFolder は、そのライフサイクルと信頼性の層を追加します。

- **マウントごとに独立した Windows Service** を使用し、1 台のサーバー障害が他のマウントへ波及しない設計
- 子プロセスの生存確認を約 1 秒ごとに実行
- ハングしたファイルシステム呼び出しが watchdog 自体を止めない、**タイムアウト可能な独立ファイルシステム・ヘルスプローブ**
- rclone クラッシュ後の自動置き換え
- 上限付き指数バックオフと jitter を使った再接続
- supervisor 自体が終了しても Windows SCM による復旧を実行
- `KILL_ON_JOB_CLOSE` を有効にした Windows **Job Object** により孤児マウントプロセスを防止
- PID を確認したうえで rclone RC を使って正常終了
- stale reparse point の自動クリーンアップ
- マウント先が空でない通常ディレクトリの場合は上書き・隠蔽を拒否
- マウントごとに独立した RC ポート、キャッシュ、ログ
- VFS キャッシュ上限と最低空き容量の保護
- SSH `known_hosts` の厳格な検証
- Windows OpenSSH が実際にネゴシエートした host-key algorithm に基づく固定
- 共有ランタイム更新時に全 CloudFolder サービスを安全に停止・復旧するアップグレード処理

## アーキテクチャ

```text
Claude Code / Codex / VS Code / エクスプローラー
                 │
                 ├──── 通常のファイル I/O ────┐
                 │                              ▼
                 │                         Windows パス
                 │                              │
                 │                            WinFsp
                 │                              │
                 │                         rclone VFS
                 │                              │
                 │                            SFTP
                 │                              │
                 │                              ▼
                 └── cf run / cf sh ── SSH ──► Linux ワークスペース

CloudFolderService.exe が各 rclone mount を監視します：
ヘルスプローブ → クラッシュ復旧 → バックオフ → ログ → 安全なクリーンアップ → SCM 復旧

cf.exe がターミナルを橋渡しします：
未送信書き込みを flush → cwd を変換 → リモート実行 → 終了コードを保持 → ローカル表示を更新
```

CloudFolder は WinFsp や rclone を置き換えるものではありません。WinFsp が Windows のユーザー空間ファイルシステム橋渡しを提供し、rclone が SFTP/VFS マウントエンジンを担当し、CloudFolder がその組み合わせを常駐・自己復旧・管理可能にします。

## CloudFolder Manager

対話型マネージャーは意図的にシンプルです。

```text
1. リモートフォルダーを追加
2. フォルダーを開く
3. マウントを再起動
4. マウントを削除
5. Doctor / トラブルシューティング
6. ログを開く
7. 終了
```

CloudFolder のマウントを削除しても、削除されるのは**ローカルのマウント設定とサービス設定だけ**です。リモートファイルは削除されません。ネットワーク障害時、VFS キャッシュに未反映の書き込みが残っている可能性があるため、ローカルキャッシュは既定で保持されます。`Uninstall -PurgeCache` を明示的に実行した場合のみ CloudFolder のキャッシュルートを削除します。

## 一般ユーザー向けの既定値

- ローカルフォルダー：`%USERPROFILE%\CloudFolder\<name>`
- 専用鍵：`%USERPROFILE%\.ssh\cloudfolder_ed25519`
- 認証：SSH 公開鍵。SSH パスワードは保存しません
- VFS キャッシュ：`full`、最大 `8 GiB`
- 最低空き容量：`5 GiB`
- 新規マウントの既定 profile：`Dev`
- 開発モードの write-back 遅延：`1s`。ただし `cf run` はリモート実行前に明示的な flush barrier を使用します
- VFS 同時アップロード数：`8`
- Windows ファイルシステム ACL：CloudFolder をインストールした Windows ユーザー SID が filesystem owner となり FullControl を持ちます。LocalSystem と Administrators も FullControl を保持します
- ヘルスプローブ：`10s` ごと、タイムアウト `5s`、3 回連続失敗でマウントを再生成
- rclone のアイドル SFTP 接続：`20s`
- Windows Service：自動起動（遅延開始）

上級ユーザーは `C:\ProgramData\CloudFolder\mounts\<name>\` 以下に生成された TOML/INI を編集し、対応する `CloudFolder.<name>` サービスを再起動できます。

## セキュリティモデル

無人運用の Windows Service は、再起動のたびに SSH 鍵のパスフレーズを対話的に入力できません。そのため CloudFolder は既定で専用の**パスフレーズなし SSH 秘密鍵**を作成し、Windows ACL でアクセスを保護します。マウントサービスを実行する LocalSystem には読み取り権限が与えられます。

マウントサービス自体は、自動起動・自己修復・SCM recovery のため LocalSystem として実行されます。一方、SYSTEM-owned の WinFsp ファイルシステムでは通常ユーザーの書き込みや Git の ownership 判定に問題が起きるため、CloudFolder はインストールしたユーザー SID に合わせた WinFsp `FileSecurity` を生成します。そのユーザーが filesystem owner と FullControl を持ち、SYSTEM と Administrators も FullControl を保持します。簡単に済ませるために Everyone へ FullControl を与えることはありません。

公開鍵は、Windows OpenSSH がサーバーのフィンガープリントを表示して確認された後にのみサーバーへ登録されます。それ以降も `known_hosts` の検証は厳格に行われます。CloudFolder は SSH パスワードを rclone 設定、TOML、ログ、環境変数、コマンドライン引数へ書き込みません。

詳細は [SECURITY.md](SECURITY.md) を参照してください。

## 現在の制限

- 初心者向けマネージャーで現在設定できるのは **SFTP** マウントです。rclone 自体は多数のバックエンドに対応していますが、安全かつ簡単な UI への公開は今後の課題です。
- CloudFolder はリアルタイムのリモートファイルシステムであり、**オフライン同期ミラーではありません**。遅延や速度はネットワークとサーバー性能に依存します。
- SFTP マウント上でのローカル Git 操作や、コールドキャッシュ状態で大量の小さなファイルを読むリポジトリ全体のスキャンは遅くなる場合があります。Git、全体検索、ビルド、テスト、パッケージマネージャーなどの高 fan-out 処理は `cf run -- git ...`、`cf run -- rg ...` などでリモート Linux 上から実行することを推奨します。
- POSIX の権限、所有者、symlink の意味を Windows ファイルシステムへ完全に対応させられない場合があります。
- 現在の rclone SFTP 投影では、Linux symlink の厳密な意味は Windows ネイティブ symlink として保持されません。
- 現在の Release は **Authenticode 署名されていません**。そのため Windows SmartScreen が「不明な発行元」を表示する場合があります。各 Release ZIP には SHA-256 チェックサムも公開されます。

## トラブルシューティング

**CloudFolder Manager → Doctor / troubleshoot** を開いてください。Doctor は以下を確認します。

- CloudFolder サービスエンジン
- rclone
- WinFsp
- Windows OpenSSH
- 設定済みの各 Windows Service
- 各ローカルマウントポイント
- 各マウントに対する新規の厳格な SFTP 接続確認

ログの保存先：

```text
C:\ProgramData\CloudFolder\logs\
```

## 開発者向け

エンドユーザーは **Rust をインストールする必要はありません**。

Windows でソースからビルドする場合：

```powershell
.\scripts\build.ps1
```

ローカルビルドスクリプトは Windows GNU Rust target と ASCII のみの Cargo target ディレクトリを使用するため、リポジトリのパスに Unicode 文字が含まれていても動作します。GitHub Actions の Release ビルドは `windows-latest` 上で標準 MSVC toolchain を使用します。

主な検証コマンド：

```powershell
.\scripts\smoke-test.ps1 -MountPoint 'C:\Users\Alice\CloudFolder\Lab Server'

# 破壊的な耐障害性テスト。管理者権限で、破棄可能なテスト用マウントだけに対して実行してください。
.\scripts\fault-test.ps1 `
  -ServiceName 'CloudFolder.lab-server' `
  -MountPoint 'C:\Users\Alice\CloudFolder\Lab Server' `
  -RemoteHost 'server.example.com' `
  -RemotePort 22 `
  -RcPort 55770
```

CI では Rust formatting、tests、Clippy、Windows PowerShell 5.1 parser チェックを実行します。`v*` タグを push すると `CloudFolder-windows-x64.zip` が自動ビルドされ、GitHub Release として公開されます。

## Credits

CloudFolder は次の優れたプロジェクトの上に構築されています。

- [rclone](https://rclone.org/) — リモートストレージおよび VFS mount エンジン
- [WinFsp](https://winfsp.dev/) — Windows ユーザー空間ファイルシステム基盤
- [windows-service](https://crates.io/crates/windows-service) — Rust による Windows Service 統合

## License

MIT。詳細は [LICENSE](LICENSE) を参照してください。
