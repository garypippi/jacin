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
│  coordinator.rs  handle_nvim_message → update preedit/popup    │
│                                                                │
│  State:                                                        │
│    ImeState      mode (Disabled/Enabling/Enabled), preedit     │
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
 Disabled ──────────> Enabling ──────────────> Enabled {vim_mode}
     ^                                             │
     │              disable() (toggle-off/commit)  │
     └─────────────────────────────────────────────┘
```

- Deactivate/Activate cycle: Enabled → release grab → re-grab → Enabling → keymap → Enabled (state restored)
- `reactivation_count` caps consecutive re-grabs at 2 to prevent infinite loops

### VimMode (Axis 2, inside Enabled)

Insert ←→ Normal. Visual/operator-pending are observed from Neovim's mode string, not tracked as VimMode variants.

### PendingState (Axis 3, atomic cross-thread)

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

## 4. Core Design Principles

### Neovim = Single Source of Truth

- IME only reads Neovim's buffer via `collect_snapshot()`, never writes directly
- `ImeState.preedit` is a cache of Neovim's response — never edited locally
- All text manipulation goes through `nvim.input()` to preserve undo/redo/macros/plugins

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
Current: Neovim buffer = 1 line → preedit = 1 line → popup = h-scroll
Future:  Neovim buffer = N lines → app preedit = current line only → popup = multiline
```

Modules requiring changes: `handler.rs` (multiline snapshot), `protocol.rs` (multiline PreeditInfo), `ime.rs` (multiline storage), `coordinator.rs` (extract current line for compositor), `unified_window.rs` (multiline rendering).
