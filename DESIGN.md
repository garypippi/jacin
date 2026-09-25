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
│                   Single thread (calloop)                       │
│                                                                │
│  dispatch.rs     Wayland event dispatch (key, activate, etc.)  │
│  input.rs        handle_key → keysym→Vim notation → push_key   │
│  model.rs        Model::reduce(FromNeovim) → Vec<Effect>       │
│  coordinator.rs  apply_effects, Neovim sources, popup          │
│                                                                │
│  State:                                                        │
│    ImeState      mode (Disabled/Enabling/Enabled)              │
│    NvimView      vim mode, cmdline, candidates, msgs,          │
│                  Screen (grids), BufferMirror (buffer lines)   │
│    SentKeys      keys sent to Neovim, awaiting KeyProcessed    │
│    KeyboardState XKB context, modifiers                        │
│    WaylandState  protocol handles, serial, virtual keyboard    │
│                                                                │
│  Calloop sources:                                              │
│    WaylandSource, SIGINT/SIGTERM, control socket (toggle),     │
│    Neovim stdout (+ stdin while the pipe is full),             │
│    repeat timer, animation timer (REC blink, message expiry)   │
└──────────┬──────────────────────────────────────┬──────────────┘
           │ Neovim (non-blocking calls)          │ PopupContent
           ▼                                      ▼
┌──────────────────────────┐  ┌──────────────────────────────────┐
│  Neovim Backend (neovim/)│  │  UI Layer                        │
│                          │  │                                  │
│  mod.rs   Neovim: glue,  │  │  UnifiedPopup                    │
│           request ids,   │  │    mode + window grid + floats + │
│           startup        │  │    cursor; panel when needed:    │
│  keys.rs  KeyProcessor   │  │    cmdline or showcmd, message,  │
│           (no I/O)       │  │    candidates + scrollbar        │
│  redraw.rs  redraw →     │  │                                  │
│           FromNeovim     │  │  TextRenderer (fontdue + cache)  │
│  rpc.rs   msgpack-rpc    │  │  Layout (layout.rs)              │
│           codec (no I/O) │  └──────────────────────────────────┘
│  process.rs  nvim --embed│
│           non-blocking   │
│           pipes          │
│                          │
│  Neovim process          │
│  + plugins (skkeleton…)  │
└──────────────────────────┘
```

Everything runs on one thread: Neovim's stdout is an event source like the
Wayland socket, and nothing waits for Neovim.

## 2. RPC Strategy

No Lua is injected and no autocmds are defined: jacin only calls the
Neovim API and observes UI (redraw) and buffer events. msgpack-rpc is
implemented in `rpc.rs` (with `rmpv` for values).

### Message Order

Messages are handled one at a time: each message's events are applied to
the model before the next message is decoded. Notifications (redraw, buffer
events) and responses come in the order Neovim sent them on its single
stdout, so the model's mode — set by both `mode_change` and `nvim_get_mode`
replies — is always the latest Neovim reported.

### Key Queue

`handle_key` registers the key in `SentKeys` (seq, keycode, modifiers, Vim
notation) and pushes it to the `KeyProcessor`, which processes queued keys
one at a time; at most one of its requests is outstanding. A held key does
not repeat while keys are still queued or processing.

`nvim_input` only queues keys in Neovim and `nvim_get_mode` (fast) answers
immediately, so a mode query sent together with the input would report the
state before the keys. Key-processing requests are therefore held until the
`nvim_input` before them has been answered.

### Insert Mode: Fire-and-Forget

```
KeyProcessor              Neovim
    |  nvim_input(key)      |
    |---------------------->|
    |  KeyProcessed         |  redraw (grid_line, flush)
    |  (at once)            |  nvim_buf_lines_event
    |<----------------------|
```

1 RPC + push notifications. The key is done as soon as it is sent. Keys that
wait for an argument in insert mode (`<C-k>`, `<C-v>`, `<C-q>`) also query
the mode.

### Other Modes: Input, then Mode

```
KeyProcessor              Neovim
    |  nvim_input(key)      |
    |---------------------->|
    |<----------------------|  (answered)
    |  nvim_get_mode()      |
    |---------------------->|
    |<----------------------|  mode, blocking
    |  KeyProcessed (Getchar / Motion / CommandLine / None)
