//! Neovim backend handler
//!
//! Runs Neovim in embedded mode with vim-skkeleton for Japanese input.

use async_trait::async_trait;
use crossbeam_channel::{Receiver, Sender};
use tokio::runtime::Runtime;

use nvim_rs::create::tokio::new_child_cmd;
use nvim_rs::{Handler, Neovim};
use tokio::process::Command;

use super::protocol::{
    AtomicPendingState, CandidateInfo, FromNeovim, PendingState, PreeditInfo, Snapshot,
    ToNeovim,
};
use crate::config::Config;

/// Single pending state for multi-key sequences (mutually exclusive).
static PENDING: AtomicPendingState = AtomicPendingState::new();

/// Get a reference to the global pending state.
pub fn pending_state() -> &'static AtomicPendingState {
    &PENDING
}

type NvimWriter = nvim_rs::compat::tokio::Compat<tokio::process::ChildStdin>;

/// Handler for Neovim RPC notifications.
/// Receives push notifications (e.g., ime_snapshot from autocmds) and
/// forwards them to the main thread via the tx channel.
#[derive(Clone)]
pub struct NvimHandler {
    tx: Sender<FromNeovim>,
}

#[async_trait]
impl Handler for NvimHandler {
    type Writer = NvimWriter;

    async fn handle_notify(
        &self,
        name: String,
        args: Vec<nvim_rs::Value>,
        _neovim: Neovim<NvimWriter>,
    ) {
        if name == "ime_snapshot"
            && let Some(value) = args.first()
        {
            match parse_snapshot(value) {
                Ok(snapshot) => {
                    eprintln!(
                        "[NVIM] Push snapshot: mode={}, preedit={:?}",
                        snapshot.mode, snapshot.preedit
                    );

                    let cursor_begin = snapshot.cursor_byte.saturating_sub(1);
                    let cursor_end = if snapshot.char_width > 0 {
                        cursor_begin + snapshot.char_width
                    } else {
                        cursor_begin
                    };

                    let _ = self.tx.send(FromNeovim::Preedit(PreeditInfo::new(
                        snapshot.preedit,
                        cursor_begin,
                        cursor_end,
                        snapshot.mode,
                    )));

                    if let Some(candidates) = snapshot.candidates {
                        if candidates.is_empty() {
                            let _ =
                                self.tx.send(FromNeovim::Candidates(CandidateInfo::empty()));
                        } else {
                            let selected = snapshot.selected.unwrap_or(-1).max(0) as usize;
                            let mut info = CandidateInfo::new(candidates, selected);
                            info.selected =
                                info.selected.min(info.candidates.len().saturating_sub(1));
                            let _ = self.tx.send(FromNeovim::Candidates(info));
                        }
                    } else {
                        let _ = self.tx.send(FromNeovim::Candidates(CandidateInfo::empty()));
                    }
                }
                Err(e) => {
                    eprintln!("[NVIM] Failed to parse push snapshot: {}", e);
                }
            }
        }
    }
}

/// Run the Neovim event loop in a blocking manner
pub fn run_blocking(rx: Receiver<ToNeovim>, tx: Sender<FromNeovim>, config: Config) {
    let rt = Runtime::new().expect("Failed to create tokio runtime");
    rt.block_on(async move {
        if let Err(e) = run_neovim(rx, tx, &config).await {
            eprintln!("[NVIM] Error: {}", e);
        }
    });
}

async fn run_neovim(rx: Receiver<ToNeovim>, tx: Sender<FromNeovim>, config: &Config) -> anyhow::Result<()> {
    eprintln!("[NVIM] Starting Neovim...");

    // Start Neovim in embedded mode
    let mut cmd = Command::new("nvim");
    cmd.args(["--embed", "--headless"]);

    let handler = NvimHandler { tx: tx.clone() };
    let (nvim, _io_handler, _child) = new_child_cmd(&mut cmd, handler).await?;

    eprintln!("[NVIM] Connected to Neovim");

    // Initialize
    init_neovim(&nvim).await?;

    let _ = tx.send(FromNeovim::Ready);

    // Track last known vim mode for insert-mode fire-and-forget optimization.
    // Starts as "i" because init_neovim() ends with startinsert.
    let mut last_mode = String::from("i");

    // Main loop - process messages from IME
    loop {
        match rx.recv() {
            Ok(ToNeovim::Key(key)) => {
                eprintln!("[NVIM] Received key: {:?}", key);
                if let Err(e) = handle_key(&nvim, &key, &tx, config, &mut last_mode).await {
                    eprintln!("[NVIM] Key handling error: {}", e);
                }
            }
            Ok(ToNeovim::Shutdown) | Err(_) => {
                eprintln!("[NVIM] Shutting down...");
                let _ = nvim.command("qa!").await;
                break;
            }
        }
    }

    Ok(())
}

