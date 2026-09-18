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
commit = "<C-CR>"         # Commit the buffer (all lines) to the application

[behavior]
startinsert = true        # true: start in insert mode, false: start in normal mode
recording_blink = true    # Blink the REC indicator while recording a macro

[font]
family = "Noto Sans CJK JP"   # Proportional font (candidates/messages). Default: fontconfig auto
mono_family = "JetBrains Mono" # Monospace font (keypress/mode display). Default: "monospace"
size = 16.0                    # Font size in pixels
```

All fields are optional and fall back to the defaults shown above.

### How text is shown and committed

- The popup renders Neovim's window directly from UI events (`ext_multigrid`), including your colorscheme highlights and floating windows such as the nvim-cmp menu. Neovim's UI is resized to fit the popup, so long lines wrap there. The application receives no preedit; text is only inserted on commit.
- Input can span multiple lines: `<CR>` inserts a newline, and the commit key (or turning the IME off) commits the whole buffer joined with `\n`. `<CR>`/`<BS>`/commit key are passed to the application only when the buffer is completely empty. Note that in terminals a committed newline acts like Enter.
- Whatever Neovim draws in its window (including floats) appears in the popup; externalized UI (native popup menu, command line, messages) uses jacin's own sections. The native popup menu (`ext_popupmenu`) works with skkeleton henkan and any plugin that calls `complete()`, including ddc.vim with `ddc-ui-native`; nvim-cmp's menu is shown as a floating window.

> **Note:** Since jacin sets `buftype=nofile` on its buffer, ddc.vim requires `specialBufferCompletion` enabled in your ddc config.

### Neovim configuration for jacin

jacin starts Neovim with `g:jacin = 1` set before your config is loaded, so you can adjust settings for the IME without affecting your editor. jacin does not change these options itself. Anything drawn inside the window is shown in the popup, so you may want to turn off columns and line decorations:

```lua
if vim.g.jacin then
  vim.opt.number = false
  vim.opt.relativenumber = false
  vim.opt.signcolumn = "no"
  vim.opt.foldcolumn = "0"
  vim.opt.cursorline = false
end
```

The statusline, tabline, intro screen and `~` filler lines are never shown, so they need no configuration.

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

## Security Warning

jacin grabs your keyboard via the Wayland input method protocol. While the keyboard is grabbed, **all keystrokes pass through jacin and the embedded Neovim instance** before reaching the focused application. This is inherent to how IMEs work, but be aware that any Neovim plugin loaded in the embedded instance can observe your input. Use `--clean` to run without user config/plugins if needed.

## License

MIT
