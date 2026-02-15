//! Neovim backend handler
//!
//! Runs Neovim in embedded mode as a pure Wayland↔Neovim bridge for input processing.

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::{error::Error, fmt};

use async_trait::async_trait;
use crossbeam_channel::{Receiver, Sender};
use tokio::runtime::Runtime;

use nvim_rs::create::tokio::new_child_cmd;
use nvim_rs::{Handler, Neovim, Value};
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
    /// Cached popupmenu items for popupmenu_select (ext_popupmenu).
    last_popupmenu_items: Arc<Mutex<Vec<String>>>,
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
                Some("cancelled" | "executed") => {
                    let event = get_str("type").unwrap_or_default();
                    let cmdtype = get_str("cmdtype").unwrap_or_else(|| ":".to_string());
                    let executed = event == "executed";
                    PENDING.clear();
                    log::debug!("[NVIM] Cmdline left ({}, cmdtype={})", event, cmdtype);
                    send_msg(
                        &self.tx,
                        FromNeovim::CmdlineCancelled { cmdtype, executed },
                    );
                }
                Some("message") => {
                    if let Some(text) = get_str("text") {
                        let cmdtype = get_str("cmdtype").unwrap_or_else(|| ":".to_string());
                        log::debug!("[NVIM] Cmdline message ({}): {:?}", cmdtype, text);
                        send_msg(&self.tx, FromNeovim::CmdlineMessage { text, cmdtype });
                    }
                }
                other => {
                    log::warn!("[NVIM] Unknown cmdline type: {:?}", other);
                }
            }
        } else if name == "redraw" {
            self.handle_redraw(&args);
        }
    }
}

impl NvimHandler {
    /// Parse and dispatch redraw notification events (ext_cmdline, ext_popupmenu).
    fn handle_redraw(&self, args: &[Value]) {
        for event_group in args {
            let Some(arr) = event_group.as_array() else {
                continue;
            };
            let Some(event_name) = arr.first().and_then(|v| v.as_str()) else {
                continue;
            };
            // Each event_group: ["event_name", params_call1, params_call2, ...]
            for params in &arr[1..] {
                match event_name {
                    "cmdline_show" => self.handle_cmdline_show(params),
                    "cmdline_pos" => self.handle_cmdline_pos(params),
                    "cmdline_hide" => self.handle_cmdline_hide(params),
                    "popupmenu_show" => self.handle_popupmenu_show(params),
                    "popupmenu_select" => self.handle_popupmenu_select(params),
                    "popupmenu_hide" => self.handle_popupmenu_hide(),
                    _ => {
                        log::trace!("[NVIM] Ignoring redraw event: {}", event_name);
                    }
                }
            }
        }
    }

    /// cmdline_show: [content, pos, firstc, prompt, indent, level]
    /// content: [[attr_id, text], ...]
    fn handle_cmdline_show(&self, params: &Value) {
        let Some(arr) = params.as_array() else {
            log::debug!("[NVIM] cmdline_show: expected array params");
            return;
        };
        if arr.len() < 6 {
            log::debug!("[NVIM] cmdline_show: expected 6 params, got {}", arr.len());
            return;
        }
        // Parse content: array of [attr_id, text] chunks
        let content = if let Some(chunks) = arr[0].as_array() {
            chunks
                .iter()
                .filter_map(|chunk| {
                    chunk
                        .as_array()
                        .and_then(|c| c.get(1))
                        .and_then(|v| v.as_str())
                })
                .collect::<Vec<&str>>()
                .join("")
        } else {
            String::new()
        };
        let pos = arr[1].as_u64().unwrap_or(0) as usize;
        let firstc = arr[2].as_str().unwrap_or("").to_string();
        let prompt = arr[3].as_str().unwrap_or("").to_string();
        // arr[4] = indent (unused)
        let level = arr[5].as_u64().unwrap_or(1);

        // Set CommandLine pending from the redraw notification side so that
        // plugin-triggered command-line mode (e.g., input() from
        // skkeleton dictionary registration) also suppresses the
        // c-mode recovery in handle_snapshot_response.
        PENDING.store(PendingState::CommandLine);
        log::debug!(
            "[NVIM] cmdline_show: firstc={:?}, prompt={:?}, content={:?}, pos={}, level={}",
            firstc,
            prompt,
            content,
            pos,
            level
        );
        send_msg(
            &self.tx,
            FromNeovim::CmdlineShow {
                content,
                pos,
                firstc,
                prompt,
                level,
            },
        );
    }

    /// cmdline_pos: [pos, level]
    fn handle_cmdline_pos(&self, params: &Value) {
        let Some(arr) = params.as_array() else {
            log::debug!("[NVIM] cmdline_pos: expected array params");
            return;
        };
        if arr.len() < 2 {
            log::debug!("[NVIM] cmdline_pos: expected 2 params, got {}", arr.len());
            return;
        }
        let pos = arr[0].as_u64().unwrap_or(0) as usize;
        let level = arr[1].as_u64().unwrap_or(1);
        log::trace!("[NVIM] cmdline_pos: pos={}, level={}", pos, level);
        send_msg(&self.tx, FromNeovim::CmdlinePos { pos, level });
    }

