# CloudFolder

**[中文](README.md) | [English](README.en.md) | [日本語](README.ja.md)**

**リモートの Linux / SFTP ディレクトリを通常の Windows フォルダーとしてマウントし、自動的に接続を維持します。**

CloudFolder は、SSH/SFTP 上のリモートディレクトリを、エクスプローラー、VS Code、Python、Git ツールなどの一般的な Windows アプリからそのまま使えるローカルパスとして提供します。内部では **rclone + WinFsp** を利用し、軽量な Rust 製 Windows Service がヘルスチェック、クラッシュ復旧、安全なクリーンアップ、複数マウントの分離管理を担当します。

> 現在の初心者向けインストーラー対象：**Windows 10/11 x64 + SSH/SFTP サーバー**。

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

インストール後は、**スタートメニュー → CloudFolder → CloudFolder Manager** から、マウントの追加、オープン、再起動、診断、削除を行えます。

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
エクスプローラー / VS Code / Python / 一般的な Windows アプリ
                     │
                     ▼
                 Windows パス
                     │
                  WinFsp
                     │
                     ▼
               rclone mount
                     │
                  SFTP/SSH
                     │
                     ▼
                Linux サーバー

CloudFolderService.exe が各 rclone mount を横から監視します：
ヘルスプローブ → クラッシュ復旧 → バックオフ → ログ → 安全なクリーンアップ → SCM 復旧
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
- write-back 遅延：`5s`
- ヘルスプローブ：`10s` ごと、タイムアウト `5s`、3 回連続失敗でマウントを再生成
- rclone のアイドル SFTP 接続：`20s`
- Windows Service：自動起動（遅延開始）

上級ユーザーは `C:\ProgramData\CloudFolder\mounts\<name>\` 以下に生成された TOML/INI を編集し、対応する `CloudFolder.<name>` サービスを再起動できます。

## セキュリティモデル

無人運用の Windows Service は、再起動のたびに SSH 鍵のパスフレーズを対話的に入力できません。そのため CloudFolder は既定で専用の**パスフレーズなし SSH 秘密鍵**を作成し、Windows ACL でアクセスを保護します。マウントサービスを実行する LocalSystem には読み取り権限が与えられます。

公開鍵は、Windows OpenSSH がサーバーのフィンガープリントを表示して確認された後にのみサーバーへ登録されます。それ以降も `known_hosts` の検証は厳格に行われます。CloudFolder は SSH パスワードを rclone 設定、TOML、ログ、環境変数、コマンドライン引数へ書き込みません。

詳細は [SECURITY.md](SECURITY.md) を参照してください。

## 現在の制限

- 初心者向けマネージャーで現在設定できるのは **SFTP** マウントです。rclone 自体は多数のバックエンドに対応していますが、安全かつ簡単な UI への公開は今後の課題です。
- CloudFolder はリアルタイムのリモートファイルシステムであり、**オフライン同期ミラーではありません**。遅延や速度はネットワークとサーバー性能に依存します。
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