```

`nvim_get_mode` is a fast API that answers even while Neovim is blocked in
getchar(); `blocking` tells that it is. After `q` sequences,
`reg_recording()` updates the REC indicator. In the command line (`:`, `/`,
`input()` from a plugin, …) keys are input as typed and the mode is queried
after each one, so the key after `<CR>` is not taken for a command-line key.

### Special Keys (BS, Enter, Commit)

`nvim_get_mode` first: if Neovim is blocked in getchar() the key answers
it. Otherwise one `nvim_buf_get_lines`: if the whole buffer is empty the
key passes through to the app, otherwise BS/Enter go to Neovim and the
commit key commits all lines joined with `\n`, then resets (see Reset)
before any key typed after it. BS/Enter read the current buffer (a
focused floating window's, such as a plugin's prompt); the commit key reads
the attached main buffer.

Non-fast requests (`nvim_buf_get_lines`, `reg_recording()`) don't answer
while Neovim is blocked in getchar(), so they are only sent after
`nvim_get_mode` has shown it is not.

### Key Acknowledgment

Each key carries its `seq` through `KeyProcessed` and `PassthroughKey`.
`Model::reduce(KeyProcessed)` shows the key (outside insert mode, Ctrl/Alt
keys, register names after `<C-r>`) with the modifiers recorded when it was
pressed; a `PassthroughKey` resolves to the keycode and modifiers of its own
key, even if later keys were pressed since.

### Reset

IME off / Deactivate, a commit, and enabling (keymap received) reset
Neovim: normal mode, no floating windows, an empty main buffer, then
insert mode after a commit or when enabling with `startinsert`. A keymap
sent again while enabled leaves Neovim as it is.

A reset is an item in the key queue: it keeps its place after queued keys
(a commit's reset goes ahead of them), is not acknowledged, and re-syncs the
pending state and mode from `nvim_get_mode()`. IME off / Deactivate first
drops keys that have not started. It uses the API rather than keys like
`<Esc>ggdG`, which depend on the user's mappings and on the main window
having the cursor:

1. `nvim_get_mode`; while blocked in getchar() input `<Esc>` (the argument
   of getchar() is not mapped), otherwise `<C-\><C-n>` until normal mode —
   at least once, as a count or operator may be pending. Up to 4 keys; if
   still blocked, give up without the non-fast calls
2. `:fclose!`, `nvim_buf_set_lines` on the main buffer, `:startinsert` —
   each answered before the next request
3. `nvim_get_mode` to re-sync

### nvim_ui_attach Redraw Events

`nvim_ui_attach` with `ext_cmdline`, `ext_popupmenu`, `ext_messages`, `ext_multigrid` extensions, sized to what the popup can show. `redraw.rs` handles:
- `cmdline_show/pos/hide` — command-line display
- `popupmenu_show/select/hide` — completion candidates
- `msg_show/msg_clear` — command output messages
- `mode_change` — mode updates
- grid events (`grid_line`, `win_pos`, `win_float_pos`, `win_viewport`, …) — batched per `flush` into `FromNeovim::GridFlush`, mirrored in `Screen`

Requests from Neovim (none are expected) get an error response, since
Neovim waits for one.

### Buffer Events

`nvim_buf_attach` on the current buffer keeps `BufferMirror` in sync via `nvim_buf_lines_event`; on `nvim_buf_detach_event` (e.g. `:enew` wipes the buffer) jacin re-attaches to the current buffer. IME off commits the mirror without an RPC.

### Process Lifetime

stdout EOF means Neovim exited (e.g. `:q`): the IME is disabled and the next
toggle spawns a new Neovim. On exit jacin sends `qa!`, waits up to 200ms,
then kills it.

## 3. State Model

### ImeMode (Axis 1)

```
           jacin on/toggle        keymap event
 Disabled ──────────> Enabling ──────────────> Enabled         
     ^                                             │
     │        disable() (toggle-off, Neovim exit)  │
     └─────────────────────────────────────────────┘
