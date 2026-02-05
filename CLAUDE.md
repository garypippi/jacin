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

## Key Files

- `src/main.rs` - Wayland event loop, protocol dispatch, State struct
- `src/neovim.rs` - Neovim IPC, key handling, skkeleton/cmp integration
- `src/candidate_window.rs` - Candidate popup UI using input_popup_surface
- `src/text_renderer.rs` - Font rendering with fontdue

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

Known Issues:
- Vim motions like `diw`, `ciw` not working (text objects)

Not yet implemented:
- Text object motions (diw, ciw, etc.)
- Ctrl+C to clear preedit (currently exits IME)

## Architecture

See `IDEA.md` for design rationale and future ideas.
