# jacin

Hobby IME toy project bridging Wayland and Neovim.\
No Fcitx/IBus needed.\
Requires a Wayland compositor that implements.

- `zwp_input_method_v2`
- `zwp_virtual_keyboard_v1`
- `zwp_input_popup_surface_v2`

![DEMO](https://github.com/user-attachments/assets/789e383e-bc74-444e-b9ca-52169d024db4)


## Requirements

- Neovim >= 0.10
- A Wayland compositor with `zwp_input_method_v2`, `zwp_virtual_keyboard_v1`, and `zwp_input_popup_surface_v2` support
- A compositor keybind to send `SIGUSR1` to jacin for toggling

### Hyprland example

```ini
bind = ALT, grave, exec, pkill -SIGUSR1 jacin
```

## Configuration

Config file: `~/.config/jacin/config.toml`

```toml
[keybinds]
commit = "<C-CR>"         # Commit preedit text to application

[completion]
adapter = "native"        # "native" (ext_popupmenu) or "nvim-cmp"

[behavior]
startinsert = true        # true: start in insert mode, false: start in normal mode
recording_blink = true    # Blink the REC indicator while recording a macro
write_to_commit = false   # :w commits preedit text to the application (:wq, :x also work)

[font]
family = "Noto Sans CJK JP"   # Proportional font (preedit/candidates). Default: fontconfig auto
mono_family = "JetBrains Mono" # Monospace font (keypress/mode display). Default: "monospace"
size = 16.0                    # Font size in pixels
```

All fields are optional and fall back to the defaults shown above.

### Completion adapters

- **native** (default): Uses Neovim's `ext_popupmenu` UI extension. Works with skkeleton henkan and any plugin that calls `complete()`, including ddc.vim with `ddc-ui-native`.
- **nvim-cmp**: Hooks into nvim-cmp's Lua API directly for candidate extraction (nvim-cmp uses its own floating window, not the native popup menu).

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
