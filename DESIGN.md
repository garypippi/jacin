# Design Document

## 1. Architecture Overview

```
┌────────────────────────────────────────────────────────────────┐
│              Wayland Compositor (Hyprland)                      │
│  zwp_input_method_v2  zwp_input_popup_surface_v2  wl_shm       │
└──┬────────────────────────────▲────────────────────▲───────────┘
   │ Events (key, activate,     │ commit_string,     │ buffer
   │  deactivate, done)         │ commit(serial)     │ attach
   │                            │                    │
   ▼                            │                    │
┌────────────────────────────────────────────────────────────────┐
│                     Main Thread (calloop)                       │
│                                                                │
│  dispatch.rs     Wayland event dispatch (key, activate, etc.)  │
│  input.rs        handle_key → keysym→Vim notation → ToNeovim  │
│  model.rs        Model::reduce(FromNeovim) → Vec<Effect>       │
│  coordinator.rs  apply_effects → commit/popup/passthrough      │
│                                                                │
│  State:                                                        │
│    ImeState      mode (Disabled/Enabling/Enabled)              │
│    NvimView      observed vim mode, cmdline, candidates, msgs, │
│                  Screen (grids), BufferMirror (buffer lines)   │
│    KeyboardState XKB context, modifiers                        │
│    KeypressState accumulated key sequences, display timeout    │
│    WaylandState  protocol handles, serial, virtual keyboard    │
│                                                                │
│  Calloop sources:                                              │
│    WaylandSource, SIGUSR1 ping, repeat timer, display timer    │
└──────────┬──────────────────────────────────────┬──────────────┘
           │ ToNeovim (crossbeam ch)              │ PopupContent
           ▼                                      ▼
┌──────────────────────────┐  ┌──────────────────────────────────┐
│  Neovim Backend          │  │  UI Layer                        │
│  (Tokio thread)          │  │                                  │
│                          │  │  UnifiedPopup                    │
│  handler.rs              │  │    window grid + floats + cursor │
│    recv ToNeovim::Key    │  │    keypress display              │
│    nvim.input / get_mode │  │    candidates + scrollbar        │
│    handle_redraw         │  │    mode indicator                │
│      (ext_cmdline,       │  │                                  │
│       ext_popupmenu,     │  │  TextRenderer (fontdue + cache)  │
│       ext_messages,      │  │  Layout (layout.rs)              │
│       ext_multigrid,     │  └──────────────────────────────────┘
│       mode_change)       │
│    nvim_buf_lines_event  │
│    → FromNeovim (ch)     │
│                          │
│  Neovim process          │
│  (headless, nvim-rs)     │
│  + plugins (skkeleton…)  │
└──────────────────────────┘
```

## 2. RPC Strategy

No Lua is injected and no autocmds are defined: jacin only calls the
Neovim API and observes UI (redraw) and buffer events.

### Insert Mode: Fire-and-Forget

```
Main Thread          Handler Thread       Neovim
    |  ToNeovim::Key      |                  |
    |-------------------->|  nvim.input()    |
    |  FromNeovim::        |---------------->|
    |  KeyProcessed       |                  | redraw (grid_line, flush)
    |<--------------------|                  | nvim_buf_lines_event
    |  (ready for next)   |<-----------------|
    |                     |  FromNeovim::GridFlush / BufLines
    |<--------------------|
    |--> render popup     |
```

1 RPC + push notifications. No blocking wait.

### Normal Mode: Synchronous 2-RPC

```
Main Thread          Handler Thread       Neovim
    |  ToNeovim::Key      |                  |
    |-------------------->|  nvim.input()    |
    |                     |  nvim_get_mode() |
    |                     |<-----------------|
    |  KeyProcessed       |  (Getchar / Motion pending, last_mode)
    |<--------------------|
```

2 RPCs (nvim_input + nvim_get_mode, a fast API that answers even while
blocked in getchar). The display follows from redraw events. After `q`
sequences, `reg_recording()` updates the REC indicator.

### Special Keys (BS, Enter, Commit)

One `nvim_buf_get_lines` RPC: if the whole buffer is empty the key passes
through to the app, otherwise BS/Enter go to Neovim and the commit key
commits all lines joined with `\n`.

