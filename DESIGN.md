# Design Document

## 1. Architecture Diagram

### Component Data Flow

```
┌─────────────────────────────────────────────────────────────────────────┐
│                        Wayland Compositor (Hyprland)                    │
│                                                                         │
│  zwp_input_method_v2          zwp_input_popup_surface_v2    wl_shm     │
└──┬──────────┬──────────────────────────▲──────────────────────▲─────────┘
   │Events    │ Requests                 │ surface.commit()    │ buffer
   │          │                          │                     │ attach
   ▼          │                          │                     │
┌──────────────────────────────────────────────────────────────────────────┐
│                     Wayland Frontend Layer                               │
│                                                                          │
│  ┌──────────────────────┐    ┌──────────────────────────────────────┐    │
│  │  Event Dispatch       │    │  WaylandState                       │    │
│  │                       │    │                                      │    │
│  │  Activate ─┐          │    │  input_method: ZwpInputMethodV2     │    │
│  │  Deactivate│          │    │  keyboard_grab: Option<Grab>        │    │
│  │  Done ─────┼──────────┼──▶ │  serial: u32  (Done count)          │    │
│  │  Key ──────┼──┐       │    │                                      │    │
│  │  Modifiers─┼──┤       │    │  set_preedit_string(text, b, e)     │    │
│  │  Keymap ───┼──┤       │    │  commit_string(text)                │    │
│  └────────────┘  │       │    │  commit(serial)                     │    │
│                  │       │    │  delete_surrounding(before, after)   │    │
│                  │       │    │  grab_keyboard() / release()        │    │
│                  │       │    └──────────────────────────────────────┘    │
│                  │       │                                                │
│  SIGUSR1 ────────┼──┐    │    ┌────────────────────────────────┐         │
│  (toggle)        │  │    │    │  KeyboardState                 │         │
│                  │  │    │    │  xkb_context / xkb_state       │         │
│                  │  │    │    │  modifiers (ctrl, alt, shift)   │         │
│                  │  │    │    │  get_key_info(key) -> (sym, utf8)│        │
│                  │  │    │    └────────────────────────────────┘         │
└──────────────────┼──┼────┼──────────────────────────────────────────────┘
                   │  │    │
                   ▼  ▼    ▼
┌──────────────────────────────────────────────────────────────────────────┐
│                     IME Core (State + Coordination)                      │
│                     State -- main thread                                 │
│                     (coordinator.rs, input.rs)                           │
│                                                                          │
│  ┌─────────────────────────┐     ┌────────────────────────────────┐     │
│  │  ImeState               │     │  KeypressState                 │     │
│  │                         │     │                                │     │
│  │  mode: ImeMode          │     │  accumulated_keys: String      │     │
│  │    Disabled             │     │  visible: bool                 │     │
│  │    Enabling{skk_after}  │     │  timeout: Instant              │     │
│  │    Enabled{vim,skk}     │     │  vim_mode: String              │     │
│  │    Disabling            │     │                                │     │
│  │                         │     │  (display timeout: 1.5s)       │     │
│  │  preedit: String        │     └────────────────────────────────┘     │
│  │  cursor_begin: usize    │                                            │
│  │  cursor_end: usize      │     ┌────────────────────────────────┐     │
│  │  candidates: Vec<Str>   │     │  Config                        │     │
│  │  selected_candidate     │     │  toggle_key: "<A-`>"           │     │
│  └─────────────────────────┘     │  commit_key: "<C-CR>"          │     │
│                                  └────────────────────────────────┘     │
│  ┌──────────────────────────────────────────────────────────────────┐   │
│  │  handle_key(key, state)                                          │   │
│  │    1. XKB keysym -> Vim notation conversion                      │   │
│  │    2. Snapshot PendingState                                      │   │
│  │    3. send_to_nvim(key) -> ToNeovim channel                      │   │
│  │    4. wait_for_nvim_response(200ms) <- FromNeovim channel        │   │
│  │    5. handle_nvim_message()                                      │   │
│  │       -> update ImeState.preedit                                 │   │
│  │       -> update WaylandState (set_preedit / commit_string)       │   │
│  │       -> update UI popup                                         │   │
│  └──────────────────────────────────────────────────────────────────┘   │
└──────────┬───────────────────────────────────────────────┬──────────────┘
           │                                               │
           │  ToNeovim::Key(String)                        │  PopupContent
           │  ToNeovim::Shutdown                           │  { preedit, cursor,
           │  (crossbeam bounded ch, cap=64)               │    vim_mode, keypress,
           │                                               │    candidates, selected }
           ▼                                               ▼
