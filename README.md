# jacin

An input method for Wayland that lets you type into any application with Neovim.\
Turn it on and your keys go to an embedded Neovim shown in a popup at the text cursor; commit, and the text is inserted into the application. Japanese (or any other) input comes from your Neovim plugins, such as skkeleton. No Fcitx/IBus needed.

![DEMO](https://github.com/user-attachments/assets/789e383e-bc74-444e-b9ca-52169d024db4)

## Requirements

jacin works only on Wayland compositors that implement these protocols:

- `zwp_input_method_v2` (with `zwp_input_popup_surface_v2`)
- `zwp_virtual_keyboard_v1`

It is developed on Hyprland; wlroots-based compositors such as Sway should work as well. **GNOME and KDE Plasma are not supported**: GNOME takes input methods only through IBus, and KDE only through `zwp_input_method_v1`.

You also need:

- Neovim >= 0.11
- Applications running natively on Wayland with `text-input-v3` support (GTK, Qt, Firefox, Chromium, most terminals). X11 applications under Xwayland cannot use jacin.
- To build: Rust (edition 2024), `pkg-config`, and the development files for `libxkbcommon` and `fontconfig`

## Install

```sh
git clone https://github.com/garypippi/jacin
cd jacin
cargo install --path .
```

This puts `jacin` in `~/.cargo/bin`.

## Setup

Only one IME can bind `zwp_input_method_v2` at a time, so stop fcitx5, ibus, etc. first. Then start jacin with your session and bind a key to `jacin toggle`.

Hyprland (`hyprland.lua`):

```lua
hl.exec_cmd("jacin")
hl.bind("ALT + grave", hl.dsp.exec_cmd("jacin toggle"))
```

Hyprland (`hyprland.conf`):

```ini
exec-once = jacin
bind = ALT, grave, exec, jacin toggle
```

Commands:

```sh
jacin          # Run the IME with your Neovim config and plugins
jacin --clean  # Run the IME with Neovim without user config/plugins
jacin toggle   # Turn the IME on or off, and print the new state (on/off)
jacin on       # Turn the IME on
jacin off      # Turn the IME off
jacin status   # Print whether the IME is on (for status bars)
```

The commands talk to the running jacin over a socket in `$XDG_RUNTIME_DIR`, one per Wayland display. If no text field is focused when the IME is turned on, it takes effect on the next one.

## Usage

- **Toggle on**: a popup appears at the text cursor showing Neovim's window, in insert mode. Type and edit as in Neovim.
- **Commit**: `<C-CR>` inserts the whole buffer into the application and clears it. The key can be changed in the config.
- **Toggle off**: whatever is left in the buffer is committed first.
- **New lines**: `<CR>` inserts a newline in the buffer; the commit inserts all lines joined with `\n`. In terminals a committed newline acts like Enter.
- **Empty buffer**: `<CR>`, `<BS>` and the commit key go to the application, so you can submit a form or delete text without turning the IME off.
- **Paste**: use Neovim's `<C-r>+` in insert mode (needs a Neovim clipboard provider such as `wl-clipboard`).
- **Password fields**: keys go straight to the application while one is focused (see [Security](#security)).
- **`:q`** quits the embedded Neovim and turns the IME off; the next toggle starts a new one.

## Configuration

Config file: `~/.config/jacin/config.toml` (or `$XDG_CONFIG_HOME/jacin/config.toml`)

```toml
[keybinds]
commit = "<C-CR>"         # Commit the buffer (all lines) to the application

[behavior]
startinsert = true        # true: start in insert mode, false: start in normal mode
recording_blink = true    # Blink the REC indicator while recording a macro

[font]
family = "Noto Sans CJK JP"   # Proportional font (candidates/messages). Default: fontconfig auto
mono_family = "JetBrains Mono" # Monospace font (Neovim window, command line, mode). Default: "monospace"
size = 16.0                    # Font size in pixels

[appearance]
opacity = 1.0             # Opacity of the popup background, 0.0 (transparent) to 1.0
```

All fields are optional; omitted ones use the defaults noted in the comments (`keybinds`, `behavior` and `appearance` values above are the defaults).

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

The popup takes its colors from your colorscheme (see below), so you can also give jacin its own colors:

```lua
if vim.g.jacin then
  vim.cmd.colorscheme("habamax")
  vim.api.nvim_set_hl(0, "PmenuSel", { fg = "#ffffff", bg = "#3d59a1" })
end
```

### What the popup shows

- Neovim's window is drawn from its UI events (`ext_multigrid`), with your colorscheme and floating windows such as the nvim-cmp menu. Neovim's UI is resized to fit the popup, so long lines wrap there. The application receives no preedit; text is only inserted on commit.
- The command line, messages and the native popup menu are shown in a panel below the text. The native popup menu (`ext_popupmenu`) works with skkeleton henkan and any plugin that calls `complete()`, including ddc.vim with `ddc-ui-native`.
- A strip on the left edge shows the mode (plain in insert mode, colored otherwise).
- Colors follow your colorscheme: `Normal` for the background and text, `FloatBorder` (or `NonText`) for the border, `MsgArea` for the command line and messages, `Pmenu`/`PmenuSel`/`PmenuSbar`/`PmenuThumb` for candidates, `Cursor` for the block cursor and `ErrorMsg` for the recording indicator. The mode strip colors are fixed.

> **Note:** jacin sets `buftype=nofile` on its buffer, so ddc.vim needs `specialBufferCompletion` enabled in your ddc config.

## Security

jacin grabs your keyboard via the Wayland input method protocol. While the keyboard is grabbed, **all keystrokes pass through jacin and the embedded Neovim instance** before reaching the focused application. This is inherent to how IMEs work, but be aware that any Neovim plugin loaded in the embedded instance can observe your input. Use `--clean` to run without user config/plugins if needed.

Password fields are the exception: when the focused field reports itself as a password or PIN field (or marks its content as sensitive/hidden), jacin releases the keyboard grab and hides the popup, so keys go straight to the application and never reach Neovim. The IME stays on and comes back when focus leaves the field. Fields that don't report this, such as a `sudo` prompt in a terminal, can't be detected: turn the IME off for those.

## Troubleshooting

Run jacin from a terminal with debug logging:

```sh
RUST_LOG=debug jacin
```

Debug logs include the keys you type, so don't share them after typing anything private.

## License

MIT