### nvim_ui_attach Redraw Events

`nvim_ui_attach` with `ext_cmdline`, `ext_popupmenu`, `ext_messages`, `ext_multigrid` extensions. The `handle_redraw` dispatcher processes:
- `cmdline_show/pos/hide` — command-line display; `cmdline_hide` at level 1 ends the CommandLine pending state
- `popupmenu_show/select/hide` — completion candidates
- `msg_show/msg_clear` — command output messages
- `mode_change` — mode updates
- grid events (`grid_line`, `win_pos`, `win_float_pos`, `win_viewport`, …) — batched per `flush` into `FromNeovim::GridFlush`, mirrored in `Screen`

### Buffer Events

`nvim_buf_attach` on the current buffer keeps `BufferMirror` in sync via `nvim_buf_lines_event`; on `nvim_buf_detach_event` (e.g. `:enew` wipes the buffer) jacin re-attaches to the current buffer. IME off commits the mirror without an RPC.

## 3. State Model

### ImeMode (Axis 1)

```
              SIGUSR1             keymap event
 Disabled ──────────> Enabling ──────────────> Enabled         
     ^                                             │
     │              disable() (toggle-off/commit)  │
     └─────────────────────────────────────────────┘
```

- Deactivate/Activate cycle: Enabled → release grab → re-grab → Enabling → keymap → Enabled (state restored)
- Activate/Deactivate are deferred to the `Done` event and processed together (deactivate first), so a window switch becomes a single release + re-grab

### Vim mode (Axis 2, observed)

Not tracked by the IME state machine. `NvimView.vim_mode` mirrors Neovim (`mode_change` redraw event).

### PendingState (Axis 3, per Neovim session)

```
             None
          ╱  │  │  ╲
    Getchar  │  │  CommandLine
   (q,f,t,r) │  │   (:)
              │  │
     InsertReg│  │Motion (d,c,y)
     (<C-r>)  │  │  ↓
              │  TextObject (di_, ca_)
         NormalReg
           (")
```

All pending states resolve back to None after the sequence completes. `CommandLine` resolves on `cmdline_hide` (level 1).

Owned by the Neovim thread (`Arc<AtomicPendingState>` shared by the key loop and the notification handler, recreated on each spawn). The main thread only sees it through `FromNeovim::KeyProcessed { pending }`, sent exactly once per key after all data messages for that key.

## 4. Core Design Principles

### Neovim = Single Source of Truth

- IME only observes Neovim (UI events, buffer events, `nvim_buf_get_lines`), never writes the buffer directly
- `Screen` and `BufferMirror` are mirrors of Neovim — never edited locally
- All text manipulation goes through `nvim.input()` to preserve undo/redo/macros/plugins

### Reducer + Effects

- `Model::reduce(msg) -> Vec<Effect>` (model.rs) is the only place that updates IME state from Neovim messages; it performs no I/O
- `State::apply_effects` (coordinator.rs) executes effects: commit, popup render, passthrough, Neovim input
- Replay tests feed `FromNeovim` sequences (tests/fixtures) through the real reducer

### Popup-Only Display

- The app gets no preedit (`set_preedit_string` is only used to clear it on commit); text appears in the app only on commit
- UnifiedPopup renders Neovim's window grid (with floats such as nvim-cmp menus), keypress/mode, and candidates

### Wayland-Dependent vs Independent

| Wayland-dependent | Wayland-independent (testable) |
|---|---|
| WaylandState (protocol ops, serial) | ImeState (pure state machine) |
| KeyboardState (XKB keymap) | KeypressState (pure logic) |
| SIGUSR1 signal handling | Neovim handler (RPC only) |
| SHM buffer attach/commit | Config, TextRenderer, Layout |

## 5. Multiline

```
Neovim buffer = N lines → app preedit = none → popup = window grid (up to MAX_GRID_ROWS rows, scrolled to the cursor)
```

`<CR>` is a native newline. `<CR>`/`<BS>`/commit key pass through only when the whole buffer is empty. The commit key and IME off commit all lines joined with `\n` (IME off uses `BufferMirror`). Trailing empty lines are kept.