┌───────────────────────────────┐    ┌────────────────────────────────────┐
│  Neovim Backend               │    │  UI Layer                          │
│  (separate thread + Tokio)    │    │                                    │
│                               │    │  ┌──────────────────────────┐      │
│  ┌─────────────────────────┐  │    │  │  UnifiedPopup            │      │
│  │  Handler Thread         │  │    │  │                          │      │
│  │                         │  │    │  │  ┌────────────────────┐  │      │
│  │  recv ToNeovim::Key ────┼──┼─┐  │  │  │  Preedit Section   │  │      │
│  │                         │  │ │  │  │  │  line/block cursor  │  │      │
│  │  nvim.input(key)        │  │ │  │  │  │  horizontal scroll  │  │      │
│  │  nvim.get_mode()        │  │ │  │  │  └────────────────────┘  │      │
│  │  nvim.command_output()  │  │ │  │  │  ┌────────────────────┐  │      │
│  │                         │  │ │  │  │  │  Keypress Section   │  │      │
│  │  PendingState logic:    │  │ │  │  │  │  "d$", "di", "<C-r>a"│ │      │
│  │    Getchar / Motion /   │  │ │  │  │  └────────────────────┘  │      │
│  │    TextObject /         │  │ │  │  │  ┌────────────────────┐  │      │
│  │    InsertRegister /     │  │ │  │  │  │  Candidate Section  │  │      │
│  │    NormalRegister       │  │ │  │  │  │  numbered + sel HL  │  │      │
│  │                         │  │ │  │  │  │  scrollbar          │  │      │
│  │  query_and_send_preedit │  │ │  │  │  └────────────────────┘  │      │
│  │    getline('.') -> text │  │ │  │  └──────────────────────────┘      │
│  │    col('.') -> cursor   │  │ │  │                                    │
│  └────────┬────────────────┘  │ │  │  ┌──────────────────────────┐      │
│           │                   │ │  │  │  TextRenderer             │      │
│           │ FromNeovim:       │ │  │  │  fontdue + glyph cache    │      │
│           │   Preedit(info)   │ │  │  │  SHM double buffering     │      │
│           │   Commit(text)    │ │  │  └──────────────────────────┘      │
│           │   Candidates(..)  │ │  │                                    │
│           │   DeleteSurround  │ │  └────────────────────────────────────┘
│           │                   │ │
│           ▼                   │ │
│  (crossbeam bounded ch)       │ │
│                               │ │
│  ┌─────────────────────────┐  │ │
│  │  PENDING: AtomicU8      │◄─┼─┘  atomic read from main thread
│  │  (static, lock-free)    │  │
│  └─────────────────────────┘  │
│                               │
│  ┌─────────────────────────┐  │
│  │  Neovim Process         │  │
│  │  (headless, nvim-rs)    │  │
│  │  + skkeleton plugin     │  │
│  │  + nvim-cmp plugin      │  │
│  └─────────────────────────┘  │
└───────────────────────────────┘
```

### Synchronous Flow per Keystroke

```
Time ->
Main Thread                Handler Thread              Neovim Process
    |                           |                           |
    |  Key event (Wayland)      |                           |
    |-------------------------->|                           |
    |  ToNeovim::Key("a")       |                           |
    |                           |-------------------------->|
    |                           |  nvim.input("a")          |
    |                           |                           |
    |                           |  sleep(5ms) for skkeleton |
    |                           |                           |
    |                           |-------------------------->|
    |                           |  nvim.get_mode()          |
    |                           |<--------------------------|
    |                           |  -> "i" (insert)          |
    |                           |                           |
    |                           |-------------------------->|
    |                           |  getline('.') + col('.')   |
    |                           |<--------------------------|
    |                           |  -> text + cursor pos     |
    |                           |                           |
    |<--------------------------|                           |
    |  FromNeovim::Preedit      |                           |
    |                           |                           |
    |--> update ImeState        |                           |
    |--> set_preedit -> compositor                          |
    |--> update popup (UI)      |                           |
    |                           |                           |
    |  (ready for next key)     |                           |
