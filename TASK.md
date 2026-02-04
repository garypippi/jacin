# Implementation Tasks

**Language:** Rust

---

## Phase 1: Project Setup & Protocol Binding

- [ ] Initialize Cargo project (`cargo init`)
- [ ] Add dependencies:
  - `wayland-client` - Wayland protocol bindings
  - `wayland-protocols` - Standard protocol definitions
  - `wayland-protocols-misc` - For `zwp_input_method_v2`
  - `smithay-client-toolkit` - High-level client helpers
- [ ] Generate/import `zwp_input_method_v2` protocol bindings
- [ ] Connect to Wayland display (`Connection::connect_to_env`)
- [ ] Bind `zwp_input_method_manager_v2` global from registry
- [ ] Create `zwp_input_method_v2` object
- [ ] Implement `Dispatch` for `activate` / `deactivate` events
- [ ] Set up event loop (`calloop` or `wayland-client` built-in)

## Phase 2: Keyboard Grab

- [ ] Request `zwp_input_method_keyboard_grab_v2` on activate
- [ ] Implement `Dispatch` for keyboard grab events:
  - `keymap` - Parse xkb keymap
  - `key` - Handle key press/release
  - `modifiers` - Track modifier state
  - `repeat_info` - Key repeat settings
- [ ] Add `xkbcommon` crate for keymap handling
- [ ] Release grab properly on deactivate
- [ ] Track grab state to avoid double-grab

## Phase 3: Simple Passthrough

- [ ] Convert keysym to UTF-8 string (`xkbcommon`)
- [ ] Call `commit_string()` for printable characters
- [ ] Call `commit()` after each string commit
- [ ] Forward non-printable keys (backspace, enter, etc.)
- [ ] Test with `foot` terminal or GTK entry widget
- [ ] Verify characters appear correctly

## Phase 4: Preedit (Composition)

- [ ] Implement `set_preedit_string()` for composition display
- [ ] Track preedit buffer internally (`String`)
- [ ] Set preedit cursor position (`cursor_begin`, `cursor_end`)
- [ ] Clear preedit on commit (`set_preedit_string("")`)
- [ ] Handle preedit styling (underline via `set_preedit_style`)
- [ ] Test: type "abc" shows preedit, Enter commits

## Phase 5: Neovim Backend Integration

- [ ] Add `nvim-rs` crate for msgpack-rpc
- [ ] Add async runtime (`tokio`)
- [ ] Design Neovim plugin API:
  - Function to receive key events
  - Function to return conversion candidates
  - Events for preedit/commit updates
- [ ] Launch headless Neovim (`nvim --embed`)
- [ ] Establish RPC connection
- [ ] Send key events to Neovim
- [ ] Receive preedit/commit responses
- [ ] Implement basic romaji → hiragana conversion in Neovim

## Phase 6: Candidate Window UI

- [ ] Add `wayland-protocols-wlr` for layer-shell
- [ ] Bind `zwlr_layer_shell_v1` protocol
- [ ] Create layer surface (layer: `OVERLAY`, anchor: configurable)
- [ ] Add rendering backend:
  - Option A: `softbuffer` + `tiny-skia` (CPU, simple)
  - Option B: `wgpu` (GPU, more complex)
- [ ] Add font rendering (`cosmic-text` or `fontdue`)
- [ ] Render candidate list
- [ ] Position window near cursor (use `cursor_rect` from IME events)
- [ ] Handle candidate selection (number keys, arrows)
- [ ] Show/hide window based on IME state

## Phase 7: Polish & Edge Cases

- [ ] Handle `surrounding_text` event (if provided by application)
- [ ] Proper serial number tracking for all requests
- [ ] Handle rapid activate/deactivate cycles gracefully
- [ ] Test with various applications:
  - GTK4 apps
  - Qt apps
  - Electron apps (VS Code, Discord)
  - Firefox
- [ ] Graceful error handling (connection loss, etc.)
- [ ] Logging with `tracing` crate

---

## Crate Summary

```toml
[dependencies]
wayland-client = "0.31"
wayland-protocols = { version = "0.31", features = ["client"] }
wayland-protocols-misc = { version = "0.3", features = ["client"] }
smithay-client-toolkit = "0.18"
xkbcommon = "0.7"
calloop = "0.12"
calloop-wayland-source = "0.2"

# Phase 5+
tokio = { version = "1", features = ["rt", "process", "io-util"] }
nvim-rs = { version = "0.6", features = ["use_tokio"] }

# Phase 6+
wayland-protocols-wlr = { version = "0.2", features = ["client"] }
tiny-skia = "0.11"
softbuffer = "0.4"
cosmic-text = "0.11"
```

---

## Notes

### Pitfalls to Watch

- Must call `commit()` after `set_preedit_string()` / `commit_string()` batch
- Serial number synchronization is required
- Keyboard grab must be released on deactivate
- `surrounding_text` may be empty or unavailable
- Only one IME can bind `zwp_input_method_v2` at a time
- `wayland-rs` uses `Dispatch` trait pattern - different from C callbacks

### Useful References

- [wayland-rs book](https://smithay.github.io/wayland-rs/)
- [smithay-client-toolkit examples](https://github.com/Smithay/client-toolkit/tree/master/examples)
- [zwp_input_method_v2 protocol](https://wayland.app/protocols/input-method-unstable-v2)
- [zwp_text_input_v3 protocol](https://wayland.app/protocols/text-input-unstable-v3)
- [wlr-layer-shell protocol](https://wayland.app/protocols/wlr-layer-shell-unstable-v1)
- [nvim-rs documentation](https://docs.rs/nvim-rs)
