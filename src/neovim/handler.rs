//! Neovim backend handler
//!
//! Runs Neovim in embedded mode as a pure Wayland↔Neovim bridge for input processing.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use crossbeam_channel::{Receiver, Sender};
use tokio::runtime::Runtime;

use nvim_rs::create::tokio::new_child_cmd;
use nvim_rs::{Handler, Neovim};
use tokio::process::Command;

use super::protocol::{
    AtomicPendingState, CandidateInfo, FromNeovim, PendingState, PreeditInfo, Snapshot, ToNeovim,
    VisualSelection,
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
                    log::debug!(
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
                        snapshot.recording,
                    )));

                    let _ = self.tx.send(FromNeovim::VisualRange(visual));
                }
                Err(e) => {
                    log::error!("[NVIM] Failed to parse push snapshot: {}", e);
                }
            }
        } else if name == "ime_candidates"
            && let Some(value) = args.first()
            && let Some(map) = value.as_map()
        {
            let get_arr = |field: &str| -> Option<&Vec<nvim_rs::Value>> {
                map.iter()
                    .find(|(k, _)| k.as_str() == Some(field))
                    .and_then(|(_, v)| v.as_array())
            };
            let get_i64 = |field: &str| -> Option<i64> {
                map.iter()
                    .find(|(k, _)| k.as_str() == Some(field))
                    .and_then(|(_, v)| v.as_i64())
            };

            let words: Vec<String> = get_arr("candidates")
                .map(|arr| {
                    arr.iter()
                        .filter_map(|item| item.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let selected = get_i64("selected").unwrap_or(-1);

            if words.is_empty() {
                let _ = self.tx.send(FromNeovim::Candidates(CandidateInfo::empty()));
            } else {
                let sel = selected.max(0) as usize;
                let mut info = CandidateInfo::new(words, sel);
                info.selected = info.selected.min(info.candidates.len().saturating_sub(1));
                let _ = self.tx.send(FromNeovim::Candidates(info));
            }
        } else if name == "ime_auto_commit" {
            if let Some(text) = args.first().and_then(|v| v.as_str()) {
                log::debug!("[NVIM] Auto-commit: {:?}", text);
                let _ = self.tx.send(FromNeovim::AutoCommit(text.to_string()));
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
                        log::debug!("[NVIM] Cmdline update: {:?}", text);
                        let _ = self.tx.send(FromNeovim::CmdlineUpdate(text));
                    }
                }
                Some("cancelled") | Some("executed") => {
                    PENDING.clear();
                    log::debug!("[NVIM] Cmdline left ({})", get_str("type").unwrap_or_default());
                    let _ = self.tx.send(FromNeovim::CmdlineCancelled);
                }
                Some("message") => {
                    if let Some(text) = get_str("text") {
                        log::debug!("[NVIM] Cmdline message: {:?}", text);
                        let _ = self.tx.send(FromNeovim::CmdlineMessage(text));
                    }
                }
                other => {
                    log::warn!("[NVIM] Unknown cmdline type: {:?}", other);
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
            log::error!("[NVIM] Error: {}", e);
        }
    });
}

