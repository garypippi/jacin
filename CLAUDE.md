# Custom IME for Wayland

A custom Input Method Editor for Linux Wayland (Hyprland/wlroots), with Neovim as the backend.

## Tech Stack

- **Language:** Rust
- **Wayland:** wayland-client, smithay-client-toolkit
- **Protocols:** zwp_input_method_v2, zwp_input_popup_surface_v2, zwp_virtual_keyboard_v1
- **Backend:** Neovim (headless) via nvim-rs

## Commands

```sh
cargo build          # Build
cargo run            # Run (requires Hyprland)
cargo run -- --clean # Run with vanilla Neovim (no user config/plugins)
cargo clippy         # Lint
cargo fmt            # Format
cargo test           # Unit + integration tests
```

## Module Structure

```
src/
  main.rs                    # Entry point, State struct, event loop setup
  dispatch.rs                # Wayland Dispatch impls, memmap_keymap
  input.rs                   # Key processing, handle_key, send_to_nvim
  keysym.rs                  # keysym_to_vim (pure conversion function)
  model.rs                   # Model (ImeState + KeypressState + NvimView), reduce(FromNeovim) -> Vec<Effect>
  coordinator.rs             # apply_effects, IME toggle, popup coordination
  config.rs                  # Config file loading (TOML), keybind defaults
  state/
    wayland.rs               # WaylandState (protocol handles, serial, virtual keyboard)
    keyboard.rs              # KeyboardState (XKB, modifiers, debouncing, repeat params)
    repeat.rs                # KeyRepeatState (key repeat timing/tracking)
    ime.rs                   # ImeState, ImeMode state machine
    nvim_view.rs             # NvimView (observed vim mode, recording, cmdline, candidates, messages, screen, buffer)
    buffer.rs                # BufferMirror (buffer lines from nvim_buf_attach, committed on IME off)
    screen.rs                # Screen (ext_multigrid: grids by id, windows, viewport, floats, cursor, hl)
    grid.rs                  # Grid (cell storage for one grid)
    keypress.rs              # KeypressState (accumulated keys, pending type, timeout)
    animation.rs             # AnimationState (blinking indicators, transient display)
  neovim/
    mod.rs                   # NeovimHandle (public API)
    protocol.rs              # ToNeovim, FromNeovim typed messages (serde)
    handler.rs               # Tokio-side Neovim message handling (redraw events, sub-handlers)
    redraw_grid.rs           # ext_linegrid/ext_multigrid redraw events → GridEvent (pure parsing)
    integration_tests.rs     # Headless nvim integration tests
  ui/
    unified_window.rs        # Unified popup (window grid, keypress, candidates)
    layout.rs                # Popup layout calculation and sizing
    text_render.rs           # Font rendering with fontdue, SHM utilities
```

## Key Design

- **ImeMode state machine**: Disabled → Enabling → Enabled (explicit states, not boolean flags)
- **Typed Neovim protocol**: Serde-based `ToNeovim`/`FromNeovim` messages with bounded channels
- **No Lua / autocmds**: jacin only calls the Neovim API (`nvim_input`, `nvim_get_mode`, `nvim_buf_get_lines`, `reg_recording()`); state comes from UI events and buffer events
- **Optimized RPC**: Insert mode uses fire-and-forget (`nvim_input`; display follows redraw/buffer events); normal mode uses 2-RPC pull (`nvim_input` + `nvim_get_mode`)
- **nvim_ui_attach extensions**: `ext_cmdline`, `ext_popupmenu`, `ext_messages`, `ext_multigrid`, `mode_change` — Neovim's UI protocol drives command-line, completion, messages, mode, and the window display
- **Display**: popup renders the window grid from ext_multigrid events (`Screen`/`WindowView`); the app gets no preedit; Neovim is started with `g:jacin = 1` for user config
- **Multiline**: `<CR>` is a native newline; commit key joins all lines with `\n`; IME off commits the `nvim_buf_attach` mirror (`BufferMirror`); `<CR>`/`<BS>`/commit pass through only when the whole buffer is empty
- **Config**: TOML at `~/.config/jacin/config.toml` — commit keybind, font, startinsert, recording_blink (unknown keys are ignored)

## Architecture

See `DESIGN.md` for detailed design documentation.
