//! Neovim backend handler
//!
//! Runs Neovim in embedded mode as a pure Wayland↔Neovim bridge for input processing.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{error::Error, fmt};

use async_trait::async_trait;
use crossbeam_channel::{Receiver, Sender};
use tokio::runtime::Runtime;

use nvim_rs::create::tokio::new_child_cmd;
use nvim_rs::{Handler, Neovim};
use tokio::process::Command;

use super::protocol::{
    AtomicPendingState, CandidateInfo, FromNeovim, PendingState, PreeditInfo, Snapshot, ToNeovim,
};
use crate::config::Config;

/// Single pending state for multi-key sequences (mutually exclusive).
static PENDING: AtomicPendingState = AtomicPendingState::new();

/// Get a reference to the global pending state.
pub fn pending_state() -> &'static AtomicPendingState {
    &PENDING
}

type NvimWriter = nvim_rs::compat::tokio::Compat<tokio::process::ChildStdin>;
type NvimResult<T> = Result<T, NvimError>;

#[derive(Debug)]
enum NvimError {
    RuntimeInit(std::io::Error),
    Backend(anyhow::Error),
    SnapshotParse(&'static str),
}

impl fmt::Display for NvimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NvimError::RuntimeInit(e) => write!(f, "runtime init failed: {e}"),
            NvimError::Backend(e) => write!(f, "backend error: {e}"),
            NvimError::SnapshotParse(msg) => write!(f, "snapshot parse failed: {msg}"),
        }
    }
}

impl Error for NvimError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            NvimError::RuntimeInit(e) => Some(e),
            NvimError::Backend(e) => Some(e.root_cause()),
            NvimError::SnapshotParse(_) => None,
        }
    }
}

impl From<anyhow::Error> for NvimError {
    fn from(value: anyhow::Error) -> Self {
        NvimError::Backend(value)
    }
}

