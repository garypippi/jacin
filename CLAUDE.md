# Custom IME for Wayland

A custom Input Method Editor for Linux Wayland (Hyprland/wlroots), with Neovim as the backend.

## Tech Stack

- **Language:** Rust
- **Wayland:** wayland-client, smithay-client-toolkit
- **Protocols:** zwp_input_method_v2, zwp_input_popup_surface_v2, zwp_virtual_keyboard_v1
- **Backend:** Neovim (headless), msgpack-rpc over its stdin/stdout (own codec on `rmpv`)

## Commands

```sh
cargo build          # Build
cargo run            # Run (requires Hyprland)
cargo run -- --clean # Run with vanilla Neovim (no user config/plugins)
cargo clippy         # Lint
cargo fmt            # Format
cargo test           # Unit tests
cargo test -- --ignored # Integration tests (spawn headless nvim)
```

## Module Structure

```
src/
  main.rs                    # Entry point, State struct, event loop setup
  dispatch.rs                # Wayland Dispatch impls, memmap_keymap
  input.rs                   # Key processing, handle_key, send_to_nvim
  keysym.rs                  # keysym_to_vim (pure conversion function)
  model.rs                   # Model (ImeState + NvimView + SentKeys), reduce(FromNeovim) -> Vec<Effect>
  coordinator.rs             # apply_effects, IME toggle, popup coordination
  config.rs                  # Config file loading (TOML), keybind defaults
  control.rs                 # Control socket: jacin toggle/on/off/status
  state/
    wayland.rs               # WaylandState (protocol handles, serial, virtual keyboard)
    keyboard.rs              # KeyboardState (XKB, modifiers, debouncing, repeat params)
    repeat.rs                # KeyRepeatState (key repeat timing/tracking)
    ime.rs                   # ImeState, ImeMode state machine
    sent_keys.rs             # SentKeys (keys awaiting KeyProcessed, for display and passthrough)
    nvim_view.rs             # NvimView (vim mode, recording, cmdline, showcmd, candidates, messages, screen, buffer)
    buffer.rs                # BufferMirror (buffer lines from nvim_buf_attach, committed on IME off)
    screen.rs                # Screen (ext_multigrid: grids by id, windows, viewport, floats, cursor, hl)
    grid.rs                  # Grid (cell storage for one grid)
    animation.rs             # AnimationState (blinking indicators, transient display)
  neovim/
    mod.rs                   # Neovim (public API): spawn, read/next_message, push_key, input, requests
    process.rs               # NvimProcess (nvim --embed, non-blocking pipes, write queue)
    rpc.rs                   # msgpack-rpc codec (no I/O)
    keys.rs                  # KeyProcessor: key queue state machine (no I/O)
    mode.rs                  # Mode, classified from mode_change and nvim_get_mode
    protocol.rs              # FromNeovim typed messages (serde), PendingState
    redraw.rs                # redraw notifications → FromNeovim (pure parsing)
    redraw_grid.rs           # ext_linegrid/ext_multigrid redraw events → GridEvent (pure parsing)
    integration_tests.rs     # Headless nvim integration tests
  ui/
    unified_window.rs        # Unified popup (mode + window grid; panel when needed: cmdline/showcmd, message, candidates)
    layout.rs                # Popup layout calculation and sizing
    theme.rs                 # Popup colors from Normal and builtin highlight groups (hl_group_set)
    text_render.rs           # Font rendering with fontdue, SHM utilities
examples/
  grid_probe.rs              # Print ext_multigrid redraw events for given keys
tests/fixtures/              # FromNeovim message sequences replayed through Model::reduce
```

## Key Design

- **ImeMode state machine**: Disabled → Enabling → Enabled (explicit states, not boolean flags)
- **Single thread**: Neovim's stdout is a calloop source; messages are handled one at a time, each one's events reduced before the next; nothing waits for Neovim
- **Typed Neovim protocol**: Serde-based `FromNeovim` messages into `Model::reduce`
- **No Lua / autocmds**: jacin only calls the Neovim API (`nvim_input`, `nvim_get_mode`, `nvim_buf_get_lines`, `reg_recording()`; resets with `:fclose!`, `nvim_buf_set_lines`, `:startinsert`, and `setreg()` to the configured stash register for text thrown away by a focus change); state comes from UI events and buffer events
- **Key queue**: `KeyProcessor` handles one key at a time; insert mode is fire-and-forget (`nvim_input`; display follows redraw/buffer events); other modes query `nvim_get_mode` after the key's `nvim_input` is answered; non-fast requests only after `nvim_get_mode` shows Neovim isn't blocked in getchar
- **One mode**: `NvimView.mode` (`Mode` enum) from `mode_change` and `nvim_get_mode`, last arrival wins
- **No plugin-specific handling**: decisions use only generic Vim state (mode, blocking)
- **nvim_ui_attach extensions**: `ext_cmdline`, `ext_popupmenu`, `ext_messages`, `ext_multigrid`, `mode_change` — Neovim's UI protocol drives command-line, completion, messages, mode, and the window display
- **Display**: popup renders the window grid from ext_multigrid events (`Screen`/`WindowView`); the app gets no preedit; Neovim is started with `g:jacin = 1` for user config
- **Multiline**: `<CR>` is a native newline; commit key joins all lines with `\n`; IME off commits the `nvim_buf_attach` mirror (`BufferMirror`); `<CR>`/`<BS>`/commit pass through only when the whole buffer is empty
- **Config**: TOML at `~/.config/jacin/config.toml` — commit keybind, font, startinsert, recording_blink, stash_register, popup opacity, focus mode size (required for focus mode) (unknown keys are ignored)
- **Colors**: the popup follows the colorscheme (`default_colors_set`, `hl_group_set`); built-in One Dark-like colors only where Neovim sets none

## Architecture

See `DESIGN.md` for detailed design documentation.
