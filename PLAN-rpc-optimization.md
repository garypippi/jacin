# RPC Communication Optimization Plan

## Goal

Reduce per-keystroke RPC round-trips from 5-7 down to 1-2, eliminating fixed sleeps.
Maintain existing separation of concerns (Rust = state machine + Wayland, Neovim = editing + conversion).

---

## Current Problem

### RPC Calls Per Keystroke (handler.rs)

```
[RPC 1] nvim_get_mode()              → PendingState / blocking detection
[RPC 2] nvim_input(key)              → Key injection
        tokio::sleep(5ms)            → Fixed wait for skkeleton (no guarantee)
[RPC 3] getline('.')                 → Preedit text
[RPC 4] col('.')                     → Cursor position
[RPC 5] strlen(matchstr(...))        → (normal mode) char width under cursor
[RPC 6] cmp.visible() / get_entries  → Candidate menu
[RPC 7] pumvisible() / complete_info → Fallback candidates
```

Each RPC costs ~3-5ms (msgpack-rpc socket round-trip). Total: 30-50ms/key.

### Root Cause

handler.rs queries Neovim's internal state **one field at a time** over RPC.
`vim.fn.*` calls inside Neovim are microsecond-level C function calls,
but wrapping each in a separate RPC makes socket overhead dominant.

---

## Design: Mode-Based Communication Strategy

| Mode | Method | RPC Count | Rationale |
|------|--------|-----------|-----------|
| Insert mode (skkeleton active) | Push notification | 1 + push | skkeleton is async (Deno IPC), completion time unknown |
| Insert mode (skkeleton inactive) | Push notification | 1 + push | Unified via TextChangedI/CursorMovedI |
| Normal mode | 2-RPC pull | 2 | Operations are synchronous, query immediately after input |
| Pending state (getchar/motion) | Existing (no change) | 1 | Key injection only, no state query needed |

### Why a Single Lua Function Won't Work

```lua
-- This does NOT work:
function ime_process_key(key)
  vim.api.nvim_input(key)       -- Queued in typeahead buffer
  vim.wait(20, skk_done, 1)     -- Typeahead NOT processed inside RPC handler
  return collect_snapshot()     -- Returns stale (pre-input) state
end
```

Inside `nvim_exec_lua`, `vim.api.nvim_input` puts keys into the typeahead buffer,
but they are not executed until Neovim's state machine (state_enter) resumes
after the RPC handler returns. `vim.wait` processes timers and libuv events
but does not advance the main input loop.

**Therefore: `nvim_input` and state query must be separate RPCs.**

---

## Architecture

### Insert Mode: Push Notification

```
 Rust handler                          Neovim
 ────────────                          ──────
  RPC 1: nvim_input("a")  ──────────→  Queued in typeahead
         (returns immediately)           ↓
                                        RPC complete → main loop resumes
                                         ↓
                                        Typeahead "a" processed
                                         ↓
                                        skkeleton fires (→ Deno IPC)
                                         ↓
                                        Conversion complete
                                         ↓
                                        autocmd fires (skkeleton-handled
                                         or TextChangedI/CursorMovedI)
                                         ↓
                                        Lua callback:
                                          snapshot = collect_snapshot()
                                          vim.rpcnotify(0, "ime_snapshot", snapshot)
                                         ↓
  NvimHandler::handle_notify() ←──────  Push notification
    → tx.send(FromNeovim::Snapshot)
         ↓
  event loop: try_recv()
    → update preedit / candidates / mode
```

**RPC count: 1 (input) + 1 push = effectively 1 round-trip of latency.**

### Normal Mode: 2-RPC Pull

```
 Rust handler                          Neovim
 ────────────                          ──────
  RPC 1: nvim_input("dw")  ─────────→  Queued in typeahead
         (returns immediately)           ↓
                                        Buffer modified (synchronous)
                                         ↓
  RPC 2: nvim_exec_lua(               ← Query snapshot
           "return collect_snapshot()")
         ←──────────────────────────── { preedit, cursor, mode, ... }
```

**RPC count: 2 (no sleep).**

