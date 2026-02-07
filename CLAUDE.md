# Custom IME for Wayland

A custom Input Method Editor for Linux Wayland (Hyprland/wlroots), with Neovim as the backend.

## Tech Stack

- **Language:** Rust
- **Wayland:** wayland-client, smithay-client-toolkit
- **Protocols:** zwp_input_method_v2, zwp_input_popup_surface_v2, zwp_virtual_keyboard_v1
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
  main.rs                    # Entry point, State struct, event loop setup
  dispatch.rs                # Wayland Dispatch impls, memmap_keymap
  input.rs                   # keysym_to_vim, key processing, keypress display
  coordinator.rs             # Neovim response handling, IME toggle, preedit/popup coordination
  config.rs                  # Config file loading (TOML), keybind defaults
  state/
    mod.rs                   # Re-exports
    wayland.rs               # WaylandState (protocol handles, serial, virtual keyboard)
    keyboard.rs              # KeyboardState (XKB, modifiers, debouncing, repeat params)
    repeat.rs                # KeyRepeatState (key repeat timing/tracking)
    ime.rs                   # ImeState, ImeMode state machine, VimMode
    keypress.rs              # KeypressState (accumulated keys, pending type, timeout)
  neovim/
    mod.rs                   # NeovimHandle (public API)
    protocol.rs              # ToNeovim, FromNeovim typed messages (serde)
    handler.rs               # Tokio-side Neovim message handling
    event_source.rs          # Calloop event source (infrastructure)
  ui/
    mod.rs                   # Re-exports
    unified_window.rs        # Unified popup (preedit, keypress, candidates)
    text_render.rs           # Font rendering with fontdue, SHM utilities
```

## Key Components

- **Config module**: TOML config at `~/.config/custom-ime/config.toml` with configurable keybinds (toggle, commit)
- **State modules**: Separate concerns into `WaylandState`, `KeyboardState`, `KeyRepeatState`, `ImeState`, `KeypressState`
- **ImeMode state machine**: Explicit states (Disabled, Enabling, Enabled, Disabling) replacing boolean flags
- **Typed Neovim protocol**: Serde-based `ToNeovim`/`FromNeovim` messages with bounded channels
- **Optimized RPC**: Insert mode uses fire-and-forget (`nvim_input` + push notification via `rpcnotify`); normal mode uses 2-RPC pull (`nvim_input` + `collect_snapshot()`); special keys use single Lua function calls
- **UI module**: Unified popup window (preedit with cursor, keypress display, candidates with scrollbar)

## Current State

Working:
- Basic Japanese input via skkeleton
- Configurable toggle key (default Alt+`) to toggle IME (via SIGUSR1 signal, triggered by Hyprland keybind)
- General Alt key support (Alt+any key produces `<A-...>` Vim notation)
- Passthrough mode by default (keyboard only grabbed when IME enabled)
- Toggle-off commits pending preedit text to application before disabling
- Survives Activate/Deactivate cycles (e.g., switching cells in spreadsheets) with re-grab loop protection
- Candidate window follows cursor (via zwp_input_popup_surface_v2)
- nvim-cmp integration for candidate selection (Ctrl+N/P, Ctrl+K to confirm)
- Enter confirms skkeleton conversion (stays in preedit when ▽/▼ markers present)
- Configurable commit key (default Ctrl+Enter) commits preedit text to application
- Escape switches to normal mode in neovim
- Cursor position display: line cursor in insert mode, block cursor in normal mode
- Vim text object motions (diw, ciw, daw, etc.)
- Getchar-blocking keys (q, f, t, r, m, etc.) handled via nvim_get_mode() blocking detection
- Auto-recovery from command-line mode (skkeleton nested henkan can trigger it)
- Yank & paste: y$, yw, yiw, <C-r>" (insert mode), "ay$ (named registers)
- Unified popup window: shows preedit with cursor (block/line), keypress sequences, and candidates
- Preedit has max width with cursor-centered scrolling for long text
- Keypress display: shows insert mode entry keys (i, a, A, o), register paste sequences (<C-r>a), and completed operator sequences (d$, "ay$) for 1.5s
- Key repeat: held keys repeat using compositor's rate/delay, respects XKB per-key repeat flags (modifiers don't repeat)
- Visual mode (character-wise): `v` enters visual, `h`/`l`/`w`/`b` extend selection with highlight, `d`/`c`/`y`/`x` operate on selection, `"ay` yanks to named register
- Command mode: `:w` commits preedit (keep enabled), `:wq`/`:x` commit+disable, `:q`/`:q!` discard+disable, other commands pass through to Neovim

- Modifier clearing: uses zwp_virtual_keyboard_v1 to clear stuck modifiers (e.g., Alt from toggle keybind) on grab start and release

Known Issues:
- Multiline operations (yy, dd, cc, p, P) not yet supported (single-line preedit only)

## Architecture

See `IDEA.md` for design rationale and future ideas.
