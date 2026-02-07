# custom-ime

Neovim をバックエンドとした、Linux Wayland 向けカスタム IME。

[Hyprland](https://hyprland.org/)（wlroots 系コンポジタ）向けに開発。`zwp_input_method_v2` プロトコルを直接実装しており、fcitx や ibus は不要です。

> **注意: これは趣味・学習目的のプロジェクトです。**
> キーボードグラブを使用するため、バグやクラッシュが発生した場合にキーボード入力が一切できなくなる可能性があります。SSH やシリアルコンソール等、別の入力手段を確保した上でご利用ください。

## 仕組み

```
キーボード → Hyprland → custom-ime → Neovim (headless)
                            ↓
                      アプリケーション (zwp_input_method_v2 経由)
```

キーボードグラブでキー入力を受け取り、組み込みの Neovim インスタンス上で [skkeleton](https://github.com/vim-skkeleton/skkeleton) を使って日本語変換を行い、結果を Wayland input method プロトコル経由でアプリケーションに送信します。

## 機能

- skkeleton による SKK 方式の日本語入力
- IME 内で Vim キーバインド: ノーマルモード、ビジュアルモード、コマンドモード、テキストオブジェクト、レジスタ
- nvim-cmp による候補選択（Ctrl+N/P で移動、Ctrl+K で確定）
- プリエディット表示・カーソル表示・候補一覧を含むポップアップウィンドウ
- デフォルトはパススルー（IME 有効時のみキーボードをグラブ）
- コンポジタの rate/delay に基づくキーリピート
- トグルキー・確定キーの設定変更が可能
- `zwp_virtual_keyboard_v1` によるモディファイアクリア（トグルキーバインドで Alt が固着する問題を修正）

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
   ./target/release/custom-ime
   ```

2. SIGUSR1 を送って IME のオン/オフを切り替え:
   ```sh
   pkill -SIGUSR1 custom-ime
   ```

   コンポジタの設定でキーにバインドしてください。Hyprland の場合:
   ```
   bind = ALT, grave, exec, pkill -SIGUSR1 custom-ime
   ```

3. テキスト入力フィールドにフォーカスすると、IME が自動的にアクティブになります。

4. IME 有効時に skkeleton で日本語を入力し、Ctrl+Enter（デフォルト）でアプリケーションにテキストを確定します。

## 設定

設定ファイル: `~/.config/custom-ime/config.toml`

```toml
[keybinds]
toggle = "<A-`>"    # トグルキー（Vim 記法、skkeleton に送信される）
commit = "<C-CR>"   # 確定キー（Vim 記法）
```

## ログ

`env_logger` を使用。`RUST_LOG` 環境変数で出力レベルを制御できます:

```sh
RUST_LOG=info ./target/release/custom-ime    # ライフサイクルイベント
RUST_LOG=debug ./target/release/custom-ime   # 詳細な動作ログ
RUST_LOG=custom_ime=debug ./target/release/custom-ime  # このクレートのみ
```

## アーキテクチャ

詳細は [DESIGN.md](DESIGN.md)（設計ドキュメント）と [IDEA.md](IDEA.md)（設計思想）を参照してください。

## ライセンス

[MIT](LICENSE)
