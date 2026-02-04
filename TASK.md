# Implementation Tasks

**Language:** Rust

---

## Phase 1: Project Setup & Protocol Binding

- [x] Initialize Cargo project (`cargo init`)
- [x] Add dependencies:
  - `wayland-client` - Wayland protocol bindings
  - `wayland-protocols` - Standard protocol definitions
  - `wayland-protocols-misc` - For `zwp_input_method_v2`
  - ~~`smithay-client-toolkit`~~ - Not needed for basic IME
- [x] Generate/import `zwp_input_method_v2` protocol bindings
- [x] Connect to Wayland display (`Connection::connect_to_env`)
- [x] Bind `zwp_input_method_manager_v2` global from registry
- [x] Create `zwp_input_method_v2` object
- [x] Implement `Dispatch` for `activate` / `deactivate` events
- [x] Set up event loop (`calloop` + `calloop-wayland-source`)

## Phase 2: Keyboard Grab

- [x] Request `zwp_input_method_keyboard_grab_v2` on activate
- [x] Implement `Dispatch` for keyboard grab events:
  - `keymap` - Parse xkb keymap
  - `key` - Handle key press/release
  - `modifiers` - Track modifier state
  - `repeat_info` - Key repeat settings
- [x] Add `xkbcommon` crate for keymap handling
- [x] ~~Release grab properly on deactivate~~ (causes issues, keep grab until exit)
- [x] Track grab state to avoid double-grab

## Phase 3: Simple Passthrough

- [x] Convert keysym to UTF-8 string (`xkbcommon`)
- [x] Call `commit_string()` for printable characters
- [x] Call `commit()` after each string commit
- [x] Handle backspace (`delete_surrounding_text`), enter, tab
- [x] Test with terminal and Firefox
- [x] Verify characters appear correctly

**Known limitations (Phase 3):**
- Arrow keys, Home, End, etc. not forwarded (needs `zwp_virtual_keyboard_v1`)
- Ctrl+shortcuts not forwarded (same reason)
- Ctrl+C exits IME (development convenience)

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

- [ ] Implement `zwp_virtual_keyboard_v1` for forwarding shortcuts/navigation
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

## Crate Summary (Current)

```toml
[dependencies]
wayland-client = "0.31"
wayland-protocols = { version = "0.32", features = ["client"] }
wayland-protocols-misc = { version = "0.3", features = ["client"] }
calloop = { version = "0.14", features = ["signals"] }
calloop-wayland-source = "0.4"
anyhow = "1"
xkbcommon = "0.8"
libc = "0.2"
```

---

## Notes

### Lessons Learned

- Releasing keyboard grab on deactivate causes key replay issues (Ctrl+T loop in Firefox)
- Solution: Keep grab until IME exits, don't release on window switch
- Use `eprintln!` (stderr) instead of `println!` (stdout) to avoid terminal feedback loops
- Add startup delay (500ms) to let pending key events clear after `cargo run`

### Pitfalls to Watch

- Must call `commit()` after `set_preedit_string()` / `commit_string()` batch
- Serial number synchronization is required
- Only one IME can bind `zwp_input_method_v2` at a time
- `wayland-rs` uses `Dispatch` trait pattern - different from C callbacks
- Borrowed fd from keymap event - don't take ownership

### Useful References

- [wayland-rs book](https://smithay.github.io/wayland-rs/)
- [smithay-client-toolkit examples](https://github.com/Smithay/client-toolkit/tree/master/examples)
- [zwp_input_method_v2 protocol](https://wayland.app/protocols/input-method-unstable-v2)
- [zwp_text_input_v3 protocol](https://wayland.app/protocols/text-input-unstable-v3)
- [wlr-layer-shell protocol](https://wayland.app/protocols/wlr-layer-shell-unstable-v1)
- [nvim-rs documentation](https://docs.rs/nvim-rs)
