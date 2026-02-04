//! Neovim backend for IME
//!
//! Runs Neovim in embedded mode with vim-skkeleton for Japanese input.

use crossbeam_channel::{Receiver, Sender, unbounded};
use std::thread;
use std::time::Duration;

use nvim_rs::create::tokio::new_child_cmd;
use nvim_rs::{Handler, Neovim};
use tokio::process::Command;
use tokio::runtime::Runtime;

/// Messages sent from IME to Neovim
#[derive(Debug)]
pub enum ToNeovim {
    /// Send a key to Neovim (raw key string like "a", "A", "<BS>", "<CR>")
    Key(String),
    /// Shutdown Neovim
    Shutdown,
}

/// Messages sent from Neovim to IME
#[derive(Debug, Clone)]
pub enum FromNeovim {
    /// Preedit text changed
    Preedit(String),
    /// Text should be committed
    Commit(String),
    /// Delete surrounding text (before_length, after_length)
    DeleteSurrounding(u32, u32),
    /// Completion candidates from nvim-cmp
    Candidates(Vec<String>),
    /// Neovim is ready
    Ready,
}

/// Handle to communicate with Neovim backend
pub struct NeovimHandle {
    pub sender: Sender<ToNeovim>,
    pub receiver: Receiver<FromNeovim>,
}

impl NeovimHandle {
    /// Send a key to Neovim
    pub fn send_key(&self, key: &str) {
        let _ = self.sender.send(ToNeovim::Key(key.to_string()));
    }

    /// Try to receive a message from Neovim (non-blocking)
    pub fn try_recv(&self) -> Option<FromNeovim> {
        self.receiver.try_recv().ok()
    }

    /// Receive with timeout
    pub fn recv_timeout(&self, timeout: Duration) -> Option<FromNeovim> {
        self.receiver.recv_timeout(timeout).ok()
    }

    /// Shutdown Neovim
    pub fn shutdown(&self) {
        let _ = self.sender.send(ToNeovim::Shutdown);
    }
}

/// Empty handler - we don't need to handle notifications for now
#[derive(Clone)]
struct NvimHandler;

impl Handler for NvimHandler {
    type Writer = nvim_rs::compat::tokio::Compat<tokio::process::ChildStdin>;
}

/// Spawn Neovim backend in a separate thread
pub fn spawn_neovim() -> anyhow::Result<NeovimHandle> {
    let (to_nvim_tx, to_nvim_rx) = unbounded::<ToNeovim>();
    let (from_nvim_tx, from_nvim_rx) = unbounded::<FromNeovim>();

    thread::spawn(move || {
        let rt = Runtime::new().expect("Failed to create tokio runtime");
        rt.block_on(async move {
            if let Err(e) = run_neovim(to_nvim_rx, from_nvim_tx).await {
                eprintln!("[NVIM] Error: {}", e);
            }
        });
    });

    Ok(NeovimHandle {
        sender: to_nvim_tx,
        receiver: from_nvim_rx,
    })
}

type NvimWriter = nvim_rs::compat::tokio::Compat<tokio::process::ChildStdin>;