```

## 2. State Transition Diagrams

### Orthogonal State Decomposition

The design has **3 independent state axes** (plus macros, not yet implemented):

```
+---------------------------------------------------------------------+
|                     Orthogonal State Structure                       |
|                                                                     |
|  Axis 1: ImeMode (exclusive)     Axis 2: VimMode (exclusive)       |
|  +---------------+               +---------------+                  |
|  | Disabled      |               | Insert        |  <- only valid   |
|  | Enabling      |               | Normal        |     when Enabled |
|  | Enabled ------+--- contains ->| Visual        |                  |
|  | Disabling     |               | Op-Pending    |                  |
|  +---------------+               +---------------+                  |
|                                                                     |
|  Axis 3: PendingState (exclusive)  Axis 4: Macro (not implemented) |
|  +---------------+               +---------------+                  |
|  | None          |               | Idle          |                  |
|  | Getchar       |               | Recording     | <- orthogonal   |
|  | Motion        |               | Playing       |    to VimMode   |
|  | TextObject    |               +---------------+                  |
|  | InsertReg     |                                                  |
|  | NormalReg     |               Axis 5: Skkeleton (parallel)      |
|  +---------------+               +---------------+                  |
|                                  | Active        | <- only valid   |
|                                  | Inactive      |    when Enabled |
|                                  +---------------+                  |
+---------------------------------------------------------------------+

Examples of simultaneously valid states:
  ImeMode::Enabled + VimMode::Normal + PendingState::Motion + Skk::Active
  ImeMode::Enabled + VimMode::Insert + PendingState::None + Skk::Active + Macro::Recording
```

### Axis 1: ImeMode Transitions

```
                     SIGUSR1 (toggle on)
                    +----------------------+
                    |                      v
              +----------+          +--------------+
              |          |          |   Enabling    |
              | Disabled |          | {skk_after:   |
              |          |          |   true/false}  |
              +----------+          +------+-------+
                    ^                      | keymap event arrives
                    |                      | complete_enabling()
                    |                      v
              +----------+          +--------------+
              |          |<---------|   Enabled     |
              |Disabling | (normal) | {vim_mode,    |
              |          |          |  skk_active}  |
              +----------+          +--------------+
                    ^                      |
                    |                      |
                    +----------------------+
                     start_disabling()

  Shortcut transition (disable() -- toggle-off, commit):
    Enabled ------------------------------------> Disabled
    (Skips Disabling. Caller handles keyboard release directly.)

  Deactivate/Activate cycle (compositor-driven):
    Enabled -> [compositor: Deactivate] -> release grab -> [compositor: Activate]
    -> Enabling{skk_after: false} -> [keymap] -> Enabled (state restored)

  Forbidden transitions:
    Disabled -> Enabled    (must go through Enabling)
    Enabling -> Disabled   (no way to cancel keymap wait)
    Disabling -> Enabling  (must fully return to Disabled first)
