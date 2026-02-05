# Custom IME for Linux Wayland

## Context / Goal

I want to implement a custom IME (Input Method Editor) on Linux Wayland, purely for fun and learning.

- **Target environment:** Hyprland (wlroots-based compositor)
- **Explicit non-dependencies:** fcitx, ibus (too large; I want to understand and control the IME stack myself)
- **Backend idea:** Neovim running headless, with the Wayland-facing IME acting as a thin frontend

---

## Understanding So Far

### Wayland IME Architecture

On Wayland, IME is compositor-controlled:

- **Applications** (GTK/Qt/Electron) talk to the compositor via `zwp_text_input_v3`
- **IME** talks to the compositor via the privileged protocol `zwp_input_method_v2`
- Only one IME client can bind `zwp_input_method_v2`, and the compositor decides who is allowed

### GNOME / KDE Situation

- GNOME (Mutter) and KDE (KWin) do **not** allow arbitrary IME clients to bind `zwp_input_method_v2`
- In practice, they only support IBus or Fcitx as the IME bridge
- This is enforced at the compositor level for security reasons (not application-level)
- XIM is X11-only and effectively dead
- UIM on Wayland runs through IBus/Fcitx, so it doesn't avoid them

**Conclusion:** A custom standalone IME cannot work on GNOME/KDE Wayland without going through ibus/fcitx.

### Why Hyprland / wlroots Works

- Hyprland uses wlroots and does **not** hardcode an IME provider
- A custom Wayland client can directly bind `zwp_input_method_v2`
- This makes it possible to implement a fully custom IME without fcitx/ibus

---

## Architecture

### High-Level Flow

```
Keyboard
  ↓
Hyprland (wlroots)
  ↓
Custom IME (Wayland client)
  - binds zwp_input_method_v2
  - handles key events
  - manages preedit / commit
  - renders candidate UI (input_popup_surface)
  ↓
Hyprland
  ↓
Applications (via zwp_text_input_v3)
```

### Module Structure

```
src/
  main.rs                    # Entry point, Wayland dispatch, coordination
  state/
    wayland.rs               # WaylandState (protocol handles, serial)
    keyboard.rs              # KeyboardState (XKB, modifiers, debouncing)
    ime.rs                   # ImeState, ImeMode state machine, VimMode
  neovim/
    mod.rs                   # NeovimHandle (public API)
    protocol.rs              # ToNeovim, FromNeovim typed messages
    handler.rs               # Tokio-side message handling
    event_source.rs          # Calloop event source (infrastructure)
  ui/
    candidate_window.rs      # Candidate popup UI
    text_render.rs           # Font rendering with fontdue
```

### State Management

**ImeMode State Machine** (replaces boolean flags):
- `Disabled` - IME off, keyboard not grabbed, passthrough mode
- `Enabling` - Waiting for keymap after keyboard grab
- `Enabled` - Active, processing input (contains `VimMode` and `skkeleton_active`)
- `Disabling` - Releasing keyboard

**VimMode** (within Enabled state):
- `Insert` - Characters inserted at cursor
- `Normal` - Commands and motions
- `Visual` - Selection active
- `OperatorPending` - Waiting for motion (e.g., after `d`)

### Neovim Communication

- **Bounded channels** (capacity 64) for backpressure
- **Typed protocol** with serde for JSON parsing
- **Separate thread** running Tokio runtime for async Neovim IPC

### IME Backend: Neovim + vim-skkeleton

- **Neovim** (headless, `--embed` mode)
- **vim-skkeleton** plugin for SKK (Simple Kana to Kanji) conversion
- IPC via msgpack-rpc (nvim-rs crate)

```
Key events → Neovim (skkeleton) → preedit/commit responses
```

SKK is a Japanese input method where:
- Lowercase = hiragana
- Uppercase = start kanji conversion
- Simple, modal, programmer-friendly

---

## Non-Goals / Constraints

- Not aiming for GNOME/KDE compatibility
- Not using fcitx or ibus
- Not using XIM
- This is a learning / experimental project, not production-ready

---

## Open Questions

- wlroots / Wayland protocol details for IME implementation
- Common pitfalls when implementing `zwp_input_method_v2`
- Designing IME UI positioning (cursor rect, candidate window)

---

## Implemented Features

### IME Toggle via Alt+`

- **Alt+`** triggers SIGUSR1 signal to toggle IME (configured in Hyprland keybind)
- Avoids Ctrl+J conflict with browser shortcuts
- Passthrough mode by default - keyboard only grabbed when IME is enabled

### Separate Confirm vs Commit ✓

- **Enter** = confirm skkeleton conversion (stay in preedit when ▽/▼ markers present)
- **Ctrl+Enter** = commit preedit text to application

This allows composing longer text with multiple conversions:
```
Type: きょうは → ▽きょうは
Space: → ▼今日は
Enter: → 今日は (confirmed, still in preedit!)
Type more: → 今日はいい▽てんき
Space: → 今日はいい▼天気
Enter: → 今日はいい天気 (still in preedit!)
Ctrl+Enter: → commit "今日はいい天気" to app
```

### Basic Vim Mode

- **Escape** switches to normal mode in neovim
- **Ctrl+C** exits IME (releases keyboard grab)

### Cursor Position Display ✓

- Insert mode: line cursor at current position
- Normal mode: block cursor (highlighted character) on current character
- Properly handles multibyte characters (Japanese UTF-8)

---

### Vim Text Object Motions ✓

Text object motions like `diw`, `ciw`, `daw` now work:
- Tracks operator-pending mode locally to avoid RPC hangs
- Detects motion completion (simple motions, text object prefixes)
- Resumes normal queries after operation completes

---

## Known Issues / TODO

### Ctrl+C Behavior

Currently Ctrl+C exits the IME. Should instead:
- Clear preedit text
- Return to insert mode
- Stay active

---

## Future Ideas: Leveraging Neovim Power

### Full Vim Modal Editing in Preedit

The main advantage of Neovim backend over fcitx+skk is full vim power in preedit:

- **Normal mode**: `hjkl`, `w`/`b` word motions, `ciw`, `r`, `x`, etc.
- **Registers**: `"ay` to save, `"ap` to paste
- **Undo/redo**: Full undo tree
- **Macros**: `qa...q` recording, `@a` playback

### Other nvim-cmp Sources

Could add completion sources beyond skkeleton:
- Emoji: `:thinking:` → 🤔
- Math symbols: `\alpha` → α
- User snippets/abbreviations
