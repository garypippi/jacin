# jacin

Hobby IME toy project bridging Wayland and Neovim (Hyprland/wlroots)

![DEMO](https://github.com/user-attachments/assets/3084a358-5935-4384-ac12-2ef20227b396)
![DEMO](https://github.com/user-attachments/assets/d85d7781-2e09-4d32-a9e4-74cf0858f43d)

## Requirements

- A wlroots-based Wayland compositor (Hyprland, Sway, etc.) with `zwp_input_method_v2` support
- Neovim >= 0.10
- A compositor keybind to send `SIGUSR1` to jacin for toggling

### Hyprland example

```ini
bind = ALT, grave, exec, pkill -SIGUSR1 jacin
```

## Configuration

Config file: `~/.config/jacin/config.toml`

```toml
[keybinds]
commit = "<C-CR>"    # Commit preedit text to application

[completion]
adapter = "native"   # "native" (CompleteChanged/CompleteDone) or "nvim-cmp"

[behavior]
auto_startinsert = false  # true: start in insert mode, false: start in normal mode
```

All fields are optional and fall back to the defaults shown above.

### Completion adapters

- **native** (default): Uses Vim's `CompleteChanged`/`CompleteDone` autocmds. Works with skkeleton henkan and any plugin that calls `complete()`, including ddc.vim with `ddc-ui-native`.
- **nvim-cmp**: Hooks into nvim-cmp's Lua API directly for candidate extraction.

> **Note:** Since jacin sets `buftype=nofile` on its buffer, ddc.vim requires `specialBufferCompletion` enabled in your ddc config.

## Usage

Kill any running IME (fcitx5, ibus, etc.) before starting jacin. Only one IME can bind `zwp_input_method_v2` at a time.

```sh
cargo build --release
./target/release/jacin
./target/release/jacin --clean # Start with vanilla Neovim (no user config/plugins)
```

Toggle the IME by sending `SIGUSR1`:

```sh
pkill -SIGUSR1 jacin
```
## Logging

```sh
RUST_LOG=debug ./target/release/jacin
```
## Limitations

Preedit is single-line only. Multiline operations (`yy`, `dd`, `cc`, `p`, `P`) are not supported.

## Security Warning

jacin grabs your keyboard via the Wayland input method protocol. While the keyboard is grabbed, **all keystrokes pass through jacin and the embedded Neovim instance** before reaching the focused application. This is inherent to how IMEs work, but be aware that any Neovim plugin loaded in the embedded instance can observe your input. Use `--clean` to run without user config/plugins if needed.

## License

MIT
