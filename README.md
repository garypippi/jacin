# jacin

> [!NOTE]
> jacin is still under development. Configuration and behavior may change between versions.

An input method for Wayland that lets you type into any application using Neovim.

Turn it on, and your keystrokes are sent to an embedded Neovim instance displayed in a popup at the text cursor. Commit the buffer, and its contents are inserted into the application.

Japanese input — or any other input method you can implement in Neovim — is provided by your Neovim plugins, such as an SKK plugin. No Fcitx or IBus is required.

https://github.com/user-attachments/assets/27987426-6e1c-42c6-bb97-7ab894fdd457

## Requirements

jacin requires a Wayland compositor that implements:

- `zwp_input_method_v2` (including `zwp_input_popup_surface_v2`)
- `zwp_virtual_keyboard_v1`

It is primarily developed and tested on Hyprland. wlroots-based compositors such as Sway may work as well.

**GNOME and KDE Plasma are currently unsupported.** GNOME's input-method integration is based on IBus, while KDE Plasma does not currently provide the Wayland protocols jacin requires.

You also need:

- Neovim >= 0.11
- Applications running natively on Wayland with `text-input-v3` support, such as GTK/Qt applications, Firefox, Chromium, and most terminals
- Rust (edition 2024), `pkg-config`, and the development files for `libxkbcommon` and `fontconfig` to build jacin

X11 applications running through Xwayland cannot use jacin.

## Installation

There are currently no releases or packages. Build jacin from source:

```sh
cargo build --release
```

Then place `target/release/jacin` somewhere in your `PATH`.

## Setup

Only one input method can bind `zwp_input_method_v2` at a time, so stop Fcitx5, IBus, or any other input-method daemon first. Then start jacin with your Wayland session and bind a key to `jacin toggle`.

### Hyprland (`hyprland.lua`)

```lua
hl.exec_cmd("jacin")
hl.bind("ALT + grave", hl.dsp.exec_cmd("jacin toggle"))
```

### Hyprland (`hyprland.conf`)

```ini
exec-once = jacin
bind = ALT, grave, exec, jacin toggle
```

## Commands

```sh
jacin          # Run the IME with your Neovim config and plugins
jacin --clean  # Run the IME with Neovim without user config/plugins
jacin toggle   # Toggle the IME and print the new state (on/off)
jacin on       # Turn the IME on
jacin off      # Turn the IME off
jacin status   # Print whether the IME is on (useful for status bars)
jacin focus    # Toggle focus mode and print the new mode (focus/popup)
```

These commands communicate with the running jacin process through a socket in `$XDG_RUNTIME_DIR`. A separate socket is used for each Wayland display.

If no text field is focused when the IME is enabled, it becomes active the next time a compatible text field receives focus.

## Usage

- **Toggle on:** A popup appears at the text cursor and displays the Neovim window in insert mode. Type and edit normally using Neovim.
- **Commit:** `<C-CR>` inserts the entire main buffer into the application and then clears it. The commit key can be changed in the configuration.
- **Toggle off:** Any text remaining in the buffer is committed before the IME is disabled.
- **New lines:** `<CR>` inserts a newline into the Neovim buffer. When committed, the buffer is inserted with its newline characters preserved. In terminals, a committed newline behaves like pressing Enter.
- **Empty buffer:** When the buffer is empty, `<CR>`, `<BS>`, and the commit key are forwarded directly to the application. This lets you submit forms or delete text without disabling the IME.
- **Leaving a text field:** Moving focus to another field, window, or workspace clears the buffer without committing it. If `stash_register = "j"` is configured, the discarded text is saved to register `j`. You can restore it with `<C-r>j` in insert mode or `"jp` in normal mode.
- **Paste:** Use Neovim's `<C-r>+` in insert mode. This requires a Neovim clipboard provider such as `wl-clipboard`.
- **Password fields:** If the focused field identifies itself as a password or PIN field, keystrokes are sent directly to the application and never reach Neovim. The IME becomes active again after focus leaves the field. Some password prompts do not expose this information. A `sudo` prompt inside a terminal, for example, cannot be detected reliably. Turn the IME off before entering sensitive text in such cases.
- **`:q`:** Quitting the embedded Neovim instance disables the IME. The next toggle starts a new Neovim instance.
- **One window:** Only the window with the cursor is displayed, and only the main buffer is committed. Splits, tab pages, and other buffers are closed or hidden again after each commit and when the IME is disabled.
- **Focus mode:** `jacin focus` switches to a larger popup showing the full Neovim screen, including splits, tab pages, the statusline, and the tabline. This makes it possible to do things such as opening a note beside the input buffer with `:vsplit memo.md`. Neovim's native popup menu appears next to the completed word rather than in the panel. Committing still inserts only the main buffer, and the layout is reset afterward as described above. Focus mode remains enabled across IME toggles until `jacin focus` is run again. It requires an explicit size in the `[focus]` configuration section.

