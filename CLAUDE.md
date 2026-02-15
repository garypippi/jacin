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
  coordinator.rs             # Neovim response handling, IME toggle, preedit/popup coordination
  config.rs                  # Config file loading (TOML), keybind defaults
  state/
    wayland.rs               # WaylandState (protocol handles, serial, virtual keyboard)
    keyboard.rs              # KeyboardState (XKB, modifiers, debouncing, repeat params)
    repeat.rs                # KeyRepeatState (key repeat timing/tracking)
    ime.rs                   # ImeState, ImeMode state machine, VimMode
    keypress.rs              # KeypressState (accumulated keys, pending type, timeout)
    animation.rs             # AnimationState (blinking indicators, transient display)
  neovim/
    mod.rs                   # NeovimHandle (public API)
    protocol.rs              # ToNeovim, FromNeovim typed messages (serde), Snapshot
    handler.rs               # Tokio-side Neovim message handling (redraw events, sub-handlers)
    event_source.rs          # Calloop event source (infrastructure)
    integration_tests.rs     # Headless nvim integration tests
    lua/
      snapshot.lua           # collect_snapshot() function
      key_handlers.lua       # ime_handle_bs(), ime_handle_commit()
      auto_commit.lua        # ime_context table, check_line_added()
      autocmds.lua           # ModeChanged, TextChangedI, CursorMovedI, CmdlineLeave
      completion_cmp.lua     # nvim-cmp completion adapter
      write_commit.lua       # :w handler for write_to_commit option
  ui/
    unified_window.rs        # Unified popup (preedit, keypress, candidates)
    layout.rs                # Popup layout calculation and sizing
    text_render.rs           # Font rendering with fontdue, SHM utilities
```

## Key Design

- **ImeMode state machine**: Disabled → Enabling → Enabled (explicit states, not boolean flags)
- **Typed Neovim protocol**: Serde-based `ToNeovim`/`FromNeovim` messages with bounded channels
- **Optimized RPC**: Insert mode uses fire-and-forget (`nvim_input` + push notification via autocmds); normal mode uses 2-RPC pull (`nvim_input` + `collect_snapshot()`)
- **nvim_ui_attach extensions**: `ext_cmdline`, `ext_popupmenu`, `ext_messages`, `mode_change` — Neovim's UI protocol drives command-line, completion, messages, and mode updates
- **Config**: TOML at `~/.config/jacin/config.toml` — commit keybind, completion adapter, font, startinsert, write_to_commit

## Known Limitations

- Multiline operations (yy, dd, cc, p, P) not yet supported (single-line preedit only)

## Architecture

See `DESIGN.md` for detailed design documentation.
