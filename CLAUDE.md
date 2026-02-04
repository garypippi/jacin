# Custom IME for Wayland

A custom Input Method Editor for Linux Wayland (Hyprland/wlroots), with Neovim as the backend.

## Tech Stack

- **Language:** Rust
- **Wayland:** wayland-client, smithay-client-toolkit
- **Protocols:** zwp_input_method_v2, wlr-layer-shell
- **Backend:** Neovim (headless) via nvim-rs

## Commands

```sh
cargo build          # Build
cargo run            # Run (requires Hyprland)
cargo clippy         # Lint
cargo fmt            # Format
```

## Architecture

See `IDEA.md` for design and `TASK.md` for implementation progress.