## Configuration

Configuration file: `$XDG_CONFIG_HOME/jacin/config.toml` (`~/.config/jacin/config.toml` if `XDG_CONFIG_HOME` is not set)

Example:

```toml
[keybinds]
commit = "<C-CR>"          # Commit the entire buffer to the application (Vim key notation, e.g. "<A-;>")

[behavior]
startinsert = true         # true: start in insert mode; false: start in normal mode
recording_blink = true     # Blink the REC indicator while recording a macro
stash_register = "j"       # Save text lost on focus change to this register (a letter). Default: none

[font]
family = "sans-serif"      # Font for candidates and messages (a fontconfig name). Default: fontconfig's default
mono_family = "monospace"  # Font for the Neovim window and command line. Default: "monospace"
size = 16.0                # Font size in pixels

[appearance]
opacity = 1.0              # Popup background opacity: 0.0 (transparent) to 1.0

[focus]
columns = 100              # Neovim columns in focus mode
lines = 30                 # Neovim lines in focus mode
```

All fields are optional. The values shown above for `[keybinds]`, `[behavior]`, and `[appearance]` are the defaults, except for `stash_register`, which is disabled by default.

The `[focus]` values are examples rather than defaults. Focus mode is available only when both `columns` and `lines` are configured, because an appropriate size depends on your monitor and font.

### Neovim configuration for jacin

jacin sets `g:jacin = 1` before loading your Neovim configuration. You can use it to customize Neovim specifically for jacin without affecting your normal editor sessions.

Anything rendered inside the Neovim window is shown in the popup, so you may want to disable columns and other decorations (jacin does not change these options itself):

```lua
if vim.g.jacin then
  vim.opt.number = false
  vim.opt.relativenumber = false
  vim.opt.signcolumn = "no"
  vim.opt.foldcolumn = "0"
  vim.opt.cursorline = false
end
```

In normal popup mode, the statusline, tabline, intro screen, and `~` filler lines are never shown, so they need no configuration. Focus mode shows the whole screen as Neovim draws it.

The popup uses colors from your Neovim colorscheme, so you can also give jacin its own colorscheme or highlights:

```lua
if vim.g.jacin then
  vim.cmd.colorscheme("habamax")
  vim.api.nvim_set_hl(0, "PmenuSel", { fg = "#ffffff", bg = "#3d59a1" })
end
```

### What the popup shows

- The Neovim window is rendered from Neovim UI events using `ext_multigrid`, including your colorscheme and floating windows such as completion menus created by plugins. The Neovim UI is resized to fit the popup, so long lines may wrap differently from your normal editor.
- jacin does not send preedit text to the application. Text is inserted only when the buffer is committed.
- The command line, messages, and Neovim's native popup menu are displayed in a panel below the main window. The native popup menu (`ext_popupmenu`) is used by plugins that call `complete()`.
- A strip along the left edge displays the current mode. It is plain in insert mode and colored in other modes.
- Most colors are taken from Neovim highlight groups (the mode-strip colors are fixed):
  - `Normal` for the main background and text
  - `FloatBorder` or `NonText` for borders
  - `MsgArea` for the command line and messages
  - `Pmenu`, `PmenuSel`, `PmenuSbar`, and `PmenuThumb` for completion candidates
  - `Cursor` for the block cursor
  - `ErrorMsg` for the macro-recording indicator

> [!NOTE]
> jacin's main buffer uses `buftype=nofile`. Some completion plugins ignore `nofile` buffers by default. If completion does not appear, check your plugin's configuration.

## Troubleshooting

Run jacin from a terminal with debug logging enabled:

```sh
RUST_LOG=debug jacin
```

**Warning:** Debug logs may contain the keys you type. Do not share them after entering passwords, private messages, or other sensitive information.

## License

MIT