```

### Axis 2: VimMode Transitions (inside ImeMode::Enabled)

```
                         <Esc>
            +----------------------------------+
            |                                  |
            v          i, a, A, o, I, O        |
     +----------+ ----------------------> +----------+
     |          |                         |          |
     |  Normal  | <---------------------- |  Insert  |  <- initial state
     |          |         <Esc>           |          |
     +----------+                         +----------+
       |      ^                                ^
       |      |                                |
   v, V|      | <Esc> or op complete           | op complete (c-family)
       |      |                                |
       v      |         d, y, c, >          +--+
     +----------+ ----------------> +-----------------+
     |          |                   |  OperatorPending |
     |  Visual  |                   |  {operator, await}|
     |          |                   +-----------------+
     +----------+                     |           ^
                                      | i, a      |
                                      v           |
                                   +--+-----------+
                                   | TextObjectChar
                                   | (w,p,",),b,etc)
                                   +---------------

  Command-line mode (intentionally not modeled as VimMode):
    Occurs on Neovim side -> handler detects -> auto-recovery via <C-c> + startinsert
    (Design decision: IME does not have VimMode::Command)
```

### Axis 3: PendingState Transitions (multi-key sequences)

```
                       +-------------------------------------+
                       |                                     |
                       v                                     |
                 +----------+                                |
          +------+   None   |<-----------------+             |
          |      +----------+                  |             |
          |        |  |  |  |                  |             |
          |   q,f,t|  |  |  |"                 |             |
          |   r,m  |  |  |  |(normal)          | resolved    |
          |        |  |  |  |                  | by next key |
          v        |  |  |  v                  |             |
     +----------+  |  |  | +--------------+    |             |
     | Getchar  |--+  |  | |NormalRegister|----+             |
     |(blocking)|     |  | | ("+register) |                  |
     +----------+     |  | +--------------+                  |
       next key->None |  |                                   |
                      |  | d,c,y,>,<                         |
                 <C-r>|  | (operator)                        |
                (ins) |  |                                   |
                      v  v                                   |
               +--------------+     i, a      +--------------+
               |InsertRegister|     +-------->| TextObject    |
               |(<C-r>+reg)  |     |         | (diw, caw etc)|
               +--------------+     |         +------+-------+
                 next key->None+---------+          |
                               | Motion  |<---------+ if incomplete
                               | (d+wait)|  complete->None
                               +---------+
```

### Axis 4: Macro State (design proposal -- not yet implemented)

```
                  q + register
     +----------+ ----------------> +--------------+
     |          |                   |  Recording   |
     |   Idle   |                   |  {register}  |
     |          | <---------------- |              |
     +----------+       q          +--------------+
          |
          | @ + register
          v
     +--------------+
     |   Playing    |
     |  {register,  | ---- done ----> Idle
     |   remaining} |
     +--------------+

  Orthogonality:
    Recording is fully orthogonal to VimMode and PendingState
    (Normal: qa -> Insert: type text -> Normal: q -- spans all modes)

    Playing is orthogonal to all others, with special constraints:
    - Nested playback (@a containing @b) is allowed (recursive)
    - Recording during playback (qa inside @b) should be forbidden (Neovim errors on this too)

  Neovim delegation vs IME management:
    The existing getchar mechanism handles q->register naturally.
    @a playback can be fully delegated to Neovim (though intermediate preedit update
    frequency needs consideration).
```

### Simultaneous State Validity Matrix

```
                    | VimMode | PendingState | Skkeleton | Macro     |
--------------------+---------+--------------+-----------+-----------+
ImeMode::Disabled   |   N/A   |     N/A      |    N/A    |    N/A    |
ImeMode::Enabling   |   N/A   |     N/A      |    N/A    |    N/A    |
ImeMode::Enabled    |  valid  |    valid     |   valid   | valid(TBD)|
ImeMode::Disabling  |  frozen |    frozen    |   frozen  |  frozen   |

VimMode:            |         |              |           |           |
  Insert            |    -    | None/InsReg/ | Active/   | Rec/Play/ |
                    |         | Getchar      | Inactive  | Idle      |
  Normal            |    -    | None/NrmReg/ | Active/   | Rec/Play/ |
                    |         | Motion/TxtOb/| Inactive  | Idle      |
                    |         | Getchar      |           |           |
  Visual            |    -    | None/Motion  | (usually  | Rec/Play  |
                    |         |              |  Active)  |           |
  Op-Pending        |    -    | Motion/TxtOb | (usually  | Rec/Play  |
                    |         |              |  Active)  |           |
```

## 3. Design Principles and Boundaries

### 3.1 Preedit vs IME Internal Buffer

```
+--------------------------------------------------------------+
|  Neovim Buffer (source of truth)                              |
|  getline('.') = "text being composed"                         |
|  col('.') = 10                                                |
|  <- the only authoritative source                             |
+---------------+----------------------------------------------+
                | query_and_send_preedit()
                | (pulled after every keystroke)
                v
+--------------------------------------------------------------+
|  ImeState.preedit (cache / snapshot)                          |
|  text, cursor_begin, cursor_end                               |
|  <- cache of Neovim response; never edited locally            |
+---------------+----------------------+-----------------------+
                |                      |
                v                      v
+----------------------+  +------------------------------------+
|  Wayland set_preedit |  |  UI Popup (self-drawn)              |
|  (shown inline in    |  |  cursor, candidates, mode display   |
|   app via compositor) |  |  <- builds PopupContent from state  |
+----------------------+  +------------------------------------+
```

**Principle: Neovim = Single Source of Truth**

- The IME only **reads** Neovim's buffer content, never **writes** to it directly (only indirect manipulation via `nvim.input()`)
- `ImeState.preedit` is the latest snapshot from Neovim; the IME never independently manipulates the string
- This ensures Neovim's undo/redo, macros, and plugins (skkeleton, nvim-cmp) all function correctly

**Notes for future extensions:**

- For multiline support, change `getline('.')` to `getline(1, '$')` -- the principle of Neovim as truth source remains unchanged
- Line wrapping and display position calculation are **UI Layer responsibilities**, independent of both Neovim and the compositor

### 3.2 IME UI vs Application Drawing Boundary

```
                  Application's world
                  (invisible and uncontrollable from IME)
+-----------------------------------------------------+
|  Text field                                          |
|  "hello world|"  <- committed text + app's cursor    |
|                                                     |
|  Line wrapping, font, text area height               |
|  -> all app-internal, invisible to IME               |
+-----------------------------------------------------+
          | text_input_rectangle (cursor rect only)
          | y=-22, h=22 <- cursor rectangle only
          v
+-----------------------------------------------------+
|  Compositor (Hyprland)                               |
|  Positions popup surface below cursor rectangle      |
|  IME cannot influence placement                      |
+-----------------------------------------------------+
          |
          v
+-----------------------------------------------------+
|  IME UI (UnifiedPopup)                               |
|  Self-drawn to SHM buffer                            |
|  Preedit + keypress + candidates in one surface      |
|                                                     |
|  What we CAN control:                                |
|    - popup width and height                          |
|    - internal layout (section arrangement)           |
|    - font, color, cursor rendering                   |
|    - horizontal scroll (long preedit)                |
|                                                     |
|  What we CANNOT control:                             |
|    - popup absolute position (compositor decides)    |
|    - collision with app's text wrapping              |
|    - app-side preedit display style                  |
+-----------------------------------------------------+
```

**Principle: Design for dual display**

- `set_preedit_string()` is shown inline by the app (app-dependent styling)
- UnifiedPopup is a fully IME-controlled overlay
- Both are **different views of the same data**; the app-side display is beyond IME control

**Multiline extension boundary:**

- `set_preedit_string()` sent to the app should **always remain single-line** (not a protocol constraint, but for app compatibility)
- Multiline display should be realized **only in UnifiedPopup**
- i.e.: preedit sent to app = "current line or summary", IME popup = "full content"

### 3.3 Wayland-Dependent vs Independent Layers

```
+--------------------------------------------------------------+
|              Wayland-Dependent Layer (not portable)            |
|                                                              |
|  WaylandState:                                               |
|    All zwp_input_method_v2 operations                        |
|    Serial management (Done event counting)                   |
|    grab_keyboard / release                                   |
|    zwp_input_popup_surface_v2 (popup positioning)            |
|    wl_shm (buffer sharing)                                   |
|                                                              |
|  KeyboardState:                                              |
|    XKB keymap (received from compositor)                     |
|    wl_keyboard event handling                                |
|                                                              |
|  SIGUSR1 signal (Hyprland keybind integration)               |
|    <- not wlroots-specific, but depends on compositor hook   |
+--------------------------------------------------------------+

+--------------------------------------------------------------+
|              Wayland-Independent Layer (portable / testable)   |
|                                                              |
|  ImeState:                                                   |
|    ImeMode state transitions                                 |
|    VimMode tracking                                          |
|    Preedit / candidates cache                                |
|    <- contains no Wayland concepts                           |
|                                                              |
|  KeypressState:                                              |
|    Key sequence accumulation, display timeout                |
|    <- pure logic                                             |
|                                                              |
|  Neovim Handler:                                             |
|    Key processing, mode detection, preedit retrieval         |
|    PendingState management                                   |
|    <- depends on Neovim RPC, not on Wayland                  |
|                                                              |
|  Config:                                                     |
|    TOML loading, keybind definitions                         |
|    <- fully independent                                      |
|                                                              |
|  TextRenderer:                                               |
|    Font loading, glyph cache, text measurement               |
|    <- only SHM copy is Wayland-dependent                     |
|                                                              |
|  UnifiedPopup:                                               |
|    Layout calculation, rendering logic                        |
|    <- drawing to tiny-skia Pixmap is independent             |
|    <- surface.attach / commit is Wayland-dependent           |
+--------------------------------------------------------------+
```

**Current strengths:**

- `ImeState` imports no Wayland types -- a pure state machine
- `handler.rs` depends only on Neovim RPC, unaware of Wayland
- `Config` is fully independent

**Areas for improvement:**

1. **~~`main.rs::State` is a large God Object (~970 lines)~~** (resolved)
   - Split into: `main.rs` (State struct + event loop), `dispatch.rs` (Wayland dispatch), `input.rs` (key processing), `coordinator.rs` (Neovim responses + toggle + preedit coordination)

2. **`UnifiedPopup` directly owns the wl_surface**
   - Rendering logic (`calculate_layout`, `render_*`) and Wayland surface operations are interleaved
   - Potential split: `PopupRenderer` (Pixmap generation) and `WaylandSurface` (attach/commit) -- would make rendering testable

3. **`PendingState` is a static AtomicU8**
   - Thread-safe but difficult to test or reset
   - Problematic if multiple instances are ever needed
   - Alternative: `Arc<AtomicU8>` shared between handler and main thread

### 3.4 Defenses Against State Explosion

**Current state count:**

```
ImeMode: 4 * VimMode: 4 * PendingState: 6 * Skkeleton: 2
= 192 theoretical combinations (roughly 30 actually valid)
```

Adding macro state (3) would expand this to 576.

**Defense principles:**

1. **Add orthogonal axes, but minimize cross-axis interactions**
   - Macros can be delegated to Neovim -- IME side only needs a "recording display flag"
   - If Neovim's `get_mode()` response includes recording info, no new state axis is needed in IME

2. **Observe Neovim's state for display, but do not replicate it**
   - VimMode is overwritten from `get_mode()` result every time (correct design)
   - PendingState is similarly derived from Neovim responses
   - Maintaining independent state transitions on IME side that mirror Neovim will inevitably diverge

3. **Intentionally do not model command mode**
   - The current "detect and auto-recover" approach is the right call
   - Mixing command-line input into the preedit model would cause complexity explosion
   - If `:` commands are needed in the future, add a **separate UI section (command line)** and isolate as VimMode::Command

4. **Express forbidden states in the type system**
   - `VimMode` being nested inside `ImeMode::Enabled` is good design
   - `PendingState` ideally belongs inside `Enabled` too, but is static due to cross-thread sharing
   - Eliminating invalid combinations at the type level reduces runtime guards

### 3.5 Guidelines for Multiline / Long Line Extension

```
Current: Neovim buffer = 1 line  ->  preedit = 1 line  ->  popup = h-scroll
Future:  Neovim buffer = N lines ->  preedit = 1 line  ->  popup = v+h-scroll
                                      ^
                                    send only current line to app
                                    (maintain compatibility)
```

**Key design decisions:**

| Item | Current | Extension |
|------|---------|-----------|
| Neovim -> IME query | `getline('.')` | `getline(1, '$')` + `line('.')` |
| ImeState.preedit | `String` (1 line) | `Vec<String>` or `String` with newlines |
| Wayland set_preedit | send full text | send current line only |
| Popup rendering | h-scroll only | per-line rendering, add v-scroll |
| Cursor position | byte offset in line | (line number, byte offset) |
| yy, dd, p, P | not supported | work naturally with multiline buffer |

**Impact scope (modules requiring changes):**

- `handler.rs`: `query_and_send_preedit()` -- multiline retrieval
- `protocol.rs`: `PreeditInfo` -- multiline support
- `ime.rs`: `ImeState` -- multiline preedit storage
- `coordinator.rs`: `update_preedit()` -- extract current line for compositor
- `unified_window.rs`: `render_preedit_section()` -- multiline rendering
- `wayland.rs`: no changes needed (interface preserved)
