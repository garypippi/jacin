# Custom IME for Wayland

A custom Input Method Editor for Linux Wayland (Hyprland/wlroots), with Neovim as the backend.

## Tech Stack

- **Language:** Rust
- **Wayland:** wayland-client, smithay-client-toolkit
- **Protocols:** zwp_input_method_v2, zwp_input_popup_surface_v2
- **Backend:** Neovim (headless) via nvim-rs + skkeleton + nvim-cmp

## Commands

```sh
cargo build          # Build
cargo run            # Run (requires Hyprland)
cargo clippy         # Lint
cargo fmt            # Format
```

## Module Structure

```
src/
  main.rs                    # Entry point, Wayland dispatch, coordination
  state/
    mod.rs                   # Re-exports
    wayland.rs               # WaylandState (protocol handles, serial)
    keyboard.rs              # KeyboardState (XKB, modifiers, debouncing)
    ime.rs                   # ImeState, ImeMode state machine, VimMode
  neovim/
    mod.rs                   # NeovimHandle (public API)
    protocol.rs              # ToNeovim, FromNeovim typed messages (serde)
    handler.rs               # Tokio-side Neovim message handling
    event_source.rs          # Calloop event source (infrastructure)
  ui/
    mod.rs                   # Re-exports
    candidate_window.rs      # Candidate popup UI (input_popup_surface)
    text_render.rs           # Font rendering with fontdue
```

## Key Components

- **State modules**: Separate concerns into `WaylandState`, `KeyboardState`, `ImeState`
- **ImeMode state machine**: Explicit states (Disabled, Enabling, Enabled, Disabling) replacing boolean flags
- **Typed Neovim protocol**: Serde-based `ToNeovim`/`FromNeovim` messages with bounded channels
- **UI module**: Candidate window and text rendering

## Current State

Working:
- Basic Japanese input via skkeleton
- Alt+` to toggle IME (via SIGUSR1 signal, triggered by Hyprland keybind)
- Passthrough mode by default (keyboard only grabbed when IME enabled)
- Candidate window follows cursor (via zwp_input_popup_surface_v2)
- nvim-cmp integration for candidate selection (Ctrl+N/P, Ctrl+K to confirm)
- Enter confirms skkeleton conversion (stays in preedit when ▽/▼ markers present)
- Ctrl+Enter commits preedit text to application
- Escape switches to normal mode in neovim
- Cursor position display: line cursor in insert mode, block cursor in normal mode
- Vim text object motions (diw, ciw, daw, etc.)

Known Issues:
- Ctrl+C exits IME (should clear preedit instead)

## Architecture

See `IDEA.md` for design rationale and future ideas.
