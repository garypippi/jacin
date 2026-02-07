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
    AtomicPendingState, CandidateInfo, CmdlineAction, FromNeovim, PendingState, PreeditInfo,
    Snapshot, ToNeovim, VisualSelection,
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

                    let visual = snapshot_to_visual_selection(&snapshot);

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

                    let _ = self.tx.send(FromNeovim::VisualRange(visual));
                }
                Err(e) => {
                    eprintln!("[NVIM] Failed to parse push snapshot: {}", e);
                }
            }
        } else if name == "ime_cmdline"
            && let Some(value) = args.first()
            && let Some(map) = value.as_map()
        {
            let get_str = |field: &str| -> Option<String> {
                map.iter()
                    .find(|(k, _)| k.as_str() == Some(field))
                    .and_then(|(_, v)| v.as_str().map(|s| s.to_string()))
            };

            match get_str("type").as_deref() {
                Some("update") => {
                    if let Some(text) = get_str("text") {
                        eprintln!("[NVIM] Cmdline update: {:?}", text);
                        let _ = self.tx.send(FromNeovim::CmdlineUpdate(text));
                    }
                }
                Some("command") => {
                    PENDING.clear();
                    let action = match get_str("action").as_deref() {
                        Some("write") => CmdlineAction::Write,
                        Some("write_quit") => CmdlineAction::WriteQuit,
                        Some("quit") => CmdlineAction::Quit,
                        Some("passthrough") => CmdlineAction::PassThrough,
                        other => {
                            eprintln!("[NVIM] Unknown cmdline action: {:?}", other);
                            return;
                        }
                    };
                    eprintln!("[NVIM] Cmdline command: {:?}", action);
                    let _ = self.tx.send(FromNeovim::CmdlineCommand(action));
                }
                Some("cancelled") => {
                    PENDING.clear();
                    eprintln!("[NVIM] Cmdline cancelled");
                    let _ = self.tx.send(FromNeovim::CmdlineCancelled);
                }
                other => {
                    eprintln!("[NVIM] Unknown cmdline type: {:?}", other);
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
    // Disable "-- More --" prompt — in embedded mode nobody can dismiss it,
    // so any long message (e.g. denops error) would block Neovim forever.
    nvim.command("set nomore").await?;

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

            -- Visual mode: selection range
            if mode.mode:find('^v') or mode.mode == 'V' or mode.mode == '\22' then
                local v_col = vim.fn.getpos('v')[3]
                local sel_start = math.min(v_col, col)
                local sel_end_col = math.max(v_col, col)
                local end_char = vim.fn.matchstr(line, '\\%' .. sel_end_col .. 'c.')
                snapshot.visual_begin = sel_start
                snapshot.visual_end = sel_end_col + vim.fn.strlen(end_char)
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

    // Register special key handlers as Lua functions.
    // Each replaces a multi-RPC Rust handler with a single exec_lua call.
    nvim.exec_lua(
        r#"
        -- Enter: check for henkan markers before passing through
        function _G.ime_handle_enter()
            local line = vim.fn.getline('.')
            if not (line:find('▼') or line:find('▽')) then
                return { type = 'no_marker' }
            end
            vim.api.nvim_input('<CR>')
            return { type = 'processing' }
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
            end
            return { type = 'no_cmp' }
        end

        -- Ctrl+C: clear preedit and reset to insert mode
        function _G.ime_handle_clear()
            vim.cmd('normal! 0D')
            vim.cmd('startinsert')
            return { type = 'cleared' }
        end

        -- Commit: get preedit text, clear buffer, return text for commit
        function _G.ime_handle_commit()
            local line = vim.fn.getline('.')
            if line == '' then
                return { type = 'empty' }
            end
            vim.cmd('normal! 0D')
            vim.cmd('startinsert')
            return { type = 'commit', text = line }
        end

        -- Toggle: switch skkeleton on/off
        function _G.ime_handle_toggle()
            vim.cmd('startinsert')
            local cu = vim.api.nvim_replace_termcodes('<C-u>', true, false, true)
            vim.api.nvim_feedkeys(cu, 'n', false)
            local plug = vim.api.nvim_replace_termcodes('<Plug>(skkeleton-toggle)', true, false, true)
            vim.api.nvim_feedkeys(plug, 'm', false)
            return { type = 'toggled' }
        end
        "#,
        vec![],
    )
    .await?;

    // Set up autocmds for push notifications.
    // Insert mode state changes are pushed via vim.rpcnotify instead of being polled.
    nvim.exec_lua(
        r#"
        -- skkeleton processing complete (fires after Deno IPC finishes)
        vim.api.nvim_create_autocmd('User', {
            pattern = 'skkeleton-handled',
            callback = function()
                -- Trigger cmp completion in henkan mode
                local status = vim.fn['skkeleton#vim_status']()
                vim.g.ime_skk_status = status
                if status == 'henkan' then
                    local ok, cmp = pcall(require, 'cmp')
                    if ok and cmp then
                        cmp.complete()
                    end
                end
                vim.rpcnotify(0, 'ime_snapshot', collect_snapshot())
            end,
        })

        -- Non-skkeleton insert mode changes (direct ASCII, BS, cursor movement)
        vim.api.nvim_create_autocmd({'TextChangedI', 'CursorMovedI'}, {
            callback = function()
                vim.rpcnotify(0, 'ime_snapshot', collect_snapshot())
            end,
        })

        -- Command-line display updates
        vim.api.nvim_create_autocmd('CmdlineChanged', {
            callback = function()
                if vim.fn.getcmdtype() == ':' then
                    vim.rpcnotify(0, 'ime_cmdline', {
                        type = 'update',
                        text = ':' .. vim.fn.getcmdline()
                    })
                end
            end,
        })

        -- Track command action for CmdlineLeave
        vim.g.ime_cmdline_action = nil

        -- Intercept <CR> in command mode
        vim.keymap.set('c', '<CR>', function()
            if vim.fn.getcmdtype() ~= ':' then
                return vim.api.nvim_replace_termcodes('<CR>', true, false, true)
            end
            local cmd = vim.trim(vim.fn.getcmdline())
            if cmd == 'w' then
                vim.g.ime_cmdline_action = 'write'
                return vim.api.nvim_replace_termcodes('<C-c>', true, false, true)
            elseif cmd == 'wq' or cmd == 'x' then
                vim.g.ime_cmdline_action = 'write_quit'
                return vim.api.nvim_replace_termcodes('<C-c>', true, false, true)
            elseif cmd == 'q' or cmd == 'q!' then
                vim.g.ime_cmdline_action = 'quit'
                return vim.api.nvim_replace_termcodes('<C-c>', true, false, true)
            else
                vim.g.ime_cmdline_action = 'passthrough'
                return vim.api.nvim_replace_termcodes('<CR>', true, false, true)
            end
        end, { expr = true })

        -- Post-command handling
        vim.api.nvim_create_autocmd('CmdlineLeave', {
            callback = function()
                if vim.fn.getcmdtype() ~= ':' then return end
                local action = vim.g.ime_cmdline_action
                vim.g.ime_cmdline_action = nil
                if action then
                    vim.rpcnotify(0, 'ime_cmdline', { type = 'command', action = action })
                    if action == 'passthrough' then
                        vim.schedule(function()
                            vim.cmd('startinsert')
                            vim.rpcnotify(0, 'ime_snapshot', collect_snapshot())
                        end)
                    end
                else
                    vim.rpcnotify(0, 'ime_cmdline', { type = 'cancelled' })
                end
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
    // Handle command-line mode: just forward keys, display comes via CmdlineChanged autocmd.
    if PENDING.load() == PendingState::CommandLine {
        eprintln!("[NVIM] CommandLine mode, forwarding key: {}", key);
        let _ = nvim.input(key).await;
        let _ = tx.send(FromNeovim::KeyProcessed);
        return Ok(());
    }

    // Handle getchar-pending: Neovim is blocked waiting for a character (after q, f, t, r, m, etc.)
    // Send the key to complete the getchar, then check blocking before querying snapshot.
    if PENDING.load() == PendingState::Getchar {
        eprintln!("[NVIM] Completing getchar with key: {}", key);
        let _ = nvim.input(key).await;
        PENDING.clear();
        // nvim_get_mode() is "fast" (works during getchar); exec_lua is NOT and would deadlock.
        if is_blocked(nvim).await? {
            PENDING.store(PendingState::Getchar);
            eprintln!("[NVIM] Still blocked in getchar after key: {}", key);
            let _ = tx.send(FromNeovim::KeyProcessed);
            return Ok(());
        }
        let snapshot = query_snapshot(nvim, tx).await?;
        *last_mode = snapshot.mode.clone();
        return Ok(());
    }

    // Handle Ctrl+C - clear preedit and reset to insert mode (1 RPC)
    if key == "<C-c>" {
        let _ = nvim.exec_lua("return ime_handle_clear()", vec![]).await?;
        *last_mode = String::from("i");
        let _ = tx.send(FromNeovim::Preedit(PreeditInfo::empty()));
        let _ = tx.send(FromNeovim::Candidates(CandidateInfo::empty()));
        return Ok(());
    }

    // Handle commit key (default: Ctrl+Enter) - commit preedit to application (1 RPC)
    if key == config.keybinds.commit {
        let result = nvim.exec_lua("return ime_handle_commit()", vec![]).await?;
        if get_map_str(&result, "type") == Some("commit") {
            if let Some(text) = get_map_str(&result, "text") {
                let _ = tx.send(FromNeovim::Commit(text.to_string()));
            }
            let _ = tx.send(FromNeovim::Preedit(PreeditInfo::empty()));
        } else {
            let _ = tx.send(FromNeovim::KeyProcessed);
        }
        *last_mode = String::from("i");
        return Ok(());
    }

    // Handle Enter - only pass to neovim if in SKK conversion mode (1 RPC)
    if key == "<CR>" {
        let _ = nvim.exec_lua("return ime_handle_enter()", vec![]).await?;
        // Both cases (no_marker and processing) are fire-and-forget.
        // If markers present, nvim_input('<CR>') was called in Lua; autocmd pushes snapshot.
        let _ = tx.send(FromNeovim::KeyProcessed);
        return Ok(());
    }

    // Handle Backspace - detect empty buffer for DeleteSurrounding (1 RPC)
    if key == "<BS>" {
        let result = nvim.exec_lua("return ime_handle_bs()", vec![]).await?;
        if get_map_str(&result, "type") == Some("delete_surrounding") {
            let _ = tx.send(FromNeovim::DeleteSurrounding {
                before: 1,
                after: 0,
            });
        } else {
            // nvim_input('<BS>') was called in Lua; autocmd pushes snapshot.
            let _ = tx.send(FromNeovim::KeyProcessed);
        }
        return Ok(());
    }

    // Handle Ctrl+K - confirm cmp completion (1 RPC)
    if key == "<C-k>" {
        let _ = nvim.exec_lua("return ime_handle_confirm()", vec![]).await?;
        // If confirmed, cmp.confirm() ran in Lua; TextChangedI autocmd pushes snapshot.
        // If no cmp visible, nothing happened.
        let _ = tx.send(FromNeovim::KeyProcessed);
        return Ok(());
    }

    // Handle toggle key - trigger the <Plug>(skkeleton-toggle) mapping (1 RPC)
    if key == config.keybinds.toggle {
        eprintln!("[NVIM] Toggling skkeleton via Lua handler...");
        let _ = nvim.exec_lua("return ime_handle_toggle()", vec![]).await?;
        // feedkeys queued in Lua; skkeleton-handled autocmd will push snapshot.
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
        if mode == "n" || mode.starts_with('v') {
            // Send " and set pending register state for normal/visual mode
            let _ = nvim.input(key).await;
            PENDING.store(PendingState::NormalRegister);
            eprintln!("[NVIM] Sent \", waiting for register name ({} mode)", mode);
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

    // Detect ":" in normal mode — enters command-line mode.
    // Set PENDING to CommandLine and return immediately to avoid query_snapshot
    // which would trigger c-mode recovery.
    if key == ":" && last_mode.as_str() == "n" {
        PENDING.store(PendingState::CommandLine);
        eprintln!("[NVIM] Entered command-line mode");
        let _ = tx.send(FromNeovim::CmdlineUpdate(":".to_string()));
        return Ok(());
    }

    // Normal mode or mode-changing keys: check blocking before querying snapshot.
    // nvim_get_mode() is "fast" (works during getchar); exec_lua would deadlock.
    if is_blocked(nvim).await? {
        PENDING.store(PendingState::Getchar);
        eprintln!("[NVIM] Blocked in getchar, waiting for next key");
        let _ = tx.send(FromNeovim::KeyProcessed);
        return Ok(());
    }

    let snapshot = query_snapshot(nvim, tx).await?;
    *last_mode = snapshot.mode.clone();

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
    // Skip if we intentionally entered command mode (PENDING == CommandLine).
    if snapshot.mode.starts_with('c') && PENDING.load() != PendingState::CommandLine {
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


/// Check if Neovim is blocked in getchar() via nvim_get_mode().
/// This is a "fast" API call that works even when Neovim is blocked — unlike
/// exec_lua which would deadlock.
async fn is_blocked(nvim: &Neovim<NvimWriter>) -> anyhow::Result<bool> {
    let mode_info = nvim.get_mode().await?;
    Ok(mode_info
        .iter()
        .any(|(k, v)| k.as_str() == Some("blocking") && v.as_bool() == Some(true)))
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
        "[NVIM] snapshot: preedit={:?}, cursor={}..{}, mode={}, blocking={}, visual={:?}..{:?}",
        snapshot.preedit, cursor_begin, cursor_end, snapshot.mode, snapshot.blocking,
        snapshot.visual_begin, snapshot.visual_end
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

    let visual = snapshot_to_visual_selection(&snapshot);
    let _ = tx.send(FromNeovim::VisualRange(visual));

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
        visual_begin: None,
        visual_end: None,
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
            "visual_begin" => {
                if let Some(n) = v.as_u64() {
                    snapshot.visual_begin = Some(n as usize);
                }
            }
            "visual_end" => {
                if let Some(n) = v.as_u64() {
                    snapshot.visual_end = Some(n as usize);
                }
            }
            _ => {}
        }
    }

    Ok(snapshot)
}

/// Extract a string field from a msgpack map (Lua table return value).
fn get_map_str<'a>(value: &'a nvim_rs::Value, field: &str) -> Option<&'a str> {
    value
        .as_map()?
        .iter()
        .find(|(k, _)| k.as_str() == Some(field))
        .and_then(|(_, v)| v.as_str())
}

/// Convert snapshot visual fields to VisualSelection (1-indexed Lua → 0-indexed byte offsets).
fn snapshot_to_visual_selection(snapshot: &Snapshot) -> Option<VisualSelection> {
    match (snapshot.visual_begin, snapshot.visual_end) {
        (Some(begin), Some(end)) => Some(VisualSelection::Charwise {
            begin: begin.saturating_sub(1),
            end: end.saturating_sub(1),
        }),
        _ => None,
    }
}