async fn run_neovim(rx: Receiver<ToNeovim>, tx: Sender<FromNeovim>, config: &Config) -> anyhow::Result<()> {
    log::info!("[NVIM] Starting Neovim...");

    // Start Neovim in embedded mode
    let mut cmd = Command::new("nvim");
    cmd.args(["--embed", "--headless"]);
    if config.clean {
        cmd.arg("--clean");
    }

    let handler = NvimHandler { tx: tx.clone() };
    let (nvim, io_handler, _child) = new_child_cmd(&mut cmd, handler).await?;

    log::info!("[NVIM] Connected to Neovim");

    // Initialize
    init_neovim(&nvim, config).await?;

    let _ = tx.send(FromNeovim::Ready);

    // Track whether Neovim has exited (e.g., via :q) to avoid sending qa! to dead process.
    let exited = Arc::new(AtomicBool::new(false));
    {
        let tx = tx.clone();
        let exited = exited.clone();
        tokio::spawn(async move {
            match io_handler.await {
                Ok(Ok(())) => log::info!("[NVIM] I/O loop ended cleanly"),
                Ok(Err(e)) => log::error!("[NVIM] I/O loop error: {}", e),
                Err(e) => log::error!("[NVIM] I/O task panicked: {}", e),
            }
            exited.store(true, Ordering::SeqCst);
            let _ = tx.send(FromNeovim::NvimExited);
        });
    }

    // Track last known vim mode for insert-mode fire-and-forget optimization.
    let mut last_mode = if config.behavior.auto_startinsert {
        String::from("i")
    } else {
        String::from("n")
    };

    // Main loop - process messages from IME
    loop {
        match rx.recv() {
            Ok(ToNeovim::Key(key)) => {
                if exited.load(Ordering::SeqCst) {
                    log::debug!("[NVIM] Ignoring key {:?} — Neovim already exited", key);
                    continue;
                }
                log::debug!("[NVIM] Received key: {:?}", key);
                if let Err(e) = handle_key(&nvim, &key, &tx, config, &mut last_mode).await {
                    log::error!("[NVIM] Key handling error: {}", e);
                }
            }
            Ok(ToNeovim::Shutdown) | Err(_) => {
                log::info!("[NVIM] Shutting down...");
                if !exited.load(Ordering::SeqCst) {
                    let _ = nvim.command("qa!").await;
                }
                break;
            }
        }
    }

    Ok(())
}

