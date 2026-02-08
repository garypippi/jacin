# jacin

Neovim をバックエンドとした、Linux Wayland 向けカスタム IME。

[Hyprland](https://hyprland.org/)（wlroots 系コンポジタ）向けに開発。`zwp_input_method_v2` プロトコルを直接実装しており、fcitx や ibus は不要です。

> **注意: これは趣味・学習目的のプロジェクトです。**
> キーボードグラブを使用するため、バグやクラッシュが発生した場合にキーボード入力が一切できなくなる可能性があります。SSH やシリアルコンソール等、別の入力手段を確保した上でご利用ください。

## 仕組み

```
キーボード → Hyprland → jacin → Neovim (headless)
                            ↓
                      アプリケーション (zwp_input_method_v2 経由)
```

キーボードグラブでキー入力を受け取り、組み込みの Neovim インスタンス上で [skkeleton](https://github.com/vim-skkeleton/skkeleton) を使って日本語変換を行い、結果を Wayland input method プロトコル経由でアプリケーションに送信します。

## 機能

- skkeleton による SKK 方式の日本語入力
- IME 内で Vim キーバインド: ノーマルモード、ビジュアルモード、コマンドモード、テキストオブジェクト、レジスタ
- nvim-cmp による候補選択（Ctrl+N/P で移動、Ctrl+K で確定）
- プリエディット表示・カーソル表示・候補一覧を含むポップアップウィンドウ
- 色分け Vim モード表示（INS=緑, NOR=青, VIS=紫, OP=黄, CMD=赤）
- マクロ記録状態表示（REC @reg）
- デフォルトはパススルー（IME 有効時のみキーボードをグラブ）
- コンポジタの rate/delay に基づくキーリピート
- トグルキー・確定キーの設定変更が可能
- `zwp_virtual_keyboard_v1` によるモディファイアクリア（トグルキーバインドで Alt が固着する問題を修正）
- `--clean` フラグ: ユーザ設定・プラグインなしの素の Neovim で起動

## 必要なもの

- **wlroots 系コンポジタ**（Hyprland, Sway 等）— `zwp_input_method_v2` 対応が必要
- **Neovim** — headless で組み込み子プロセスとして起動
- **skkeleton** Neovim プラグイン
- **nvim-cmp**（任意、候補選択 UI 用）
- **日本語フォント**（Noto Sans CJK, IPAGothic 等）
- **Rust** ツールチェイン（ビルド用）

## ビルド

```sh
cargo build --release
```

## 使い方

1. IME を起動（Wayland コンポジタに接続します）:
   ```sh
   ./target/release/jacin          # 通常起動
   ./target/release/jacin --clean  # 素の Neovim で起動（ユーザ設定・プラグインなし）
   ```

2. SIGUSR1 を送って IME のオン/オフを切り替え:
   ```sh
   pkill -SIGUSR1 jacin
   ```

   コンポジタの設定でキーにバインドしてください。Hyprland の場合:
   ```
   bind = ALT, grave, exec, pkill -SIGUSR1 jacin
   ```

3. テキスト入力フィールドにフォーカスすると、IME が自動的にアクティブになります。

4. IME 有効時に skkeleton で日本語を入力し、Ctrl+Enter（デフォルト）でアプリケーションにテキストを確定します。

## 設定

設定ファイル: `~/.config/jacin/config.toml`

```toml
[keybinds]
toggle = "<A-`>"    # トグルキー（Vim 記法、skkeleton に送信される）
commit = "<C-CR>"   # 確定キー（Vim 記法）

[behavior]
auto_startinsert = false  # true: IME 有効時にインサートモードで開始

[completion]
adapter = "native"  # 補完アダプタ
```

## ログ

`env_logger` を使用。`RUST_LOG` 環境変数で出力レベルを制御できます:

```sh
RUST_LOG=info ./target/release/jacin    # ライフサイクルイベント
RUST_LOG=debug ./target/release/jacin   # 詳細な動作ログ
RUST_LOG=jacin=debug ./target/release/jacin  # このクレートのみ
```

## アーキテクチャ

詳細は [DESIGN.md](DESIGN.md)（設計ドキュメント）を参照してください。

## セキュリティに関する注意

IME はその性質上、すべてのキー入力を傍受・処理するため、避けられないセキュリティリスクがあります。

- **キーボードグラブ**: IME 有効時はキーボード入力がすべて jacin を経由します。パスワード入力フィールドでも例外ではありません。IME を無効にしてから機密情報を入力してください。
- **Neovim の組み込み**: jacin はユーザの Neovim 設定（`init.lua`/`init.vim` およびプラグイン）をそのまま読み込みます（`--clean` なし）。悪意のあるプラグインや改竄された設定があった場合、入力内容の窃取・改竄が可能です。信頼できるプラグインのみをインストールしてください。jacin 専用の最小構成を使うことを推奨します（後述の「推奨: 専用 Neovim 設定」を参照）。
- **プロセス権限**: Neovim 子プロセスはユーザと同じ権限で動作するため、ファイルシステムやネットワークへのアクセスが可能です。プラグインが `jobstart()` や `system()` 等で任意のコマンドを実行できる点に注意してください。
- **プリエディットの信頼性**: 表示されるプリエディットテキストは Neovim から返されたデータに基づきます。Neovim 側の状態が改竄された場合、表示と実際に確定されるテキストが異なる可能性があります。

## 推奨: 専用 Neovim 設定

jacin は組み込み Neovim にユーザの設定をそのまま読み込みます。不要なプラグインの読み込みはセキュリティリスクになるため、`NVIM_APPNAME` で jacin 専用の最小構成を分離することを推奨します。

## ライセンス

[MIT](LICENSE)
