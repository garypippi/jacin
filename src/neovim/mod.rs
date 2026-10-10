//! Neovim backend module
//!
//! Runs an embedded Neovim and talks msgpack-rpc over its stdin/stdout on
//! the caller's thread. Nothing blocks: the caller polls `read_fd` (and
//! `write_fd` while `wants_write`), then handles messages one at a time with
//! `next_message`, applying each one's events to the model before the next.
//! Users manage their own Japanese input plugins inside Neovim.

#[cfg(test)]
mod integration_tests;
mod keys;
mod mode;
mod process;
pub mod protocol;
mod redraw;
mod redraw_grid;
mod rpc;

use std::collections::{HashMap, VecDeque};
use std::io;
use std::os::fd::BorrowedFd;
use std::time::Duration;

use rmpv::Value;

use crate::config::Config;
use keys::{Buffer, Call, Cmd, Ctx, Item, KeyProcessor, Reply, Req};
use process::NvimProcess;
use redraw::Redraw;
use rpc::{Message, Rpc};

pub use mode::Mode;
pub use process::ReadStatus;
pub use protocol::{FromNeovim, GridCell, GridEvent, HlAttr, PumAnchor};
pub use rpc::DecodeError;

/// Keys queued beyond this are dropped (Neovim is not keeping up)
pub const MAX_QUEUED_KEYS: usize = 256;

/// UI size before the popup size is known
const DEFAULT_UI_SIZE: (usize, usize) = (80, 24);

