# Design Document

## 1. Architecture Overview

```
┌────────────────────────────────────────────────────────────────┐
│              Wayland Compositor (Hyprland)                      │
│  zwp_input_method_v2  zwp_input_popup_surface_v2  wl_shm       │
└──┬────────────────────────────▲────────────────────▲───────────┘
   │ Events (key, activate,     │ set_preedit,       │ buffer
   │  deactivate, done)         │ commit_string,     │ attach
   │                            │ commit(serial)     │
   ▼                            │                    │
┌────────────────────────────────────────────────────────────────┐
│                     Main Thread (calloop)                       │
│                                                                │
│  dispatch.rs     Wayland event dispatch (key, activate, etc.)  │
│  input.rs        handle_key → keysym→Vim notation → ToNeovim  │
│  model.rs        Model::reduce(FromNeovim) → Vec<Effect>       │
│  coordinator.rs  apply_effects → preedit/commit/popup          │
│                                                                │
│  State:                                                        │
│    ImeState      mode (Disabled/Enabling/Enabled), preedit     │
│    NvimView      observed vim mode, cmdline, candidates, msgs  │
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
│  handler.rs              │  │    preedit + cursor               │
│    recv ToNeovim::Key    │  │    keypress display              │
│    nvim.input / exec_lua │  │    candidates + scrollbar        │
│    handle_redraw         │  │    mode indicator                │
│      (ext_cmdline,       │  │                                  │
│       ext_popupmenu,     │  │  TextRenderer (fontdue + cache)  │
│       ext_messages,      │  │  Layout (layout.rs)              │
│       mode_change)       │  └──────────────────────────────────┘
│    → FromNeovim (ch)     │
│                          │
│  Neovim process          │
│  (headless, nvim-rs)     │
│  + plugins (skkeleton…)  │
└──────────────────────────┘
```

## 2. RPC Strategy

### Insert Mode: Fire-and-Forget

```
Main Thread          Handler Thread       Neovim
    |  ToNeovim::Key      |                  |
    |-------------------->|  nvim.input()    |
    |  FromNeovim::        |---------------->|
    |  KeyProcessed       |                  | TextChangedI autocmd
    |<--------------------|                  | → rpcnotify("ime_snapshot")
    |  (ready for next)   |<-----------------|
    |                     |  FromNeovim::Preedit
    |<--------------------|
    |--> update preedit   |
```

1 RPC + 1 push notification. No blocking wait.

### Normal Mode: Synchronous 2-RPC

```
Main Thread          Handler Thread       Neovim
    |  ToNeovim::Key      |                  |
    |-------------------->|  nvim.input()    |
    |                     |  exec_lua(       |
    |                     |   collect_snapshot)
    |                     |<-----------------|
    |  FromNeovim::Preedit|                  |
    |<--------------------|
    |--> update preedit   |
```

2 RPCs (nvim_input + collect_snapshot). Synchronous — normal mode operations complete immediately in Neovim.

### Special Keys (BS, Commit, Enter)

Single `exec_lua("return ime_handle_*()")` — combines check and action in one RPC.

### nvim_ui_attach Redraw Events

`nvim_ui_attach` with `ext_cmdline`, `ext_popupmenu`, `ext_messages` extensions. The `handle_redraw` dispatcher processes:
- `cmdline_show/pos/hide` — command-line display
- `popupmenu_show/select/hide` — completion candidates
- `msg_show/msg_clear` — command output messages
- `mode_change` — immediate mode updates

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

Not tracked by the IME state machine. `NvimView.vim_mode` mirrors Neovim (`mode_change` redraw event and snapshots).

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

All pending states resolve back to None after the sequence completes. `CommandLine` resolves on `CmdlineLeave` autocmd.

Owned by the Neovim thread (`Arc<AtomicPendingState>` shared by the key loop and the notification handler, recreated on each spawn). The main thread only sees it through `FromNeovim::KeyProcessed { pending }`, sent exactly once per key after all data messages for that key.

## 4. Core Design Principles

### Neovim = Single Source of Truth

- IME only reads Neovim's buffer via `collect_snapshot()`, never writes directly
- `ImeState.preedit` is a cache of Neovim's response — never edited locally
- All text manipulation goes through `nvim.input()` to preserve undo/redo/macros/plugins

### Reducer + Effects

- `Model::reduce(msg, active) -> Vec<Effect>` (model.rs) is the only place that updates IME state from Neovim messages; it performs no I/O
- `State::apply_effects` (coordinator.rs) executes effects: compositor preedit/commit, popup render, passthrough, Neovim input
- Replay tests feed `FromNeovim` sequences (tests/fixtures) through the real reducer

### Dual Display

- `set_preedit_string()` → shown inline by the app (app-dependent styling)
- UnifiedPopup → IME-controlled overlay (preedit + cursor + candidates + mode)
- Both are different views of the same data

### Wayland-Dependent vs Independent

| Wayland-dependent | Wayland-independent (testable) |
|---|---|
| WaylandState (protocol ops, serial) | ImeState (pure state machine) |
| KeyboardState (XKB keymap) | KeypressState (pure logic) |
| SIGUSR1 signal handling | Neovim handler (RPC only) |
| SHM buffer attach/commit | Config, TextRenderer, Layout |

## 5. Extension Notes: Multiline

```
Snapshot display: Neovim buffer = 1 line → preedit = 1 line → popup = h-scroll (<CR> auto-commits)
Grid display:     Neovim buffer = N lines → app preedit = none → popup = window grid
```

Grid display sets `ime_context.multiline`, which disables `check_line_added` so `<CR>` is a native newline. `<CR>`/`<BS>`/commit key pass through only when the whole buffer is empty. The commit key and IME off commit all lines joined with `\n` (IME off uses the snapshot's `buffer_text`). Trailing empty lines are kept. See `MULTILINE.md` "Commit Behavior".
