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
- Basic Japanese input via skkeleton (Ctrl+J to toggle)
- Candidate window follows cursor (via zwp_input_popup_surface_v2)
- nvim-cmp integration for candidate selection (Ctrl+N/P, Ctrl+K to confirm)
- Enter confirms skkeleton conversion, commits if no conversion active

Not yet implemented:
- Vim modal editing in preedit (normal mode, motions)
- Separate Enter (confirm) vs Ctrl+Enter (commit)
- Preedit-only mode (passthrough when empty)

## Architecture

See `IDEA.md` for design rationale and future ideas.