Normal mode operations (d, y, w, motions) complete synchronously in Neovim.
No skkeleton/Deno involvement, so snapshot is available immediately after input.

---

## Neovim-Side Changes

### 1. Lua API: `collect_snapshot()`

Registered via `nvim_exec_lua` during init_neovim(). No external file needed.

```lua
-- collect_snapshot(): Return full state as a single table.
-- All calls inside are in-process (vim.fn.* = C function calls, microsecond-level).
function _G.collect_snapshot()
  local mode = vim.api.nvim_get_mode()
  local line = vim.fn.getline('.')
  local col = vim.fn.col('.')

  local snapshot = {
    preedit = line,
    cursor_byte = col,
    mode = mode.mode,
    blocking = mode.blocking,
    char_width = 0,
    candidates = vim.NIL,  -- msgpack nil
    selected = -1,
  }

  -- Normal/visual mode: character width under cursor
  if mode.mode == 'n' or mode.mode:find('^no') or mode.mode:find('^v') then
    local char = vim.fn.matchstr(line, '\\%' .. col .. 'c.')
    snapshot.char_width = vim.fn.strlen(char)
  end

  -- Candidates (only when cmp is visible)
  local ok, cmp = pcall(require, 'cmp')
  if ok and cmp.visible() then
    local entries = cmp.get_entries() or {}
    local words = {}
    for _, e in ipairs(entries) do
      local w = e:get_word()
      if w and w ~= '' then
        words[#words + 1] = w
      end
    end
    snapshot.candidates = words

    local sel_entry = cmp.get_active_entry()
    if sel_entry then
      for i, e in ipairs(entries) do
        if e == sel_entry then
          snapshot.selected = i - 1
          break
        end
      end
    end
  end

  return snapshot
end
```

### 2. Push Notification Autocmds

```lua
-- Flag to prevent duplicate snapshots when both autocmds fire for same key
vim.g.ime_snapshot_sent = false

-- skkeleton completion (fires after Deno IPC finishes)
vim.api.nvim_create_autocmd('User', {
  pattern = 'skkeleton-handled',
  callback = function()
    vim.defer_fn(function()
      -- Trigger cmp completion in henkan mode (existing behavior)
      local status = vim.fn['skkeleton#vim_status']()
      if status == 'henkan' then
        local ok, cmp = pcall(require, 'cmp')
        if ok and cmp then
          cmp.complete()
        end
      end
      -- Push snapshot (includes candidates from cmp.complete() above)
      vim.g.ime_snapshot_sent = true
      vim.rpcnotify(0, 'ime_snapshot', collect_snapshot())
    end, 5)
  end,
})

-- Non-skkeleton insert mode changes (direct ASCII, BS, cursor movement)
vim.api.nvim_create_autocmd({'TextChangedI', 'CursorMovedI'}, {
  callback = function()
    -- Skip if skkeleton-handled already sent snapshot for this key
    if vim.g.ime_snapshot_sent then
      vim.g.ime_snapshot_sent = false
      return
    end
    vim.rpcnotify(0, 'ime_snapshot', collect_snapshot())
  end,
})
```

### 3. Special Key Handlers (Lua)

```lua
-- Enter: check for henkan markers before passing through
function _G.ime_handle_enter()
  local line = vim.fn.getline('.')
  if not (line:find('▼') or line:find('▽')) then
    return { type = 'no_marker' }  -- Rust decides to ignore
  end
  vim.api.nvim_input('<CR>')
  return { type = 'processing' }
  -- skkeleton-handled autocmd will push snapshot
end

-- Backspace: detect empty buffer for DeleteSurrounding
function _G.ime_handle_bs()
  local line = vim.fn.getline('.')
  if line == '' then
    return { type = 'delete_surrounding' }
  end
  vim.api.nvim_input('<BS>')
  return { type = 'processing' }
end

-- Ctrl+K: confirm cmp completion
function _G.ime_handle_confirm()
  local ok, cmp = pcall(require, 'cmp')
  if ok and cmp.visible() then
    cmp.confirm({ select = true })
    return { type = 'confirmed' }
    -- TextChangedI autocmd will push snapshot
  end
  return { type = 'no_cmp' }
end

-- Ctrl+C: clear preedit and reset
function _G.ime_handle_clear()
  vim.cmd('normal! 0D')
  vim.cmd('startinsert')
  return collect_snapshot()
end
```