async fn init_neovim(nvim: &Neovim<NvimWriter>) -> anyhow::Result<()> {
    eprintln!("[NVIM] Initializing...");

    nvim.command("set nocompatible").await?;
    nvim.command("set encoding=utf-8").await?;

    // Check if user config was loaded
    let rtp = nvim.command_output("echo &runtimepath").await?;
    eprintln!(
        "[NVIM] runtimepath: {}",
        rtp.trim().chars().take(100).collect::<String>()
    );

    // Check if skkeleton is available
    let result = nvim
        .command_output("echo exists('*skkeleton#is_enabled')")
        .await?;
    eprintln!("[NVIM] skkeleton#is_enabled exists: {}", result.trim());

    // List loaded scripts to see what's loaded
    let scripts = nvim.command_output("scriptnames").await?;
    let script_count = scripts.lines().count();
    eprintln!("[NVIM] Loaded scripts: {} files", script_count);

    // Verify <Plug>(skkeleton-toggle) mapping exists
    let mapping = nvim
        .command_output("imap <Plug>(skkeleton-toggle)")
        .await
        .unwrap_or_default();
    eprintln!(
        "[NVIM] skkeleton-toggle mapping: {}",
        mapping.trim().chars().take(60).collect::<String>()
    );

    // List skkeleton functions to find candidate API
    let funcs = nvim
        .command_output("filter /skkeleton/ function")
        .await
        .unwrap_or_default();
    eprintln!(
        "[NVIM] skkeleton functions: {}",
        funcs.lines().take(10).collect::<Vec<_>>().join(", ")
    );

    // Register collect_snapshot() Lua function for consolidated state queries.
    // All calls inside are in-process (vim.fn.* = C function calls, microsecond-level).
    // This replaces multiple separate RPC calls (getline, col, strlen, cmp) with one.
    nvim.exec_lua(
        r#"
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
        "#,
        vec![],
    )
    .await?;

    // Set up autocmds for push notifications.
    // Insert mode state changes are pushed via vim.rpcnotify instead of being polled.
    nvim.exec_lua(
        r#"
        -- Flag to prevent duplicate snapshots when both skkeleton-handled
        -- and TextChangedI/CursorMovedI fire for the same key.
        vim.g.ime_snapshot_sent = false

        -- skkeleton processing complete (fires after Deno IPC finishes)
        vim.api.nvim_create_autocmd('User', {
            pattern = 'skkeleton-handled',
            callback = function()
                vim.defer_fn(function()
                    -- Trigger cmp completion in henkan mode (existing behavior)
                    local status = vim.fn['skkeleton#vim_status']()
                    vim.g.ime_skk_status = status
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
        "#,
        vec![],
    )
    .await?;

    // Start in insert mode
    nvim.command("startinsert").await?;

    eprintln!("[NVIM] Initialization complete");
    Ok(())
}

async fn handle_key(
    nvim: &Neovim<NvimWriter>,
    key: &str,
    tx: &Sender<FromNeovim>,
    config: &Config,
    last_mode: &mut String,
) -> anyhow::Result<()> {
    // Handle getchar-pending: Neovim is blocked waiting for a character (after q, f, t, r, m, etc.)
    // Send the key to complete the getchar, then fall through to normal query path
    if PENDING.load() == PendingState::Getchar {
        eprintln!("[NVIM] Completing getchar with key: {}", key);
        let _ = nvim.input(key).await;
        PENDING.clear();
        // Fall through to query preedit/mode normally
        // (key was already sent, skip the normal send path)
        let snapshot = query_snapshot(nvim, tx).await?;
        *last_mode = snapshot.mode.clone();
        if snapshot.blocking {
            // Still blocked (unlikely but handle gracefully)
            PENDING.store(PendingState::Getchar);
            eprintln!("[NVIM] Still blocked in getchar after key: {}", key);
        }
        return Ok(());
    }

    // Handle Ctrl+C - clear preedit and reset to insert mode
    if key == "<C-c>" {
        nvim.command("normal! 0D").await?;
        nvim.command("startinsert").await?;
        *last_mode = String::from("i");
        let _ = tx.send(FromNeovim::Preedit(PreeditInfo::empty()));
        let _ = tx.send(FromNeovim::Candidates(CandidateInfo::empty()));
        return Ok(());
    }

    // Handle commit key (default: Ctrl+Enter) - commit preedit to application
    if key == config.keybinds.commit {
        let line = nvim.command_output("echo getline('.')").await?;
        let line = line.trim().to_string();

        if !line.is_empty() {
            let _ = tx.send(FromNeovim::Commit(line));
            // Clear the line for next input
            nvim.command("normal! 0D").await?;
            nvim.command("startinsert").await?;
            let _ = tx.send(FromNeovim::Preedit(PreeditInfo::empty()));
        }
        *last_mode = String::from("i");
        return Ok(());
    }

    // Handle Enter - only pass to neovim if in SKK conversion mode
    if key == "<CR>" {
        let line = nvim.command_output("echo getline('.')").await?;
        let line = line.trim();

        // Only pass Enter to neovim if in conversion mode (has markers)
        if !line.contains('▼') && !line.contains('▽') {
            // No markers - ignore Enter (don't create newlines)
            let _ = tx.send(FromNeovim::KeyProcessed);
            return Ok(());
        }
        // Fall through to normal key handling
    }

    // Handle Backspace specially - if line is empty, delete from committed text
    if key == "<BS>" {
        let line = nvim.command_output("echo getline('.')").await?;
        let line = line.trim();

        if line.is_empty() {
            // Nothing in preedit, delete from committed text
            let _ = tx.send(FromNeovim::DeleteSurrounding {
                before: 1,
                after: 0,
            });
            return Ok(());
        }
        // Otherwise, let Neovim handle the backspace normally (fall through)
    }

    // Handle Ctrl+K specially - call cmp.confirm() directly to avoid Vim's digraph mode
    if key == "<C-k>" {
        let result = nvim
            .command_output(
                r#"lua << EOF
                local ok, cmp = pcall(require, 'cmp')
                if ok and cmp.visible() then
                    cmp.confirm({ select = true })
                    print('confirmed')
                else
                    print('no_cmp')
                end
EOF"#,
            )
            .await
            .unwrap_or_default();

        if result.trim() == "confirmed" {
            let snapshot = query_snapshot(nvim, tx).await?;
            *last_mode = snapshot.mode.clone();
        } else {
            let _ = tx.send(FromNeovim::KeyProcessed);
        }
        return Ok(());
    }

    // Handle toggle key - trigger the <Plug>(skkeleton-toggle) mapping
    if key == config.keybinds.toggle {
        eprintln!("[NVIM] Toggling skkeleton via <Plug> mapping...");
        // Ensure we're in insert mode (skkeleton toggle is an insert-mode mapping)
        nvim.command("startinsert").await?;
        // Clear any existing text using Ctrl+U (works in insert mode)
        nvim.command("call feedkeys(\"\\<C-u>\", 'n')").await?;
        // Use 'm' flag to allow remapping (needed for <Plug> to work)
        nvim.command("call feedkeys(\"\\<Plug>(skkeleton-toggle)\", 'm')")
            .await?;
        let result = nvim.command_output("echo skkeleton#is_enabled()").await?;
        eprintln!("[NVIM] skkeleton enabled: {}", result.trim());
        // Clear preedit display
        *last_mode = String::from("i");
        let _ = tx.send(FromNeovim::Preedit(PreeditInfo::empty()));
        return Ok(());
    }

    // Handle Ctrl+R in insert mode - check mode BEFORE sending to avoid hang
    // IMPORTANT: Also check PENDING - if already waiting for something,
    // don't query mode (Neovim may be blocked and will hang)
    if key == "<C-r>" && !PENDING.load().is_pending() {
        let mode_str = nvim.command_output("echo mode(1)").await?;
        let mode = mode_str.trim();
        if mode == "i" {
            // Send <C-r> and set pending register state
            let _ = nvim.input(key).await;
            PENDING.store(PendingState::InsertRegister);
            eprintln!("[NVIM] Sent <C-r>, waiting for register name (insert mode)");
            let _ = tx.send(FromNeovim::KeyProcessed);
            return Ok(());
        }
    }

    // Handle " in normal mode - register prefix for operators like "ay$
    // Skip if pending (we may be waiting for register name after <C-r>)
    if key == "\"" && !PENDING.load().is_pending() {
        let mode_str = nvim.command_output("echo mode(1)").await?;
        let mode = mode_str.trim();
        if mode == "n" {
            // Send " and set pending register state for normal mode
            let _ = nvim.input(key).await;
            PENDING.store(PendingState::NormalRegister);
            eprintln!("[NVIM] Sent \", waiting for register name (normal mode)");
            let _ = tx.send(FromNeovim::KeyProcessed);
            return Ok(());
        }
    }

    // Handle register-pending state
    let current = PENDING.load();
    let key_already_sent = if current.is_register() {
        eprintln!(
            "[NVIM] In register-pending mode (state={:?}), sending register: {}",
            current, key
        );
        let _ = nvim.input(key).await;

        if current == PendingState::InsertRegister {
            // Insert mode <C-r> handling
            if key == "<C-r>" {
                // <C-r><C-r> means "insert register literally" - still waiting for register name
                eprintln!("[NVIM] Literal register insert mode, still waiting for register name");
                let _ = tx.send(FromNeovim::KeyProcessed);
                return Ok(());
            }
            // Normal register paste - paste happened, query preedit
            PENDING.clear();
            true // Key was sent, continue to query preedit
        } else {
            // Normal mode " - register selected, now waiting for operator
            PENDING.clear();
            // Return early - preedit unchanged, next key will be operator
            eprintln!("[NVIM] Register '{}' selected, waiting for operator", key);
            let _ = tx.send(FromNeovim::KeyProcessed);
            return Ok(());
        }
    } else {
        false
    };

    if current.is_motion() {
        eprintln!(
            "[NVIM] In operator-pending mode (state={:?}), sending key: {}",
            current, key
        );
        let _ = nvim.input(key).await;

        // Check if this key completes the motion
        let completes_motion = match current {
            PendingState::Motion => {
                // Waiting for motion - single char motions complete immediately
                // Text objects (i/a) need one more char
                match key {
                    "i" | "a" => {
                        // Text object prefix - wait for one more char
                        PENDING.store(PendingState::TextObject);
                        false
                    }
                    // Single char motions that complete operator
                    "w" | "e" | "b" | "h" | "j" | "k" | "l" | "$" | "0" | "^" | "G" | "{" | "}"
                    | "(" | ")" | "%" => true,
                    // Doubled operators (yy, dd, cc) - operator char repeats to operate on line
                    "y" | "d" | "c" => true,
                    // Escape cancels
                    "<Esc>" => true,
                    _ => false,
                }
            }
            PendingState::TextObject => {
                // Waiting for text object char (w, p, ", ', etc.)
                // Any char completes it
                true
            }
            _ => false,
        };

        if completes_motion {
            eprintln!("[NVIM] Motion completed, resuming normal queries");
            PENDING.clear();
            // Fall through to normal query path
        } else {
            let _ = tx.send(FromNeovim::KeyProcessed);
            return Ok(());
        }
    } else if !key_already_sent {
        // Normal path - send key (unless already sent for register paste)
        let _ = nvim.input(key).await;
    }

    // Insert mode fire-and-forget: autocmd will push snapshot via rpcnotify.
    // Exception: Escape changes mode but no insert-mode autocmd fires after it.
    if last_mode.as_str() == "i" && key != "<Esc>" {
        let _ = tx.send(FromNeovim::KeyProcessed);
        return Ok(());
    }

    // Normal mode or mode-changing keys: query snapshot synchronously.
    // No sleep needed — normal mode operations complete synchronously in Neovim.
    let snapshot = query_snapshot(nvim, tx).await?;
    *last_mode = snapshot.mode.clone();

    if snapshot.blocking {
        // Neovim is blocked waiting for a character (e.g., after q, f, t, r, m)
        PENDING.store(PendingState::Getchar);
        eprintln!(
            "[NVIM] Blocked in getchar (mode={}), waiting for next key",
            snapshot.mode
        );
        return Ok(());
    }

    if snapshot.mode.starts_with("no") {
        // Operator-pending mode (no, nov, etc.)
        // Set flag and skip query - vim is waiting for more input
        PENDING.store(PendingState::Motion);
        eprintln!("[NVIM] Entered operator-pending mode ({})", snapshot.mode);
        return Ok(());
    }

    // Handle unexpected command-line mode (c, cv, ce, cr, etc.)
    // This can happen when skkeleton internals trigger command-line mode
    // (e.g., nested henkan with capital letters). Escape and restore insert mode.
    if snapshot.mode.starts_with('c') {
        eprintln!(
            "[NVIM] Unexpected command-line mode ({}), escaping",
            snapshot.mode
        );
        let _ = nvim.input("<C-c>").await;
        nvim.command("startinsert").await?;
        let snapshot = query_snapshot(nvim, tx).await?;
        *last_mode = snapshot.mode.clone();
        return Ok(());
    }

    Ok(())
}


/// Query full state snapshot from Neovim via collect_snapshot() Lua function.
/// Replaces separate getline/col/strlen/cmp queries with a single RPC call.
async fn query_snapshot(
    nvim: &Neovim<NvimWriter>,
    tx: &Sender<FromNeovim>,
) -> anyhow::Result<Snapshot> {
    let result = nvim.exec_lua("return collect_snapshot()", vec![]).await?;
    let snapshot = parse_snapshot(&result)?;

    let cursor_begin = snapshot.cursor_byte.saturating_sub(1);
    let cursor_end = if snapshot.char_width > 0 {
        cursor_begin + snapshot.char_width
    } else {
        cursor_begin
    };

    eprintln!(
        "[NVIM] snapshot: preedit={:?}, cursor={}..{}, mode={}, blocking={}",
        snapshot.preedit, cursor_begin, cursor_end, snapshot.mode, snapshot.blocking
    );

    let _ = tx.send(FromNeovim::Preedit(PreeditInfo::new(
        snapshot.preedit.clone(),
        cursor_begin,
        cursor_end,
        snapshot.mode.clone(),
    )));

    if let Some(ref candidates) = snapshot.candidates {
        if candidates.is_empty() {
            let _ = tx.send(FromNeovim::Candidates(CandidateInfo::empty()));
        } else {
            let selected = snapshot
                .selected
                .unwrap_or(-1)
                .max(0) as usize;
            let mut info = CandidateInfo::new(candidates.clone(), selected);
            info.selected = info.selected.min(info.candidates.len().saturating_sub(1));
            let _ = tx.send(FromNeovim::Candidates(info));
        }
    } else {
        let _ = tx.send(FromNeovim::Candidates(CandidateInfo::empty()));
    }

    Ok(snapshot)
}

/// Parse a msgpack Value (Lua table) into a Snapshot struct.
fn parse_snapshot(value: &nvim_rs::Value) -> anyhow::Result<Snapshot> {
    let map = value
        .as_map()
        .ok_or_else(|| anyhow::anyhow!("snapshot: expected map"))?;

    let mut snapshot = Snapshot {
        preedit: String::new(),
        cursor_byte: 1,
        mode: "n".to_string(),
        blocking: false,
        char_width: 0,
        candidates: None,
        selected: None,
    };

    for (k, v) in map {
        let Some(key) = k.as_str() else { continue };
        match key {
            "preedit" => {
                snapshot.preedit = v.as_str().unwrap_or("").to_string();
            }
            "cursor_byte" => {
                snapshot.cursor_byte = v.as_u64().unwrap_or(1) as usize;
            }
            "mode" => {
                snapshot.mode = v.as_str().unwrap_or("n").to_string();
            }
            "blocking" => {
                snapshot.blocking = v.as_bool().unwrap_or(false);
            }
            "char_width" => {
                snapshot.char_width = v.as_u64().unwrap_or(0) as usize;
            }
            "candidates" => {
                if let Some(arr) = v.as_array() {
                    let words: Vec<String> = arr
                        .iter()
                        .filter_map(|item| item.as_str().map(|s| s.to_string()))
                        .collect();
                    snapshot.candidates = Some(words);
                }
            }
            "selected" => {
                if let Some(n) = v.as_i64() {
                    snapshot.selected = Some(n as i32);
                }
            }
            _ => {}
        }
    }

    Ok(snapshot)
}