```

- `jacin toggle|on|off|status` reach the running jacin over a UNIX socket (`$XDG_RUNTIME_DIR/jacin-$WAYLAND_DISPLAY.sock`, one line each way); a second jacin refuses to start while it answers
- Enabling with no text field focused (or in a password field) takes no grab; the grab is taken on the next Activate
- Commit keeps the IME enabled (the buffer is cleared and insert mode restored)
- Deactivate/Activate cycle: Enabled → release grab → re-grab → Enabling → keymap → Enabled (state restored)
- Activate/Deactivate are deferred to the `Done` event and processed together (deactivate first), so a window switch becomes a single release + re-grab
- Password fields: when the `content_type` applied on `Done` has purpose `password`/`pin` or hint `sensitive_data`/`hidden_text`, the grab is released (as on Deactivate) and not taken while the field stays so; the IME stays enabled and re-grabs once the field or its content type changes. The content type that comes with a Deactivate is the old field's and is ignored. The popup is shown only while the grab is held

### Vim mode (Axis 2, observed)

Not tracked by the IME state machine. `NvimView.mode` holds the last mode
Neovim reported, by `mode_change` or an `nvim_get_mode` reply, also while the
IME is disabled (key processing reads it). Both are classified into `Mode`
(`neovim/mode.rs`) at the detail both report — completion (`ic`) is
`Insert`, select mode is `Visual` — and unknown modes are `Other`, which is
never handled fire-and-forget.

### PendingState (Axis 3, per Neovim session)

```
             None
          ╱  │  │  ╲
    Getchar  │  │  (CommandLine:
   (q,f,t,r) │  │   from the mode)
              │  │
     InsertReg│  │Motion (d,c,y, di_)
     (<C-r>)  │
         NormalReg
           (")
```

All pending states resolve back to None after the sequence completes.
Owned by the `KeyProcessor` (recreated on each spawn). The model only sees
it through `FromNeovim::KeyProcessed { seq, pending }`, sent exactly once per
user key after all data messages for that key; `CommandLine` is reported
whenever the mode is the command line.

## 4. Core Design Principles

### Neovim = Single Source of Truth

- IME only observes Neovim (UI events, buffer events, `nvim_buf_get_lines`); it writes the buffer directly only to empty it on a reset
- `Screen` and `BufferMirror` are mirrors of Neovim — never edited locally
- All text manipulation goes through `nvim_input` to preserve undo/redo/macros/plugins

### Reducer + Effects

- `Model::reduce(msg) -> Vec<Effect>` (model.rs) is the only place that updates IME state from Neovim messages; it performs no I/O
- `State::apply_effects` (coordinator.rs) executes effects: commit, popup render, passthrough
- Replay tests feed `FromNeovim` sequences (tests/fixtures) through the real reducer

### Popup-Only Display

- The app gets no preedit content; text appears in the app only on commit. An empty preedit is still sent whenever the IME is enabled, because applications such as Firefox only report the cursor rectangle in response to a preedit update, and the compositor otherwise places the popup at the previously focused field
- UnifiedPopup renders Neovim's window grid — the normal window that last had the cursor, with floats such as nvim-cmp menus overlaid; a float opened with focus keeps the window as the text and gets the cursor — with a thin strip in the mode's color along its left edge (insert blends into the border; the text never moves when the mode changes). Below it, a panel shown only when it has something: the recording indicator (`● @q`), a header row (command line, else `showcmd`), a message row, and the candidates (unnumbered: they are picked with Neovim's keys, not digits)
- While the command line is shown, only its own completion (`popupmenu_show` grid −1) is listed: a buffer's menu can't be used from a prompt such as `confirm()`, whose question and prompt must stay visible
- A candidate is highlighted only when Neovim reports one selected (none with `'completeopt'` noselect)
- A message stays while the command line is shown (it may be a prompt's question) and for 2 seconds after, or 2 seconds otherwise
- Near the bottom of the screen the compositor places the popup above the cursor (`text_input_rectangle` y ≥ 0; y = −height below it). The layout is then mirrored — the text at the bottom next to the cursor, the panel above it growing upwards — so the text doesn't move when the height changes. Only the sign of y is used: the event may describe the previous size

### Wayland-Dependent vs Independent

| Wayland-dependent | Wayland-independent (testable) |
|---|---|
| WaylandState (protocol ops, serial) | ImeState (pure state machine) |
| KeyboardState (XKB keymap) | NvimView, Model (pure logic) |
| Control socket | KeyProcessor, rpc codec, redraw parsing |
| SHM buffer attach/commit | Config, TextRenderer, Layout |

## 5. Multiline

```
Neovim buffer = N lines → app preedit = none → popup = window grid (up to MAX_GRID_ROWS rows, scrolled to the cursor)
```

`<CR>` is a native newline. `<CR>`/`<BS>`/commit key pass through only when the whole buffer is empty. The commit key and IME off commit all lines joined with `\n` (IME off uses `BufferMirror`). Trailing empty lines are kept.

## 6. Design Decisions

### Commit only via an explicit keybind (no `:w` commit)

Committing on `:w` would need a side channel to notice the write: watching a
file (inotify), parsing `msg_show`, a FIFO, or an autocmd calling back over
RPC. A file leaves IME input on disk, the others race with the key flow or
depend on fragile heuristics. The commit key (and IME off) are the only commit
paths, and the buffer keeps `buftype=nofile` so `:q` never hits E37.

### One unified popup

The protocol allows several `zwp_input_popup_surface_v2` surfaces, but the
compositor places each one near the text cursor on its own, so a second
surface (e.g. for candidates) cannot be positioned relative to the first. The
mode strip, window grid and panel share one popup laid out by
`layout.rs`.