/// What a sent request's response is for
#[derive(Debug)]
enum Pending {
    /// Key processing; the reply goes to the KeyProcessor
    Key(Req),
    /// nvim_input or a reset's call (only errors are logged)
    Unanswered(&'static str),
    /// Part of the startup sequence; `Ready` follows the last one
    Init(&'static str),
    /// nvim_buf_attach (also part of startup when `init`)
    BufAttach { init: bool },
    /// Only errors are logged
    Other(&'static str),
}

pub struct Neovim {
    process: NvimProcess,
    rpc: Rpc,
    keys: KeyProcessor,
    redraw: Redraw,
    /// Sent requests awaiting a response, by msgid
    requests: HashMap<u32, Pending>,
    /// Startup requests not answered yet
    init_left: usize,
    /// nvim_input and reset calls not answered yet
    unanswered: usize,
    /// Key-processing requests held until the inputs and calls before them
    /// are answered
    held: VecDeque<Req>,
    /// Handle of the attached buffer (from its line events)
    main_buffer: Option<Value>,
    /// Register for the text a reset throws away (`stash`)
    stash_register: Option<char>,
}

impl Neovim {
    /// Start Neovim and send the startup sequence. `Ready` is reported once
    /// it has been answered; keys may be pushed before (they queue behind it
    /// in Neovim).
    pub fn spawn(config: &Config, ui_size: Option<(usize, usize)>) -> io::Result<Self> {
        log::info!("[NVIM] Starting Neovim...");
        let mut nvim = Self {
            process: NvimProcess::spawn(config.clean)?,
            rpc: Rpc::new(),
            keys: KeyProcessor::new(config.keybinds.commit.clone()),
            redraw: Redraw::default(),
            requests: HashMap::new(),
            init_left: 0,
            unanswered: 0,
            held: VecDeque::new(),
            main_buffer: None,
            stash_register: stash_register(config),
        };

        for command in [
            "set nocompatible",
            "set encoding=utf-8",
            // Disable "-- More --" prompt — in embedded mode nobody can dismiss it,
            // so any long message (e.g. denops error) would block Neovim forever.
            "set nomore",
            // The main buffer: buftype=nofile prevents E37 "No write since
            // last change" on :q; bufhidden=hide keeps it (and its handle)
            // when another buffer takes its window (e.g. :e memo.md)
            "setlocal buftype=nofile bufhidden=hide",
        ] {
            nvim.init_request("nvim_command", vec![command.into()], command);
        }

        let (width, height) = ui_size.unwrap_or(DEFAULT_UI_SIZE);
        let options = [
            "ext_cmdline",
            "ext_popupmenu",
            "ext_messages",
            // Implied by ext_messages; explicit because GridFlush relies on it
            "ext_linegrid",
            // Per-window grids: statusline stays on grid 1, win_viewport
            // gives the buffer line count, floats get their own grids
            "ext_multigrid",
        ]
        .into_iter()
        .map(|option| (Value::from(option), Value::from(true)))
        .collect();
        nvim.init_request(
            "nvim_ui_attach",
            vec![width.into(), height.into(), Value::Map(options)],
            "nvim_ui_attach",
        );

        // Mirror the buffer lines (committed on IME off)
        nvim.attach_buffer(true);

        if config.behavior.startinsert {
            nvim.init_request("nvim_command", vec!["startinsert".into()], "startinsert");
        }
        Ok(nvim)
    }

    /// Resize the UI to `(columns, rows)`; Neovim redraws every grid
    pub fn resize_ui(&mut self, (width, height): (usize, usize)) {
        self.request(
            "nvim_ui_try_resize",
            vec![width.into(), height.into()],
            Pending::Other("nvim_ui_try_resize"),
        );
    }

    pub fn read_fd(&self) -> BorrowedFd<'_> {
        self.process.stdout_fd()
    }

    pub fn write_fd(&self) -> BorrowedFd<'_> {
        self.process.stdin_fd()
    }

    /// Requests are waiting for stdin to become writable (then `flush`)
    pub fn wants_write(&self) -> bool {
        self.process.wants_write()
    }

    pub fn flush(&mut self) {
        if let Err(e) = self.process.flush() {
            log::warn!("[NVIM] Write failed: {e}");
        }
    }

    /// Read what stdout has; the messages are then taken with `next_message`.
    /// `Eof` means Neovim exited (after its last messages).
    pub fn read(&mut self) -> ReadStatus {
        let mut bytes = Vec::new();
        let status = self.process.read_available(&mut bytes).unwrap_or_else(|e| {
            log::error!("[NVIM] Read failed: {e}");
            ReadStatus::Eof
        });
        self.rpc.feed(&bytes);
        status
    }

    /// Handle the next message read, returning its events for the model.
    /// `mode` is the model's mode with all earlier events applied.
    /// None when no complete message is left. An error means the stream is
    /// broken and Neovim must be dropped.
    pub fn next_message(&mut self, mode: Mode) -> Result<Option<Vec<FromNeovim>>, DecodeError> {
        let Some(message) = self.rpc.next_message()? else {
            return Ok(None);
        };
        let mut events = Vec::new();
        match message {
            Message::Notification { method, params } => match method.as_str() {
                "redraw" => self.redraw.handle(&params, &mut events),
                "nvim_buf_lines_event" => {
                    self.main_buffer = params.first().cloned();
                    events.extend(buf_lines_event(&params));
                }
                "nvim_buf_detach_event" => {
                    self.main_buffer = None;
                    // The attached buffer was wiped (e.g. :bwipe): follow
                    // the new current buffer
                    log::debug!("[NVIM] Buffer detached, re-attaching to current buffer");
                    self.attach_buffer(false);
                }
                _ => log::trace!("[NVIM] Ignoring notification {method}"),
            },
            Message::Response { msgid, result } => {
                self.on_response(msgid, result, mode, &mut events)
            }
            Message::Request { msgid, method, .. } => {
                // Neovim waits for the answer, so always send one
                log::warn!("[NVIM] Unsupported request from Neovim: {method}");
                let mut out = Vec::new();
                self.rpc
                    .error_response(msgid, "jacin: unsupported request", &mut out);
                self.write(&out);
            }
        }
        Ok(Some(events))
    }

    /// Queue a user key, acknowledged with `KeyProcessed { seq }`.
    /// Returns events of keys that completed without waiting for Neovim.
    pub fn push_key(&mut self, seq: u64, key: &str, mode: Mode) -> Vec<FromNeovim> {
        let item = Item::Key {
            seq,
            key: key.to_string(),
        };
        let cmds = self.keys.push(item, &Ctx { mode });
        self.run(cmds)
    }

    /// Return to normal mode with an empty buffer and no floating windows,
    /// then enter insert mode if `insert` (after queued keys, not
    /// acknowledged). With `stash`, the text thrown away goes to the stash
    /// register, if one is set.
    pub fn reset(&mut self, insert: bool, stash: bool, mode: Mode) -> Vec<FromNeovim> {
        let stash = stash && self.stash_register.is_some();
        let cmds = self.keys.push(Item::Reset { insert, stash }, &Ctx { mode });
        self.run(cmds)
    }

    /// End the input session: keys typed in it have no effect anymore.
    /// Returns the seqs of dropped keys that haven't started.
    pub fn end_session(&mut self) -> Vec<u64> {
        self.keys.end_session()
    }

    /// A key is being processed or queued
    pub fn is_busy(&self) -> bool {
        self.keys.is_busy()
    }

    pub fn queued_keys(&self) -> usize {
        self.keys.queued_len()
    }

    /// Ask Neovim to quit, then kill it if it doesn't exit in time
    pub fn shutdown(mut self) {
        log::info!("[NVIM] Shutting down...");
        self.request("nvim_command", vec!["qa!".into()], Pending::Other("qa!"));
        match self.process.wait_timeout(Duration::from_millis(200)) {
            Ok(Some(status)) => log::info!("[NVIM] Exited: {status}"),
            Ok(None) => log::warn!("[NVIM] Did not exit in time, killing"),
            Err(e) => log::warn!("[NVIM] Failed to wait for exit: {e}"),
        }
        // Dropping the process kills and reaps it if still running
    }

    /// Kill Neovim (the stream is broken)
    pub fn kill(&mut self) {
        if let Err(e) = self.process.kill() {
            log::warn!("[NVIM] Failed to kill Neovim: {e}");
        }
    }

    fn on_response(
        &mut self,
        msgid: u32,
        result: Result<Value, Value>,
        mode: Mode,
        events: &mut Vec<FromNeovim>,
    ) {
        let Some(pending) = self.requests.remove(&msgid) else {
            log::warn!("[NVIM] Response to unknown request #{msgid}");
            return;
        };
        match pending {
            Pending::Key(req) => {
                let reply = key_reply(req, result);
                let cmds = self.keys.on_reply(reply, &Ctx { mode });
                events.extend(self.run(cmds));
            }
            Pending::Init(what) => {
                if let Err(e) = result {
                    log::error!("[NVIM] {what} failed: {}", rpc::error_message(&e));
                }
                self.init_done(events);
            }
            Pending::BufAttach { init } => {
                match result {
                    Ok(Value::Boolean(true)) => {}
                    Ok(value) => log::error!("[NVIM] nvim_buf_attach returned {value}"),
                    Err(e) => {
                        log::error!("[NVIM] nvim_buf_attach failed: {}", rpc::error_message(&e))
                    }
                }
                if init {
                    self.init_done(events);
                }
            }
            Pending::Unanswered(what) => {
                if let Err(e) = result {
                    log::warn!("[NVIM] {what} failed: {}", rpc::error_message(&e));
                }
                self.unanswered -= 1;
                if self.unanswered == 0 {
                    while let Some(req) = self.held.pop_front() {
                        self.send_key_request(req);
                    }
                }
            }
            Pending::Other(what) => {
                if let Err(e) = result {
                    log::warn!("[NVIM] {what} failed: {}", rpc::error_message(&e));
                }
            }
        }
    }

    fn init_done(&mut self, events: &mut Vec<FromNeovim>) {
        self.init_left -= 1;
        if self.init_left == 0 {
            log::info!("[NVIM] Initialization complete");
            events.push(FromNeovim::Ready);
        }
    }

    /// Perform the KeyProcessor's commands, returning its model events
    fn run(&mut self, cmds: Vec<Cmd>) -> Vec<FromNeovim> {
        let mut events = Vec::new();
        for cmd in cmds {
            match cmd {
                Cmd::Input(keys) => {
                    self.unanswered += 1;
                    let pending = Pending::Unanswered("nvim_input");
                    self.request("nvim_input", vec![keys.into()], pending);
                }
                // Only made after a mode reply, so no input is unanswered
                Cmd::Call(call) => {
                    self.unanswered += 1;
                    self.send_call(call);
                }
                // nvim_input only queues the keys, and nvim_get_mode (fast)
                // answers right away: sent together, it would report the
                // state before the keys. Waiting for nvim_input's answer
                // lets Neovim's main loop process them first; the same for
                // calls, which (non-fast) wait for the main loop.
                Cmd::Request(req) if self.unanswered > 0 => self.held.push_back(req),
                Cmd::Request(req) => self.send_key_request(req),
                Cmd::SetMode(mode) => events.push(FromNeovim::ModeChange(mode)),
                Cmd::Emit(event) => events.push(event),
            }
        }
        events
    }

    fn send_key_request(&mut self, req: Req) {
        let (method, params) = match req {
            Req::GetMode => ("nvim_get_mode", vec![]),
            Req::BufLines(buffer) => (
                "nvim_buf_get_lines",
                vec![self.buffer(buffer), 0.into(), (-1).into(), false.into()],
            ),
            Req::RegRecording => (
                "nvim_call_function",
                vec!["reg_recording".into(), Value::Array(vec![])],
            ),
        };
        self.request(method, params, Pending::Key(req));
    }

    fn send_call(&mut self, call: Call) {
        let (method, params, what) = match call {
            Call::CloseFloats => ("nvim_command", vec!["fclose!".into()], "fclose!"),
            // Other buffers are only hidden: they stay loaded for next time
            Call::CloseOtherWindows => (
                "nvim_command",
                vec!["silent! tabonly! | silent! only!".into()],
                "tabonly! | only!",
            ),
            Call::StashBuffer => {
                let Some(register) = self.stash_register else {
                    return;
                };
                // Charwise, so `<C-r>` inserts it without a trailing
                // newline; an empty buffer doesn't overwrite the register
                let command = format!(
                    "if getline(1, '$') != [''] | call setreg('{register}', getline(1, '$'), 'v') | endif"
                );
                ("nvim_command", vec![command.into()], "setreg")
            }
            Call::ShowMainBuffer => (
                "nvim_win_set_buf",
                vec![0.into(), self.buffer(Buffer::Main)],
                "nvim_win_set_buf",
            ),
            Call::ClearBuffer => (
                "nvim_buf_set_lines",
                vec![
                    self.buffer(Buffer::Main),
                    0.into(),
                    (-1).into(),
                    false.into(),
                    Value::Array(vec![]),
                ],
                "nvim_buf_set_lines",
            ),
            Call::StartInsert => ("nvim_command", vec!["startinsert".into()], "startinsert"),
        };
        self.request(method, params, Pending::Unanswered(what));
    }

    /// The handle for a buffer argument (0: the current buffer)
    fn buffer(&self, buffer: Buffer) -> Value {
        match (buffer, &self.main_buffer) {
            (Buffer::Main, Some(handle)) => handle.clone(),
            _ => 0.into(),
        }
    }

    fn init_request(&mut self, method: &str, params: Vec<Value>, what: &'static str) {
        self.init_left += 1;
        self.request(method, params, Pending::Init(what));
    }

    /// Attach to the current buffer for `nvim_buf_lines_event`, starting with
    /// its whole content (mirrored in `BufferMirror`)
    fn attach_buffer(&mut self, init: bool) {
        if init {
            self.init_left += 1;
        }
        self.request(
            "nvim_buf_attach",
            vec![0.into(), true.into(), Value::Map(vec![])],
            Pending::BufAttach { init },
        );
    }

    fn request(&mut self, method: &str, params: Vec<Value>, pending: Pending) {
        let mut out = Vec::new();
        let msgid = self.rpc.request(method, params, &mut out);
        self.requests.insert(msgid, pending);
        self.write(&out);
    }

    fn write(&mut self, bytes: &[u8]) {
        if let Err(e) = self.process.write(bytes) {
            // Neovim exited; stdout reaches EOF next
            log::warn!("[NVIM] Write failed: {e}");
        }
    }
}

/// The reply to a key-processing request
fn key_reply(req: Req, result: Result<Value, Value>) -> Reply {
    let value = match result {
        Ok(value) => value,
        Err(e) => return Reply::Err(rpc::error_message(&e)),
    };
    match req {
        // nvim_get_mode is "fast": it answers even while Neovim is blocked
        // in getchar(), and tells whether it is
        Req::GetMode => {
            let field = |name: &str| {
                value
                    .as_map()?
                    .iter()
                    .find(|(k, _)| k.as_str() == Some(name))
                    .map(|(_, v)| v)
            };
            let mode = field("mode").and_then(Value::as_str).unwrap_or("");
            Reply::Mode {
                mode: Mode::from_get_mode(mode),
                blocking: field("blocking").and_then(Value::as_bool) == Some(true),
            }
        }
        Req::BufLines(_) => Reply::Lines(strings(&value)),
        Req::RegRecording => Reply::Recording(value.as_str().unwrap_or("").to_string()),
    }
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|v| v.as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The configured stash register, if it is a letter (a named register;
/// others are read-only or change on their own)
fn stash_register(config: &Config) -> Option<char> {
    let register = config.behavior.stash_register?;
    if !register.is_ascii_alphabetic() {
        log::warn!("[CONFIG] stash_register {register:?} is not a letter (a-z, A-Z): ignored");
        return None;
    }
    Some(register)
}

/// nvim_buf_lines_event: [buf, changedtick, firstline, lastline, linedata, more]
/// (lastline = -1: to the end of the buffer, sent for the initial content)
fn buf_lines_event(args: &[Value]) -> Option<FromNeovim> {
    if args.len() < 5 {
        log::debug!(
            "[NVIM] nvim_buf_lines_event: expected >= 5 args, got {}",
            args.len()
        );
        return None;
    }
    let first = args[2].as_u64().unwrap_or(0) as usize;
    let last = args[3].as_u64().map(|l| l as usize);
    let lines = strings(&args[4]);
    log::trace!("[NVIM] buf_lines: [{}, {:?}) <- {:?}", first, last, lines);
    Some(FromNeovim::BufLines { first, last, lines })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode_map(mode: &str, blocking: bool) -> Value {
        Value::Map(vec![
            (Value::from("mode"), Value::from(mode)),
            (Value::from("blocking"), Value::from(blocking)),
        ])
    }

    #[test]
    fn get_mode_reply_is_classified() {
        assert_eq!(
            key_reply(Req::GetMode, Ok(mode_map("ic", false))),
            Reply::Mode {
                mode: Mode::Insert,
                blocking: false
            }
        );
        assert_eq!(
            key_reply(Req::GetMode, Ok(mode_map("R", true))),
            Reply::Mode {
                mode: Mode::Replace,
                blocking: true
            }
        );
    }

    #[test]
    fn error_reply_carries_the_message() {
        let error = Value::Array(vec![Value::from(0), Value::from("E5555: boom")]);
        assert_eq!(
            key_reply(Req::BufLines(Buffer::Current), Err(error)),
            Reply::Err("E5555: boom".into())
        );
    }

    #[test]
    fn buf_lines_event_to_end() {
        let args = [
            Value::from(1),
            Value::from(2),
            Value::from(0),
            Value::from(-1),
            Value::Array(vec![Value::from("a"), Value::from("b")]),
            Value::from(false),
        ];
        assert_eq!(
            buf_lines_event(&args),
            Some(FromNeovim::BufLines {
                first: 0,
                last: None,
                lines: vec!["a".into(), "b".into()],
            })
        );
    }
}
