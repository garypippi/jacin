//! Neovim backend handler
//!
//! Runs Neovim in embedded mode with vim-skkeleton for Japanese input.

use crossbeam_channel::{Receiver, Sender};
use tokio::runtime::Runtime;

use nvim_rs::create::tokio::new_child_cmd;
use nvim_rs::{Handler, Neovim};
use tokio::process::Command;

use super::protocol::{
    AtomicPendingState, CandidateInfo, CmpCandidatesJson, CompleteInfoJson, FromNeovim,
    PendingState, PreeditInfo, ToNeovim,
};
use crate::config::Config;

/// Single pending state for multi-key sequences (mutually exclusive).
static PENDING: AtomicPendingState = AtomicPendingState::new();

/// Get a reference to the global pending state.
pub fn pending_state() -> &'static AtomicPendingState {
    &PENDING
}

/// Empty handler - we don't need to handle notifications for now
#[derive(Clone)]
pub struct NvimHandler;

impl Handler for NvimHandler {
    type Writer = nvim_rs::compat::tokio::Compat<tokio::process::ChildStdin>;
}

type NvimWriter = nvim_rs::compat::tokio::Compat<tokio::process::ChildStdin>;

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

    let handler = NvimHandler;
    let (nvim, _io_handler, _child) = new_child_cmd(&mut cmd, handler).await?;

    eprintln!("[NVIM] Connected to Neovim");

    // Initialize
    init_neovim(&nvim).await?;

    let _ = tx.send(FromNeovim::Ready);

    // Main loop - process messages from IME
    loop {
        match rx.recv() {
            Ok(ToNeovim::Key(key)) => {
                eprintln!("[NVIM] Received key: {:?}", key);
                if let Err(e) = handle_key(&nvim, &key, &tx, config).await {
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

    // Set up autocmd to trigger nvim-cmp completion when skkeleton enters henkan mode
    nvim.command(
        r#"
        augroup IMESkkeletonCandidates
            autocmd!
            autocmd User skkeleton-handled lua << EOF
                vim.defer_fn(function()
                    local status = vim.fn['skkeleton#vim_status']()
                    vim.g.ime_skk_status = status
                    if status == 'henkan' then
                        -- Trigger nvim-cmp completion
                        local ok, cmp = pcall(require, 'cmp')
                        if ok and cmp then
                            cmp.complete()
                        end
                    end
                end, 5)
EOF
        augroup END
    "#,
    )
    .await
    .ok();

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
) -> anyhow::Result<()> {
    // Handle getchar-pending: Neovim is blocked waiting for a character (after q, f, t, r, m, etc.)
    // Send the key to complete the getchar, then fall through to normal query path
    if PENDING.load() == PendingState::Getchar {
        eprintln!("[NVIM] Completing getchar with key: {}", key);
        let _ = nvim.input(key).await;
        PENDING.clear();
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        // Fall through to query preedit/mode normally
        // (key was already sent, skip the normal send path)
        let mode_result = nvim.get_mode().await?;
        let (mode, blocking) = parse_get_mode(&mode_result);
        if blocking {
            // Still blocked (unlikely but handle gracefully)
            PENDING.store(PendingState::Getchar);
            eprintln!("[NVIM] Still blocked in getchar after key: {}", key);
            return Ok(());
        }
        // Query preedit with current mode
        return query_and_send_preedit(nvim, tx, &mode).await;
    }

    // Handle Ctrl+C - clear preedit and reset to insert mode
    if key == "<C-c>" {
        nvim.command("normal! 0D").await?;
        nvim.command("startinsert").await?;
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
        return Ok(());
    }

    // Handle Enter - only pass to neovim if in SKK conversion mode
    if key == "<CR>" {
        let line = nvim.command_output("echo getline('.')").await?;
        let line = line.trim();

        // Only pass Enter to neovim if in conversion mode (has markers)
        if !line.contains('▼') && !line.contains('▽') {
            // No markers - ignore Enter (don't create newlines)
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
            // Give skkeleton time to process the confirmation
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

            // Get updated preedit and cursor position (insert mode after confirm)
            let line = nvim.command_output("echo getline('.')").await?;
            let line = line.trim().to_string();
            let col_str = nvim.command_output("echo col('.')").await?;
            let col: usize = col_str.trim().parse().unwrap_or(1);
            let cursor_pos = col.saturating_sub(1);

            let _ = tx.send(FromNeovim::Preedit(PreeditInfo::new(
                line,
                cursor_pos,
                cursor_pos,
                "i".to_string(),
            )));
            let _ = tx.send(FromNeovim::Candidates(CandidateInfo::empty()));
        }
        // If no cmp visible, just ignore the key
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
        // Small delay to let skkeleton process
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        let result = nvim.command_output("echo skkeleton#is_enabled()").await?;
        eprintln!("[NVIM] skkeleton enabled: {}", result.trim());
        // Clear preedit display
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
                return Ok(());
            }
            // Normal register paste - paste happened, query preedit
            PENDING.clear();
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            true // Key was sent, continue to query preedit
        } else {
            // Normal mode " - register selected, now waiting for operator
            PENDING.clear();
            // Return early - preedit unchanged, next key will be operator
            eprintln!("[NVIM] Register '{}' selected, waiting for operator", key);
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
            // Give vim time to process the complete command
            tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
            // Fall through to normal query path
        } else {
            return Ok(());
        }
    } else if !key_already_sent {
        // Normal path - send key (unless already sent for register paste)
        let _ = nvim.input(key).await;
    }

    // Small delay to let skkeleton process
    tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;

    // Use nvim_get_mode() which works even when Neovim is blocked in getchar
    let mode_result = nvim.get_mode().await?;
    let (mode, blocking) = parse_get_mode(&mode_result);

    if blocking {
        // Neovim is blocked waiting for a character (e.g., after q, f, t, r, m)
        PENDING.store(PendingState::Getchar);
        eprintln!("[NVIM] Blocked in getchar (mode={}), waiting for next key", mode);
        return Ok(());
    }

    if mode.starts_with("no") {
        // Operator-pending mode (no, nov, etc.)
        // Set flag and skip query - vim is waiting for more input
        PENDING.store(PendingState::Motion);
        eprintln!("[NVIM] Entered operator-pending mode ({})", mode);
        return Ok(());
    }

    // Handle unexpected command-line mode (c, cv, ce, cr, etc.)
    // This can happen when skkeleton internals trigger command-line mode
    // (e.g., nested henkan with capital letters). Escape and restore insert mode.
    if mode.starts_with('c') {
        eprintln!("[NVIM] Unexpected command-line mode ({}), escaping", mode);
        let _ = nvim.input("<C-c>").await;
        tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
        nvim.command("startinsert").await?;
        tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
        return query_and_send_preedit(nvim, tx, "i").await;
    }

    query_and_send_preedit(nvim, tx, &mode).await
}

/// Parse nvim_get_mode() result into (mode_string, blocking)
fn parse_get_mode(result: &[(nvim_rs::Value, nvim_rs::Value)]) -> (String, bool) {
    let mut mode = String::from("n");
    let mut blocking = false;
    for (k, v) in result {
        if let Some(key) = k.as_str() {
            match key {
                "mode" => {
                    if let Some(m) = v.as_str() {
                        mode = m.to_string();
                    }
                }
                "blocking" => {
                    blocking = v.as_bool().unwrap_or(false);
                }
                _ => {}
            }
        }
    }
    (mode, blocking)
}

/// Query preedit state and candidates from Neovim and send to IME
async fn query_and_send_preedit(
    nvim: &Neovim<NvimWriter>,
    tx: &Sender<FromNeovim>,
    mode: &str,
) -> anyhow::Result<()> {
    let line = nvim.command_output("echo getline('.')").await?;
    let line = line
        .trim_end_matches('\n')
        .trim_end_matches('\r')
        .to_string();

    let col_str = nvim.command_output("echo col('.')").await?;
    let col: usize = col_str.trim().parse().unwrap_or(1);

    let cursor_begin = col.saturating_sub(1);
    let cursor_end = if mode == "n" || mode == "v" || mode.starts_with('v') {
        let char_len_str = nvim
            .command_output(&format!(
                "echo strlen(matchstr(getline('.'), '.', {}))",
                col - 1
            ))
            .await
            .unwrap_or_default();
        let char_len: usize = char_len_str.trim().parse().unwrap_or(1);
        cursor_begin + char_len.max(1)
    } else {
        cursor_begin
    };

    eprintln!(
        "[NVIM] preedit: {:?}, col: {}, cursor: {}..{}, mode: {}",
        line, col, cursor_begin, cursor_end, mode
    );
    let _ = tx.send(FromNeovim::Preedit(PreeditInfo::new(
        line.clone(),
        cursor_begin,
        cursor_end,
        mode.to_string(),
    )));

    if let Ok(candidates) = get_skkeleton_candidates(nvim, &line).await
        && !candidates.is_empty()
    {
        let _ = tx.send(FromNeovim::Candidates(candidates));
        return Ok(());
    }

    if let Ok(candidates) = get_completion_candidates(nvim).await {
        if !candidates.is_empty() {
            let _ = tx.send(FromNeovim::Candidates(candidates));
        } else {
            let _ = tx.send(FromNeovim::Candidates(CandidateInfo::empty()));
        }
    } else {
        let _ = tx.send(FromNeovim::Candidates(CandidateInfo::empty()));
    }

    Ok(())
}

/// Query nvim-cmp for completion candidates using its Lua API
async fn get_skkeleton_candidates(
    nvim: &Neovim<NvimWriter>,
    _preedit: &str,
) -> anyhow::Result<CandidateInfo> {
    // Use nvim-cmp's Lua API directly
    let result = nvim
        .command_output(
            r#"lua << EOF
            local ok, cmp = pcall(require, 'cmp')
            if not ok then
                print('{"words":[],"selected":-1,"total":0}')
                return
            end

            -- Check if cmp is visible
            if not cmp.visible() then
                print('{"words":[],"selected":-1,"total":0}')
                return
            end

            local all_entries = cmp.get_entries() or {}
            if #all_entries == 0 then
                print('{"words":[],"selected":-1,"total":0}')
                return
            end

            -- Get selected index
            local selected_idx = -1
            local selected_entry = cmp.get_active_entry()
            if selected_entry then
                for i, entry in ipairs(all_entries) do
                    if entry == selected_entry then
                        selected_idx = i - 1
                        break
                    end
                end
            end

            -- Extract words
            local words = {}
            for _, entry in ipairs(all_entries) do
                local word = entry:get_word()
                if word and word ~= '' then
                    table.insert(words, word)
                end
            end

            print(vim.json.encode({words = words, selected = selected_idx, total = #all_entries}))
EOF"#,
        )
        .await
        .unwrap_or_default();

    let result = result.trim();

    if result.starts_with('{')
        && let Ok(parsed) = serde_json::from_str::<CmpCandidatesJson>(result)
    {
        let mut info = parsed.into_candidate_info();
        // Clamp selection to valid range
        if !info.candidates.is_empty() {
            info.selected = info.selected.min(info.candidates.len().saturating_sub(1));
        }
        return Ok(info);
    }

    Ok(CandidateInfo::empty())
}

/// Query nvim-cmp for completion candidates (fallback using pumvisible)
async fn get_completion_candidates(nvim: &Neovim<NvimWriter>) -> anyhow::Result<CandidateInfo> {
    // Check if completion menu is visible
    let pum_visible = nvim.command_output("echo pumvisible()").await?;
    if pum_visible.trim() != "1" {
        return Ok(CandidateInfo::empty());
    }

    // Get completion info using complete_info()
    let info = nvim
        .command_output("echo json_encode(complete_info(['items', 'selected']))")
        .await?;

    // Parse JSON using serde
    if let Ok(parsed) = serde_json::from_str::<CompleteInfoJson>(&info) {
        let candidates = parsed.into_candidate_info();
        eprintln!(
            "[NVIM] Found {} candidates, selected={}",
            candidates.candidates.len(),
            candidates.selected
        );
        return Ok(candidates);
    }

    Ok(CandidateInfo::empty())
}