fn send_msg(tx: &Sender<FromNeovim>, msg: FromNeovim) {
    if let Err(e) = tx.send(msg) {
        log::warn!("[NVIM] Failed to send message to main thread: {}", e);
    }
}

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
                        snapshot.mode,
                        snapshot.preedit
                    );

                    send_msg(&self.tx, FromNeovim::Preedit(snapshot.to_preedit_info()));
                    send_msg(
                        &self.tx,
                        FromNeovim::VisualRange(snapshot.to_visual_selection()),
                    );
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
                        .filter_map(|item| item.as_str().map(std::string::ToString::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let selected = get_i64("selected").unwrap_or(-1);

            if words.is_empty() {
                send_msg(&self.tx, FromNeovim::Candidates(CandidateInfo::empty()));
            } else {
                let sel = selected.max(0) as usize;
                let mut info = CandidateInfo::new(words, sel);
                info.selected = info.selected.min(info.candidates.len().saturating_sub(1));
                send_msg(&self.tx, FromNeovim::Candidates(info));
            }
        } else if name == "ime_auto_commit" {
            if let Some(text) = args.first().and_then(|v| v.as_str()) {
                log::debug!("[NVIM] Auto-commit: {:?}", text);
                send_msg(&self.tx, FromNeovim::AutoCommit(text.to_string()));
            }
        } else if name == "ime_cmdline"
            && let Some(value) = args.first()
            && let Some(map) = value.as_map()
        {
            let get_str = |field: &str| -> Option<String> {
                map.iter()
                    .find(|(k, _)| k.as_str() == Some(field))
                    .and_then(|(_, v)| v.as_str().map(std::string::ToString::to_string))
            };

            match get_str("type").as_deref() {
                Some("update") => {
                    if let Some(text) = get_str("text") {
                        // Set CommandLine pending from the notification side so that
                        // plugin-triggered command-line mode (e.g., input() from
                        // skkeleton dictionary registration) also suppresses the
                        // c-mode recovery in handle_snapshot_response.
                        PENDING.store(PendingState::CommandLine);
                        log::debug!("[NVIM] Cmdline update: {:?}", text);
                        send_msg(&self.tx, FromNeovim::CmdlineUpdate(text));
                    }
                }
                Some("cancelled" | "executed") => {
                    PENDING.clear();
                    log::debug!(
                        "[NVIM] Cmdline left ({})",
                        get_str("type").unwrap_or_default()
                    );
                    send_msg(&self.tx, FromNeovim::CmdlineCancelled);
                }
                Some("message") => {
                    if let Some(text) = get_str("text") {
                        log::debug!("[NVIM] Cmdline message: {:?}", text);
                        send_msg(&self.tx, FromNeovim::CmdlineMessage(text));
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
    let rt = match Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            let err = NvimError::RuntimeInit(e);
            log::error!("[NVIM] {}", err);
            return;
        }
    };
    rt.block_on(async move {
        if let Err(e) = run_neovim(rx, tx, &config).await {
            log::error!("[NVIM] Error: {}", e);
        }
    });
}

async fn run_neovim(
    rx: Receiver<ToNeovim>,
    tx: Sender<FromNeovim>,
    config: &Config,
) -> NvimResult<()> {
    log::info!("[NVIM] Starting Neovim...");

    // Start Neovim in embedded mode
    let mut cmd = Command::new("nvim");
    cmd.args(["--embed", "--headless"]);
    if config.clean {
        cmd.arg("--clean");
    }

    let handler = NvimHandler { tx: tx.clone() };
    let (nvim, io_handler, _child) = new_child_cmd(&mut cmd, handler)
        .await
        .map_err(|e| NvimError::Backend(e.into()))?;

    log::info!("[NVIM] Connected to Neovim");

    // Initialize
    init_neovim(&nvim, config).await.map_err(NvimError::from)?;

    send_msg(&tx, FromNeovim::Ready);

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
            send_msg(&tx, FromNeovim::NvimExited);
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

    // Load Lua modules from embedded files
    nvim.exec_lua(include_str!("lua/snapshot.lua"), vec![])
        .await?;
    nvim.exec_lua(include_str!("lua/key_handlers.lua"), vec![])
        .await?;

    // Set behavior config as Lua globals
    nvim.exec_lua(
        &format!(
            "vim.g.ime_auto_startinsert = {}",
            if config.behavior.auto_startinsert {
                "true"
            } else {
                "false"
            }
        ),
        vec![],
    )
    .await?;

    nvim.exec_lua(include_str!("lua/auto_commit.lua"), vec![])
        .await?;
    nvim.exec_lua(include_str!("lua/autocmds.lua"), vec![])
        .await?;

    // Completion adapter — branch on config
    let use_cmp = config.completion.adapter == "nvim-cmp";
    let completion_lua = if use_cmp {
        include_str!("lua/completion_cmp.lua")
    } else {
        include_str!("lua/completion_native.lua")
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
    // Dispatch through handlers in priority order.
    // Each returns Ok(true) if it fully handled the key.
    if handle_commandline_mode(nvim, key, tx).await?
        || handle_getchar_pending(nvim, key, tx, last_mode).await?
        || handle_commit_key(nvim, key, tx, config, last_mode).await?
        || handle_backspace(nvim, key, tx).await?
        || handle_enter(nvim, key, tx).await?
        || handle_insert_register(nvim, key, tx).await?
        || handle_normal_register(nvim, key, tx).await?
    {
        return Ok(());
    }

    // Register-pending and motion-pending may send the key themselves.
    // `key_sent` tracks whether nvim.input(key) was already called.
    let current = PENDING.load();
    let key_sent;
    if current.is_register() {
        if let Some(handled) = handle_register_pending(nvim, key, tx, current).await? {
            if !handled {
                return Ok(());
            }
            // handled == true means key was sent and register completed; fall through to query
            key_sent = true;
        } else {
            key_sent = false;
        }
    } else {
        key_sent = false;
    }

    if current.is_motion() {
        if !handle_motion_pending(nvim, key, tx, current).await? {
            return Ok(());
        }
        // Motion completed — fall through to query snapshot
    } else if !key_sent {
        let _ = nvim.input(key).await;
    }

    // Insert mode fire-and-forget: autocmd will push snapshot via rpcnotify.
    // Exception: Escape changes mode but no insert-mode autocmd fires after it.
    if last_mode.as_str() == "i" && key != "<Esc>" && key != "<C-c>" {
        if matches!(key, "<C-k>" | "<C-v>" | "<C-q>") && is_blocked(nvim).await? {
            PENDING.store(PendingState::Getchar);
            log::debug!("[NVIM] Insert-mode key {} triggered blocking state", key);
        }
        send_msg(tx, FromNeovim::KeyProcessed);
        return Ok(());
    }

    // ":" in normal mode enters command-line mode.
    if key == ":" && last_mode.as_str() == "n" {
        PENDING.store(PendingState::CommandLine);
        log::debug!("[NVIM] Entered command-line mode");
        send_msg(tx, FromNeovim::CmdlineUpdate(":".to_string()));
        return Ok(());
    }

    // Check blocking before querying snapshot.
    if is_blocked(nvim).await? {
        PENDING.store(PendingState::Getchar);
        log::debug!("[NVIM] Blocked in getchar, waiting for next key");
        send_msg(tx, FromNeovim::KeyProcessed);
        return Ok(());
    }

    handle_snapshot_response(nvim, tx, last_mode).await
}

// --- Sub-handlers: each returns Ok(true) when it fully handled the key ---

/// Forward key in command-line mode (display comes via CmdlineChanged autocmd).
async fn handle_commandline_mode(
    nvim: &Neovim<NvimWriter>,
    key: &str,
    tx: &Sender<FromNeovim>,
) -> anyhow::Result<bool> {
    if PENDING.load() != PendingState::CommandLine {
        return Ok(false);
    }
    log::debug!("[NVIM] CommandLine mode, forwarding key: {}", key);
    let _ = nvim.input(key).await;
    send_msg(tx, FromNeovim::KeyProcessed);
    Ok(true)
}

/// Complete a getchar-blocking key (q, f, t, r, m, etc.).
async fn handle_getchar_pending(
    nvim: &Neovim<NvimWriter>,
    key: &str,
    tx: &Sender<FromNeovim>,
    last_mode: &mut String,
) -> anyhow::Result<bool> {
    if PENDING.load() != PendingState::Getchar {
        return Ok(false);
    }
    log::debug!("[NVIM] Completing getchar with key: {}", key);
    let _ = nvim.input(key).await;
    PENDING.clear();
    if is_blocked(nvim).await? {
        PENDING.store(PendingState::Getchar);
        log::debug!("[NVIM] Still blocked in getchar after key: {}", key);
        send_msg(tx, FromNeovim::KeyProcessed);
        return Ok(true);
    }
    let snapshot = query_snapshot(nvim, tx).await?;
    *last_mode = snapshot.mode.clone();
    if snapshot.mode.starts_with("no") {
        PENDING.store(PendingState::Motion);
        log::debug!(
            "[NVIM] Getchar completed into operator-pending mode ({})",
            snapshot.mode
        );
    }
    Ok(true)
}

/// Handle commit key (default: Ctrl+Enter). Skip if motion-pending (exec_lua would deadlock).
async fn handle_commit_key(
    nvim: &Neovim<NvimWriter>,
    key: &str,
    tx: &Sender<FromNeovim>,
    config: &Config,
    last_mode: &mut String,
) -> anyhow::Result<bool> {
    let pending = PENDING.load();
    if key != config.keybinds.commit || pending.is_motion() || pending.is_register() {
        return Ok(false);
    }
    let result = nvim.exec_lua("return ime_handle_commit()", vec![]).await?;
    if get_map_str(&result, "type") == Some("commit") {
        if let Some(text) = get_map_str(&result, "text") {
            send_msg(tx, FromNeovim::Commit(text.to_string()));
        }
        send_msg(tx, FromNeovim::Preedit(PreeditInfo::empty()));
    } else {
        // Empty buffer — passthrough so the app receives the key (e.g., Ctrl+Enter to send)
        send_msg(tx, FromNeovim::PassthroughKey);
    }
    *last_mode = String::from("i");
    Ok(true)
}

/// Handle Backspace — detect empty buffer for DeleteSurrounding. Skip if motion-pending.
async fn handle_backspace(
    nvim: &Neovim<NvimWriter>,
    key: &str,
    tx: &Sender<FromNeovim>,
) -> anyhow::Result<bool> {
    let pending = PENDING.load();
    if key != "<BS>" || pending.is_motion() || pending.is_register() {
        return Ok(false);
    }
    let result = nvim.exec_lua("return ime_handle_bs()", vec![]).await?;
    if get_map_str(&result, "type") == Some("delete_surrounding") {
        send_msg(
            tx,
            FromNeovim::DeleteSurrounding {
                before: 1,
                after: 0,
            },
        );
    } else {
        send_msg(tx, FromNeovim::KeyProcessed);
    }
    Ok(true)
}

/// Handle Enter — detect empty buffer for passthrough. Skip if motion/register pending.
async fn handle_enter(
    nvim: &Neovim<NvimWriter>,
    key: &str,
    tx: &Sender<FromNeovim>,
) -> anyhow::Result<bool> {
    let pending = PENDING.load();
    if !matches!(key, "<CR>" | "<C-CR>" | "<A-CR>") || pending.is_motion() || pending.is_register()
    {
        return Ok(false);
    }
    let result = nvim.exec_lua("return ime_handle_enter()", vec![]).await?;
    if get_map_str(&result, "type") == Some("passthrough") {
        send_msg(tx, FromNeovim::PassthroughKey);
    } else {
        send_msg(tx, FromNeovim::KeyProcessed);
    }
    Ok(true)
}

/// Handle <C-r> in insert mode — enter register-paste pending state.
async fn handle_insert_register(
    nvim: &Neovim<NvimWriter>,
    key: &str,
    tx: &Sender<FromNeovim>,
) -> anyhow::Result<bool> {
    if key != "<C-r>" || PENDING.load().is_pending() {
        return Ok(false);
    }
    let mode_str = nvim.command_output("echo mode(1)").await?;
    if mode_str.trim() != "i" {
        return Ok(false);
    }
    let _ = nvim.input(key).await;
    PENDING.store(PendingState::InsertRegister);
    log::debug!("[NVIM] Sent <C-r>, waiting for register name (insert mode)");
    send_msg(tx, FromNeovim::KeyProcessed);
    Ok(true)
}

/// Handle " in normal/visual mode — enter register-prefix pending state.
async fn handle_normal_register(
    nvim: &Neovim<NvimWriter>,
    key: &str,
    tx: &Sender<FromNeovim>,
) -> anyhow::Result<bool> {
    if key != "\"" || PENDING.load().is_pending() {
        return Ok(false);
    }
    let mode_str = nvim.command_output("echo mode(1)").await?;
    let mode = mode_str.trim();
    if mode != "n" && !mode.starts_with('v') {
        return Ok(false);
    }
    let _ = nvim.input(key).await;
    PENDING.store(PendingState::NormalRegister);
    log::debug!("[NVIM] Sent \", waiting for register name ({} mode)", mode);
    send_msg(tx, FromNeovim::KeyProcessed);
    Ok(true)
}

/// Handle register-pending: complete <C-r>+reg or "+reg sequences.
/// Returns `Some(true)` = key sent & register completed (fall through to query),
/// `Some(false)` = fully handled (caller should return),
/// `None` = not in register-pending (caller continues).
async fn handle_register_pending(
    nvim: &Neovim<NvimWriter>,
    key: &str,
    tx: &Sender<FromNeovim>,
    current: PendingState,
) -> anyhow::Result<Option<bool>> {
    if !current.is_register() {
        return Ok(None);
    }
    log::debug!(
        "[NVIM] In register-pending (state={:?}), sending: {}",
        current,
        key
    );
    let _ = nvim.input(key).await;

    if current == PendingState::InsertRegister {
        if key == "<C-r>" {
            // <C-r><C-r> = insert register literally — still waiting for name
            log::debug!("[NVIM] Literal register insert mode, still waiting for register name");
            send_msg(tx, FromNeovim::KeyProcessed);
            return Ok(Some(false));
        }
        PENDING.clear();
        Ok(Some(true)) // Paste done, fall through to query preedit
    } else {
        // Normal mode " — register selected, waiting for operator
        PENDING.clear();
        log::debug!("[NVIM] Register '{}' selected, waiting for operator", key);
        send_msg(tx, FromNeovim::KeyProcessed);
        Ok(Some(false))
    }
}

/// Handle motion-pending: advance operator-pending state machine.
/// Returns `true` if motion completed (fall through to snapshot query),
/// `false` if still pending (caller should return).
///
/// Queries Neovim's actual mode after sending the key to determine completion.
/// This correctly handles all motion types including char-search motions
/// (f/t/F/T) that require an argument character.
async fn handle_motion_pending(
    nvim: &Neovim<NvimWriter>,
    key: &str,
    tx: &Sender<FromNeovim>,
    current: PendingState,
) -> anyhow::Result<bool> {
    log::debug!(
        "[NVIM] In operator-pending (state={:?}), sending key: {}",
        current,
        key
    );
    let _ = nvim.input(key).await;

    // Query Neovim's actual mode to determine if the motion completed.
    let mode_info = nvim.get_mode().await?;
    let blocking = mode_info
        .iter()
        .any(|(k, v)| k.as_str() == Some("blocking") && v.as_bool() == Some(true));
    let mode = mode_info
        .iter()
        .find(|(k, _)| k.as_str() == Some("mode"))
        .and_then(|(_, v)| v.as_str())
        .unwrap_or("n");

    if blocking || mode.starts_with("no") {
        // Still pending: either blocked in getchar (e.g., f/t waiting for char)
        // or still in operator-pending (e.g., "di" waiting for text object name).
        send_msg(tx, FromNeovim::KeyProcessed);
        return Ok(false);
    }

    // Motion completed (mode is now n, i, v, etc.)
    log::debug!("[NVIM] Motion completed, resuming normal queries");
    PENDING.clear();
    Ok(true)
}

/// Query snapshot and handle post-key mode transitions (operator-pending, command-line recovery).
async fn handle_snapshot_response(
    nvim: &Neovim<NvimWriter>,
    tx: &Sender<FromNeovim>,
    last_mode: &mut String,
) -> anyhow::Result<()> {
    let snapshot = query_snapshot(nvim, tx).await?;
    *last_mode = snapshot.mode.clone();

    if snapshot.mode.starts_with("no") {
        PENDING.store(PendingState::Motion);
        log::debug!("[NVIM] Entered operator-pending mode ({})", snapshot.mode);
        send_msg(tx, FromNeovim::KeyProcessed);
        return Ok(());
    }

    // Unexpected command-line mode (plugin triggered). Escape and restore insert mode.
    if snapshot.mode.starts_with('c') && PENDING.load() != PendingState::CommandLine {
        log::warn!(
            "[NVIM] Unexpected command-line mode ({}), escaping",
            snapshot.mode
        );
        let _ = nvim.input("<C-c>").await;
        nvim.command("startinsert").await?;
        let snapshot = query_snapshot(nvim, tx).await?;
        *last_mode = snapshot.mode.clone();
    }

    send_msg(tx, FromNeovim::KeyProcessed);
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
    let snapshot = parse_snapshot(&result).map_err(|e| anyhow::anyhow!(e.to_string()))?;

    let preedit = snapshot.to_preedit_info();
    log::debug!(
        "[NVIM] snapshot: preedit={:?}, cursor={}..{}, mode={}, blocking={}, visual={:?}..{:?}",
        snapshot.preedit,
        preedit.cursor_begin,
        preedit.cursor_end,
        snapshot.mode,
        snapshot.blocking,
        snapshot.visual_begin,
        snapshot.visual_end
    );

    send_msg(tx, FromNeovim::Preedit(preedit));
    send_msg(tx, FromNeovim::VisualRange(snapshot.to_visual_selection()));

    Ok(snapshot)
}

/// Parse a msgpack Value (Lua table) into a Snapshot struct.
fn parse_snapshot(value: &nvim_rs::Value) -> NvimResult<Snapshot> {
    let map = value
        .as_map()
        .ok_or(NvimError::SnapshotParse("expected map"))?;

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