async fn init_neovim(nvim: &Neovim<NvimWriter>, config: &Config) -> anyhow::Result<()> {
    log::info!("[NVIM] Initializing...");

    nvim.command("set nocompatible").await?;
    nvim.command("set encoding=utf-8").await?;
    // Disable "-- More --" prompt — in embedded mode nobody can dismiss it,
    // so any long message (e.g. denops error) would block Neovim forever.
    nvim.command("set nomore").await?;
    // Mark buffer as scratch — prevents E37 "No write since last change" on :q
    // bufhidden=wipe cleans up the buffer completely when hidden
    nvim.command("set buftype=nofile bufhidden=wipe").await?;

    // Register collect_snapshot() Lua function for consolidated state queries.
    // All calls inside are in-process (vim.fn.* = C function calls, microsecond-level).
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
                recording = vim.fn.reg_recording(),
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

            return snapshot
        end
        "#,
        vec![],
    )
    .await?;

    // Register special key handlers as Lua functions.
    // Each replaces a multi-RPC Rust handler with a single exec_lua call.
    let use_cmp = config.completion.adapter == "nvim-cmp";

    nvim.exec_lua(
        r#"
        -- Backspace: detect empty buffer for DeleteSurrounding
        function _G.ime_handle_bs()
            local line = vim.fn.getline('.')
            if line == '' then
                return { type = 'delete_surrounding' }
            end
            vim.api.nvim_input('<BS>')
            return { type = 'processing' }
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
        "#,
        vec![],
    )
    .await?;

    // Set behavior config as Lua globals
    nvim.exec_lua(
        &format!(
            "vim.g.ime_auto_startinsert = {}",
            if config.behavior.auto_startinsert { "true" } else { "false" }
        ),
        vec![],
    )
    .await?;

    // Set up line-addition detection for auto-commit.
    // When the buffer goes from 1 to 2+ lines (e.g., <CR>, o, O), the adjacent
    // non-cursor line is committed and deleted, keeping preedit single-line.
    nvim.exec_lua(
        r#"
        _G.ime_context = { last_line_count = 1, clearing = false }

        function _G.check_line_added()
            if ime_context.clearing then return end
            local line_count = vim.fn.line('$')
            if line_count > ime_context.last_line_count then
                -- Line added: commit the adjacent non-cursor line
                local cursor_line = vim.fn.line('.')
                local commit_line = cursor_line > 1 and (cursor_line - 1) or (cursor_line + 1)
                local text = vim.fn.getline(commit_line)
                if text ~= '' then
                    vim.rpcnotify(0, 'ime_auto_commit', text)
                end
                -- Delete the committed line
                ime_context.clearing = true
                vim.o.eventignore = 'all'
                vim.cmd(commit_line .. 'delete _')
                vim.o.eventignore = ''
                ime_context.clearing = false
            end
            ime_context.last_line_count = vim.fn.line('$')
        end
        "#,
        vec![],
    )
    .await?;

    // Set up autocmds for push notifications.
    // Insert mode state changes are pushed via vim.rpcnotify instead of being polled.
    nvim.exec_lua(
        r#"
        -- Detect line addition on insert entry (for o/O from normal mode)
        vim.api.nvim_create_autocmd('ModeChanged', {
            callback = function(args)
                if ime_context.clearing then return end
                local new_mode = args.match:match(':(.+)$')
                if new_mode and new_mode:match('^i') then
                    check_line_added()
                end
            end,
        })

        -- Insert mode changes (text edits, cursor movement)
        vim.api.nvim_create_autocmd({'TextChangedI', 'CursorMovedI'}, {
            callback = function()
                if ime_context.clearing then return end
                check_line_added()
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

        -- Post-command handling
        vim.api.nvim_create_autocmd('CmdlineLeave', {
            callback = function()
                if vim.fn.getcmdtype() ~= ':' then return end
                if vim.v.event.abort then
                    vim.rpcnotify(0, 'ime_cmdline', { type = 'cancelled' })
                else
                    -- Snapshot last message before command executes
                    local old_msg = vim.fn.execute('1messages')
                    vim.rpcnotify(0, 'ime_cmdline', { type = 'executed' })
                    vim.schedule(function()
                        -- Check if command produced a new message
                        local new_msg = vim.fn.execute('1messages')
                        if new_msg ~= old_msg and new_msg ~= '' then
                            local text = vim.trim(new_msg)
                            if text ~= '' then
                                vim.rpcnotify(0, 'ime_cmdline', { type = 'message', text = text })
                            end
                        end
                        if vim.g.ime_auto_startinsert then
                            vim.cmd('startinsert')
                            vim.rpcnotify(0, 'ime_snapshot', collect_snapshot())
                        end
                    end)
                end
            end,
        })
        "#,
        vec![],
    )
    .await?;

    // Completion adapter setup — branch on config.
    let completion_lua = if use_cmp {
        r#"
        local function ime_setup_cmp()
            local ok, cmp = pcall(require, 'cmp')
            if not ok then return false end
            local visible = false
            local last_sel = -1
            local last_count = 0
            local pending = false
            local function send()
                if not cmp.visible() then
                    if visible then
                        visible = false
                        last_sel = -1
                        last_count = 0
                        vim.rpcnotify(0, 'ime_candidates', { candidates = {}, selected = -1 })
                    end
                    return
                end
                visible = true
                local entries = cmp.get_entries() or {}
                -- Find selected index via active entry
                local active = cmp.get_active_entry()
                local sel = -1
                if active then
                    for i, e in ipairs(entries) do
                        if e == active then
                            sel = i - 1
                            break
                        end
                    end
                end
                -- Deduplicate: skip if selection and entry count unchanged
                if sel == last_sel and #entries == last_count then
                    return
                end
                last_sel = sel
                last_count = #entries
                local words = {}
                for _, e in ipairs(entries) do
                    local w = e:get_word()
                    if w and w ~= '' then words[#words + 1] = w end
                end
                vim.rpcnotify(0, 'ime_candidates', {
                    candidates = words,
                    selected = sel,
                })
            end
            local function schedule_send()
                if pending then return end
                pending = true
                vim.schedule(function()
                    pending = false
                    send()
                end)
            end
            cmp.event:on('menu_opened', function()
                last_sel = -1
                last_count = 0
                schedule_send()
            end)
            cmp.event:on('menu_closed', function()
                visible = false
                last_sel = -1
                last_count = 0
                vim.rpcnotify(0, 'ime_candidates', { candidates = {}, selected = -1 })
            end)
            -- Poll after every key to catch selection changes (Ctrl+N/P)
            vim.on_key(function()
                if visible then schedule_send() end
            end)
            return true
        end
        -- Handle lazy-loaded cmp: try now, retry on InsertEnter
        if not ime_setup_cmp() then
            vim.api.nvim_create_autocmd('InsertEnter', {
                once = true,
                callback = function() vim.schedule(ime_setup_cmp) end,
            })
        end
        "#
    } else {
        r#"
        -- Native popup menu: use CompleteChanged/CompleteDone autocmds
        vim.api.nvim_create_autocmd('CompleteChanged', {
            callback = function()
                local info = vim.fn.complete_info({'items', 'selected'})
                local words = {}
                for _, item in ipairs(info.items or {}) do
                    local w = item.word or item.abbr or ''
                    if w ~= '' then words[#words + 1] = w end
                end
                vim.rpcnotify(0, 'ime_candidates', {
                    candidates = words,
                    selected = info.selected,
                })
            end,
        })

        vim.api.nvim_create_autocmd('CompleteDone', {
            callback = function()
                vim.rpcnotify(0, 'ime_candidates', {
                    candidates = {},
                    selected = -1,
                })
            end,
        })
        "#
    };

    nvim.exec_lua(completion_lua, vec![]).await?;

    // Start in insert mode if configured
    if config.behavior.auto_startinsert {
        nvim.command("startinsert").await?;
    }

    log::info!("[NVIM] Initialization complete");
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
        log::debug!("[NVIM] CommandLine mode, forwarding key: {}", key);
        let _ = nvim.input(key).await;
        let _ = tx.send(FromNeovim::KeyProcessed);
        return Ok(());
    }

    // Handle getchar-pending: Neovim is blocked waiting for a character (after q, f, t, r, m, etc.)
    // Send the key to complete the getchar, then check blocking before querying snapshot.
    if PENDING.load() == PendingState::Getchar {
        log::debug!("[NVIM] Completing getchar with key: {}", key);
        let _ = nvim.input(key).await;
        PENDING.clear();
        // nvim_get_mode() is "fast" (works during getchar); exec_lua is NOT and would deadlock.
        if is_blocked(nvim).await? {
            PENDING.store(PendingState::Getchar);
            log::debug!("[NVIM] Still blocked in getchar after key: {}", key);
            let _ = tx.send(FromNeovim::KeyProcessed);
            return Ok(());
        }
        let snapshot = query_snapshot(nvim, tx).await?;
        *last_mode = snapshot.mode.clone();
        return Ok(());
    }

    // Handle commit key (default: Ctrl+Enter) - commit preedit to application (1 RPC)
    // Skip if motion-pending: exec_lua would deadlock while Neovim waits for operator input.
    if key == config.keybinds.commit && !PENDING.load().is_motion() {
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

    // Handle Backspace - detect empty buffer for DeleteSurrounding (1 RPC)
    // Skip if motion-pending: exec_lua would deadlock while Neovim waits for operator input.
    if key == "<BS>" && !PENDING.load().is_motion() {
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
            log::debug!("[NVIM] Sent <C-r>, waiting for register name (insert mode)");
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
            log::debug!("[NVIM] Sent \", waiting for register name ({} mode)", mode);
            let _ = tx.send(FromNeovim::KeyProcessed);
            return Ok(());
        }
    }

    // Handle register-pending state
    let current = PENDING.load();
    let key_already_sent = if current.is_register() {
        log::debug!(
            "[NVIM] In register-pending mode (state={:?}), sending register: {}",
            current, key
        );
        let _ = nvim.input(key).await;

        if current == PendingState::InsertRegister {
            // Insert mode <C-r> handling
            if key == "<C-r>" {
                // <C-r><C-r> means "insert register literally" - still waiting for register name
                log::debug!("[NVIM] Literal register insert mode, still waiting for register name");
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
            log::debug!("[NVIM] Register '{}' selected, waiting for operator", key);
            let _ = tx.send(FromNeovim::KeyProcessed);
            return Ok(());
        }
    } else {
        false
    };

    if current.is_motion() {
        log::debug!(
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
                    // Escape/Backspace cancels
                    "<Esc>" | "<BS>" => true,
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
            log::debug!("[NVIM] Motion completed, resuming normal queries");
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
    if last_mode.as_str() == "i" && key != "<Esc>" && key != "<C-c>" {
        // Some insert-mode keys trigger getchar-blocking state:
        // <C-k> = digraph (waits for 2 chars), <C-v>/<C-q> = literal char input.
        // Detect blocking to prevent subsequent keys from being consumed as arguments.
        if matches!(key, "<C-k>" | "<C-v>" | "<C-q>") && is_blocked(nvim).await? {
            PENDING.store(PendingState::Getchar);
            log::debug!("[NVIM] Insert-mode key {} triggered blocking state", key);
        }
        let _ = tx.send(FromNeovim::KeyProcessed);
        return Ok(());
    }

    // Detect ":" in normal mode — enters command-line mode.
    // Set PENDING to CommandLine and return immediately to avoid query_snapshot
    // which would trigger c-mode recovery.
    if key == ":" && last_mode.as_str() == "n" {
        PENDING.store(PendingState::CommandLine);
        log::debug!("[NVIM] Entered command-line mode");
        let _ = tx.send(FromNeovim::CmdlineUpdate(":".to_string()));
        return Ok(());
    }

    // Normal mode or mode-changing keys: check blocking before querying snapshot.
    // nvim_get_mode() is "fast" (works during getchar); exec_lua would deadlock.
    if is_blocked(nvim).await? {
        PENDING.store(PendingState::Getchar);
        log::debug!("[NVIM] Blocked in getchar, waiting for next key");
        let _ = tx.send(FromNeovim::KeyProcessed);
        return Ok(());
    }

    let snapshot = query_snapshot(nvim, tx).await?;
    *last_mode = snapshot.mode.clone();

    if snapshot.mode.starts_with("no") {
        // Operator-pending mode (no, nov, etc.)
        // Set flag and skip query - vim is waiting for more input
        PENDING.store(PendingState::Motion);
        log::debug!("[NVIM] Entered operator-pending mode ({})", snapshot.mode);
        return Ok(());
    }

    // Handle unexpected command-line mode (c, cv, ce, cr, etc.)
    // This can happen when plugin internals trigger command-line mode
    // (e.g., nested henkan with capital letters). Escape and restore insert mode.
    // Skip if we intentionally entered command mode (PENDING == CommandLine).
    if snapshot.mode.starts_with('c') && PENDING.load() != PendingState::CommandLine {
        log::warn!(
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
/// Replaces separate getline/col/strlen queries with a single RPC call.
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

    log::debug!(
        "[NVIM] snapshot: preedit={:?}, cursor={}..{}, mode={}, blocking={}, visual={:?}..{:?}",
        snapshot.preedit, cursor_begin, cursor_end, snapshot.mode, snapshot.blocking,
        snapshot.visual_begin, snapshot.visual_end
    );

    let _ = tx.send(FromNeovim::Preedit(PreeditInfo::new(
        snapshot.preedit.clone(),
        cursor_begin,
        cursor_end,
        snapshot.mode.clone(),
        snapshot.recording.clone(),
    )));

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
        visual_begin: None,
        visual_end: None,
        recording: String::new(),
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
            "recording" => {
                snapshot.recording = v.as_str().unwrap_or("").to_string();
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