    /// popupmenu_show: [items, selected, row, col, grid]
    /// items: [[word, kind, menu, info], ...]
    fn handle_popupmenu_show(&self, params: &Value) {
        let Some(arr) = params.as_array() else {
            log::debug!("[NVIM] popupmenu_show: expected array params");
            return;
        };
        if arr.len() < 2 {
            log::debug!(
                "[NVIM] popupmenu_show: expected >= 2 params, got {}",
                arr.len()
            );
            return;
        }
        let items = arr[0].as_array();
        let selected = arr[1].as_i64().unwrap_or(-1);

        let words: Vec<String> = items
            .map(|item_arr| {
                item_arr
                    .iter()
                    .map(|item| {
                        let fields = match item.as_array() {
                            Some(f) => f,
                            None => return String::new(),
                        };
                        // Try word first, then menu, then kind (Codex: kind is label-like)
                        let word = fields.first().and_then(|v| v.as_str()).unwrap_or("");
                        if !word.is_empty() {
                            return word.to_string();
                        }
                        let menu = fields.get(2).and_then(|v| v.as_str()).unwrap_or("");
                        if !menu.is_empty() {
                            return menu.to_string();
                        }
                        let kind = fields.get(1).and_then(|v| v.as_str()).unwrap_or("");
                        kind.to_string()
                    })
                    .collect()
            })
            .unwrap_or_default();

        log::debug!(
            "[NVIM] popupmenu_show: {} items, selected={}",
            words.len(),
            selected
        );

        // Cache items for popupmenu_select
        *self.last_popupmenu_items.lock().unwrap() = words.clone();

        if words.is_empty() {
            send_msg(&self.tx, FromNeovim::Candidates(CandidateInfo::empty()));
        } else {
            let sel = selected.max(0) as usize;
            let mut info = CandidateInfo::new(words, sel);
            info.selected = info.selected.min(info.candidates.len().saturating_sub(1));
            send_msg(&self.tx, FromNeovim::Candidates(info));
        }
    }

    /// popupmenu_select: [selected]
    fn handle_popupmenu_select(&self, params: &Value) {
        let selected = params
            .as_array()
            .and_then(|a| a.first())
            .and_then(|v| v.as_i64())
            .unwrap_or(-1);

        let items = self.last_popupmenu_items.lock().unwrap();
        log::trace!("[NVIM] popupmenu_select: selected={}", selected);

        if items.is_empty() {
            send_msg(&self.tx, FromNeovim::Candidates(CandidateInfo::empty()));
        } else {
            // selected = -1 means no selection; clamp to 0
            let sel = (selected.max(0) as usize).min(items.len().saturating_sub(1));
            send_msg(
                &self.tx,
                FromNeovim::Candidates(CandidateInfo::new(items.clone(), sel)),
            );
        }
    }

    /// popupmenu_hide
    fn handle_popupmenu_hide(&self) {
        log::debug!("[NVIM] popupmenu_hide");
        self.last_popupmenu_items.lock().unwrap().clear();
        send_msg(&self.tx, FromNeovim::Candidates(CandidateInfo::empty()));
    }

    /// cmdline_hide: [level]
    fn handle_cmdline_hide(&self, params: &Value) {
        let Some(arr) = params.as_array() else {
            log::debug!("[NVIM] cmdline_hide: expected array params");
            return;
        };
        let level = arr.first().and_then(|v| v.as_u64()).unwrap_or(1);
        log::debug!("[NVIM] cmdline_hide: level={}", level);
        send_msg(&self.tx, FromNeovim::CmdlineHide { level });
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

    let handler = NvimHandler {
        tx: tx.clone(),
        last_popupmenu_items: Arc::new(Mutex::new(Vec::new())),
    };
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

    // Store jacin's channel ID so Lua rpcnotify targets only this client
    // (channel 0 broadcasts to ALL clients, including denops etc.)
    let api_info = nvim.get_api_info().await?;
    let chan_id = api_info
        .first()
        .and_then(|v| v.as_i64())
        .filter(|&id| id > 0)
        .ok_or_else(|| anyhow::anyhow!("failed to get valid channel ID from nvim_get_api_info"))?;
    nvim.exec_lua(&format!("vim.g.ime_channel = {chan_id}"), vec![])
        .await?;
    log::info!("[NVIM] Channel ID: {}", chan_id);

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

    // Completion adapter — nvim-cmp requires Lua hooks; native uses ext_popupmenu
    if config.completion.adapter == "nvim-cmp" {
        nvim.exec_lua(include_str!("lua/completion_cmp.lua"), vec![])
            .await?;
    }

    // Attach as UI client to receive redraw events (ext_cmdline, ext_popupmenu)
    match nvim
        .call(
            "nvim_ui_attach",
            vec![
                Value::from(80i64),
                Value::from(24i64),
                Value::Map(vec![
                    (Value::from("ext_cmdline"), Value::from(true)),
                    (Value::from("ext_popupmenu"), Value::from(true)),
                ]),
            ],
        )
        .await?
    {
        Ok(_) => log::info!("[NVIM] nvim_ui_attach succeeded with ext_cmdline, ext_popupmenu"),
        Err(e) => anyhow::bail!("nvim_ui_attach failed: {e:?}"),
    }

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

    // ":", "/", "?" in normal mode enter command-line mode.
    // Display update comes via cmdline_show (ext_cmdline).
    // Must set PENDING synchronously to prevent handle_snapshot_response
    // from escaping command-line mode before cmdline_show arrives.
    if matches!(key, ":" | "/" | "?") && last_mode.as_str() == "n" {
        PENDING.store(PendingState::CommandLine);
        log::debug!("[NVIM] Entered command-line mode ({})", key);
        send_msg(tx, FromNeovim::KeyProcessed);
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