async fn run_neovim(rx: Receiver<ToNeovim>, tx: Sender<FromNeovim>) -> anyhow::Result<()> {
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
                if let Err(e) = handle_key(&nvim, &key, &tx).await {
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

    eprintln!("[NVIM] Initialization complete");
    Ok(())
}

async fn handle_key(
    nvim: &Neovim<NvimWriter>,
    key: &str,
    tx: &Sender<FromNeovim>,
) -> anyhow::Result<()> {
    // Ensure we're in insert mode
    nvim.command("startinsert").await?;

    // Handle Enter key specially - commit the current line
    if key == "<CR>" {
        let line = nvim.command_output("echo getline('.')").await?;
        let line = line.trim().to_string();

        if !line.is_empty() {
            let _ = tx.send(FromNeovim::Commit(line));
            // Clear the line for next input
            nvim.command("normal! 0D").await?;
        }
        return Ok(());
    }

    // Handle Escape - clear preedit
    if key == "<Esc>" {
        nvim.command("normal! 0D").await?;
        let _ = tx.send(FromNeovim::Preedit(String::new()));
        return Ok(());
    }

    // Handle Backspace specially - if line is empty, delete from committed text
    if key == "<BS>" {
        let line = nvim.command_output("echo getline('.')").await?;
        let line = line.trim();

        if line.is_empty() {
            // Nothing in preedit, delete from committed text
            let _ = tx.send(FromNeovim::DeleteSurrounding(1, 0));
            return Ok(());
        }
        // Otherwise, let Neovim handle the backspace normally (fall through)
    }

    // Handle Ctrl+J specially - trigger the <Plug>(skkeleton-toggle) mapping
    if key == "<C-j>" {
        eprintln!("[NVIM] Toggling skkeleton via <Plug> mapping...");
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
        let _ = tx.send(FromNeovim::Preedit(String::new()));
        return Ok(());
    }

    // For special keys like <CR>, etc., we need to use \<...> notation in Vimscript
    let vim_key = if key.starts_with('<') && key.ends_with('>') {
        // Convert <CR> to \<CR> for Vimscript interpretation
        format!("\\{}", key)
    } else {
        // Regular characters - escape special chars
        key.replace('\\', "\\\\").replace('"', "\\\"")
    };
    // Use 'm' flag to allow remapping (skkeleton intercepts via mappings)
    let cmd = format!("call feedkeys(\"{}\", 'm')", vim_key);
    nvim.command(&cmd).await?;

    // Small delay to let skkeleton process
    tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;

    // Get the current line content as "preedit"
    let line = nvim.command_output("echo getline('.')").await?;
    let line = line.trim().to_string();

    eprintln!("[NVIM] preedit: {:?}", line);
    let _ = tx.send(FromNeovim::Preedit(line.clone()));

    // Check for SKKeleton candidates first, then fall back to nvim-cmp
    if let Ok(candidates) = get_skkeleton_candidates(nvim, &line).await
        && !candidates.is_empty()
    {
        let _ = tx.send(FromNeovim::Candidates(candidates));
        return Ok(());
    }

    // Check for completion candidates (nvim-cmp)
    if let Ok(candidates) = get_completion_candidates(nvim).await
        && !candidates.is_empty()
    {
        let _ = tx.send(FromNeovim::Candidates(candidates));
    } else {
        // Clear candidates if none available
        let _ = tx.send(FromNeovim::Candidates(vec![]));
    }

    Ok(())
}

/// Query nvim-cmp for completion candidates using its Lua API
async fn get_skkeleton_candidates(
    nvim: &Neovim<NvimWriter>,
    _preedit: &str,
) -> anyhow::Result<Vec<String>> {
    // Use nvim-cmp's Lua API directly - it has its own window system
    let result = nvim
        .command_output(
            r#"lua << EOF
            local ok, cmp = pcall(require, 'cmp')
            if not ok then
                print('[]')
                return
            end

            -- Check if cmp is visible
            local visible = cmp.visible()
            if not visible then
                print('[]')
                return
            end

            -- Get entries from cmp
            local entries = cmp.get_entries()
            if not entries or #entries == 0 then
                print('[]')
                return
            end

            -- Extract words
            local words = {}
            for i, entry in ipairs(entries) do
                if i > 9 then break end
                local word = entry:get_word()
                if word and word ~= '' then
                    table.insert(words, word)
                end
            end

            print(vim.json.encode(words))
EOF"#,
        )
        .await
        .unwrap_or_default();

    let result = result.trim();

    if result.starts_with('[') && result != "[]" {
        let candidates = parse_json_string_array(result);
        if !candidates.is_empty() {
            return Ok(candidates);
        }
    }

    Ok(vec![])
}

/// Parse a simple JSON string array like ["a", "b", "c"]
fn parse_json_string_array(json: &str) -> Vec<String> {
    let mut items = Vec::new();
    let json = json.trim();

    if !json.starts_with('[') {
        return items;
    }

    let mut in_string = false;
    let mut escape = false;
    let mut current = String::new();

    for c in json.chars() {
        if escape {
            current.push(c);
            escape = false;
            continue;
        }

        match c {
            '\\' => escape = true,
            '"' => {
                if in_string {
                    if !current.is_empty() {
                        items.push(current.clone());
                    }
                    current.clear();
                }
                in_string = !in_string;
            }
            _ if in_string => current.push(c),
            _ => {}
        }
    }

    items.truncate(9);
    items
}

/// Query nvim-cmp for completion candidates
async fn get_completion_candidates(nvim: &Neovim<NvimWriter>) -> anyhow::Result<Vec<String>> {
    // Check if completion menu is visible
    let pum_visible = nvim.command_output("echo pumvisible()").await?;
    if pum_visible.trim() != "1" {
        return Ok(vec![]);
    }

    // Get completion info using complete_info()
    // This returns info about the popup menu
    let info = nvim
        .command_output("echo json_encode(complete_info(['items']))")
        .await?;

    // Parse JSON to extract candidate words
    let candidates = parse_completion_items(&info);
    eprintln!("[NVIM] Found {} candidates", candidates.len());

    Ok(candidates)
}

/// Parse completion items from complete_info() JSON output
fn parse_completion_items(json_str: &str) -> Vec<String> {
    // Simple JSON parsing - extract "word" fields from items array
    // Format: {"items":[{"word":"candidate1",...},{"word":"candidate2",...}]}
    let mut candidates = Vec::new();

    // Find items array
    if let Some(items_start) = json_str.find("\"items\":[") {
        let items_section = &json_str[items_start..];
        // Extract each word field
        let mut search_pos = 0;
        while let Some(word_pos) = items_section[search_pos..].find("\"word\":\"") {
            let start = search_pos + word_pos + 8; // skip "word":"
            if let Some(end_pos) = items_section[start..].find('"') {
                let word = &items_section[start..start + end_pos];
                // Unescape basic JSON escapes
                let word = word.replace("\\\"", "\"").replace("\\\\", "\\");
                candidates.push(word);
                search_pos = start + end_pos;
            } else {
                break;
            }
        }
    }

    candidates
}