---

## Rust-Side Changes

### 1. Add Notification Handler to NvimHandler

```rust
// handler.rs

#[derive(Clone)]
pub struct NvimHandler {
    tx: Sender<FromNeovim>,
}

impl Handler for NvimHandler {
    type Writer = nvim_rs::compat::tokio::Compat<tokio::process::ChildStdin>;

    fn handle_notify(
        &self,
        name: String,
        args: Vec<nvim_rs::Value>,
        _neovim: Neovim<Self::Writer>,
    ) {
        if name == "ime_snapshot" {
            if let Some(snapshot) = parse_snapshot(&args) {
                let _ = self.tx.send(FromNeovim::Snapshot(snapshot));
            }
        }
    }
}
```

### 2. Add Snapshot Type to protocol.rs

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct Snapshot {
    pub preedit: String,
    pub cursor_byte: usize,
    pub mode: String,
    pub blocking: bool,
    pub char_width: usize,
    pub candidates: Option<Vec<String>>,
    pub selected: i32,
}

pub enum FromNeovim {
    Ready,
    Preedit(PreeditInfo),
    Commit(String),
    DeleteSurrounding { before: u32, after: u32 },
    Candidates(CandidateInfo),
    Snapshot(Snapshot),  // New
}
```

### 3. Simplify handle_key in handler.rs

**Before (current): Individual queries after each key**
```rust
async fn handle_key(nvim, key, tx, config) {
    // ... special key handling ...
    nvim.input(key).await;
    tokio::time::sleep(5ms).await;          // Fixed sleep
    let mode = nvim.get_mode().await;       // RPC
    let line = nvim.getline('.').await;      // RPC
    let col = nvim.col('.').await;           // RPC
    let cmp = nvim.cmp_visible().await;     // RPC
    tx.send(Preedit(...));
    tx.send(Candidates(...));
}
```

**After: Insert mode = fire-and-forget, Normal mode = 2-RPC**
```rust
async fn handle_key(nvim, key, tx, config, last_mode: &str) {
    // ... special keys → Lua functions (1 RPC each) ...

    // Pending state handling (unchanged)
    if PENDING.load() == PendingState::Getchar { ... }
    if current.is_motion() { ... }

    // Main path
    nvim.input(key).await;

    if last_mode == "i" {
        // Insert mode: autocmd will push snapshot via rpcnotify.
        // Return immediately, no blocking.
        return Ok(());
    } else {
        // Normal mode: query snapshot synchronously (no sleep needed)
        let result = nvim.exec_lua("return collect_snapshot()", vec![]).await?;
        let snapshot = parse_snapshot_from_lua(&result)?;
        tx.send(FromNeovim::Snapshot(snapshot))?;
    }
}
```

### 4. coordinator.rs: Handle Snapshot Messages

```rust
fn handle_nvim_message(&mut self, msg: FromNeovim) {
    match msg {
        FromNeovim::Snapshot(snap) => {
            let cursor_begin = snap.cursor_byte.saturating_sub(1);
            let cursor_end = if snap.char_width > 0 {
                cursor_begin + snap.char_width
            } else {
                cursor_begin
            };
            self.ime.set_preedit(snap.preedit, cursor_begin, cursor_end);
            self.keypress.set_vim_mode(&snap.mode);
            self.update_preedit();

            if let Some(candidates) = snap.candidates {
                if candidates.is_empty() {
                    self.hide_candidates();
                } else {
                    let selected = snap.selected.max(0) as usize;
                    self.ime.set_candidates(candidates, selected);
                    self.show_candidates();
                }
            } else {
                self.hide_candidates();
            }

            if snap.blocking {
                pending_state().store(PendingState::Getchar);
            }
        }
        // ... existing Preedit, Commit, etc. kept during transition ...
    }
}
```

### 5. input.rs: Remove Blocking Wait for Insert Mode

```rust
fn handle_key(&mut self, key: u32, key_state: wl_keyboard::KeyState) {
    // ... keysym → vim_key conversion ...

    self.send_to_nvim(vim_key);

    if self.keypress.is_insert_mode() {
        // Insert mode: push notification arrives via event loop callback.
        // Do NOT block here.
    } else {
        // Normal mode: wait synchronously for snapshot.
        self.wait_for_nvim_response();
    }
}
```

---

## PendingState Management

PendingState stays in Rust (no change to ownership).

Snapshot's `mode` and `blocking` fields drive Rust-side decisions:
- `blocking == true` → `PendingState::Getchar`
- `mode.starts_with("no")` → `PendingState::Motion`
- `mode.starts_with("c")` → command-line mode recovery

Keys during pending states: `nvim.input(key)` only (no snapshot needed).

---

## Migration Steps

Incremental migration. Each phase independently verifiable.

### Phase 1: Introduce `collect_snapshot()`

- Register `collect_snapshot()` Lua function in init_neovim()
- Replace `query_and_send_preedit` + `get_skkeleton_candidates` + `get_completion_candidates` with single `collect_snapshot()` call
- **RPC pattern unchanged at this phase** (input → sleep → collect_snapshot = 3 RPCs, down from 5-7)
- Verify: all key operations work as before

### Phase 2: Add Notification Infrastructure

- Give `NvimHandler` a `tx: Sender<FromNeovim>` field
- Implement `handle_notify` for `ime_snapshot` notifications
- Add `FromNeovim::Snapshot(Snapshot)` to protocol.rs
- Add `Snapshot` handling to coordinator.rs `handle_nvim_message`
- **Push not yet active** (infrastructure only)

### Phase 3: Enable Push Notifications (Insert Mode)

- Set up autocmds (skkeleton-handled, TextChangedI, CursorMovedI)
- handler.rs: insert mode keys → `nvim_input` then return (remove sleep + query)
- input.rs: skip `wait_for_nvim_response()` for insert mode keys
- **The 5ms fixed sleep is eliminated**
- Verify: Japanese input (skkeleton), ASCII input, BS, cursor movement in insert mode

### Phase 4: Normal Mode 2-RPC

- handler.rs: normal mode keys → `nvim_input` then `collect_snapshot()` (no sleep)
- Remove all `tokio::time::sleep` calls (5ms, 10ms, 20ms)
- Verify: normal mode operations (d, y, w, motions, text objects, registers)

### Phase 5: Special Key Lua Integration

- Move Enter, BS, Ctrl+K, Ctrl+C logic into Lua functions
- Replace handler.rs individual handlers with single-RPC Lua calls
- Verify: each special key works as before

### Phase 6: Cleanup

- Remove `OldFromNeovim` compatibility layer
- Remove `get_skkeleton_candidates` / `get_completion_candidates`
- Remove `query_and_send_preedit`
- Remove all remaining fixed sleeps

---

## Expected Improvement

| Metric | Before | After |
|--------|--------|-------|
| RPC round-trips/key (insert) | 5-7 | 1 + push |
| RPC round-trips/key (normal) | 5-7 | 2 |
| Fixed sleeps | 5-20ms | 0ms |
| Latency/key (insert) | 30-50ms | 5-15ms |
| Latency/key (normal) | 30-50ms | 10-15ms |

## Risks and Considerations

- **Autocmd firing guarantees**: TextChangedI fires after buffer reflects the change,
  but timing relative to `nvim_input` completion needs verification in practice
- **Duplicate notifications**: A single key may fire both skkeleton-handled and TextChangedI.
  The `vim.g.ime_snapshot_sent` flag prevents duplicate snapshots
- **NvimHandler ownership**: Current `NvimHandler` is an empty struct. Adding
  `tx: Sender<FromNeovim>` requires changing how `new_child_cmd` is called
- **Notifications during deactivation**: Autocmds fire inside Neovim regardless of IME state.
  Rust-side `active && is_enabled()` guard remains necessary
- **defer_fn(5ms) in skkeleton-handled**: This small delay (for skkeleton internal state
  to settle) means the push arrives ~5ms after the key. This is still better than
  the current 5ms sleep + 5-7 RPC round-trips
