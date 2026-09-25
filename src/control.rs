//! Control socket: `jacin toggle|on|off|status` talk to the running jacin
//!
//! The IME is turned on from outside because jacin holds no keyboard grab
//! while it is off, so it cannot see a toggle key itself. One line is sent
//! (`toggle`) and one line comes back (`on` / `off`, or `error: ...`).

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, bail};

/// A client that doesn't send its line in time is dropped (the event loop
/// waits for it)
const READ_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Command {
    Toggle,
    On,
    Off,
    Status,
    /// Switch between the small popup and the whole Neovim screen
    Focus,
}

impl Command {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "toggle" => Some(Self::Toggle),
            "on" => Some(Self::On),
            "off" => Some(Self::Off),
            "status" => Some(Self::Status),
            "focus" => Some(Self::Focus),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Toggle => "toggle",
            Self::On => "on",
            Self::Off => "off",
            Self::Status => "status",
            Self::Focus => "focus",
        }
    }

    /// Whether the IME should be on after this command
    pub fn target(self, enabled: bool) -> bool {
        match self {
            Self::Toggle => !enabled,
            Self::On => true,
            Self::Off => false,
            Self::Status | Self::Focus => enabled,
        }
    }
}

/// A reply starting with this is printed as an error by the client
pub const ERROR_PREFIX: &str = "error: ";

pub const FOCUS_UNSET: &str = "error: focus mode needs [focus] columns and lines in the config";

pub fn reply(enabled: bool) -> &'static str {
    if enabled { "on" } else { "off" }
}

/// One socket per Wayland display, so sessions don't reach each other's jacin
pub fn socket_path() -> anyhow::Result<PathBuf> {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".into());
    Ok(PathBuf::from(runtime_dir).join(socket_name(&display)))
}

fn socket_name(display: &str) -> String {
    // WAYLAND_DISPLAY may be an absolute path
    let display = display.rsplit('/').next().unwrap_or(display);
    format!("jacin-{display}.sock")
}

/// Send a command to the running jacin and return its reply
pub fn send(command: Command) -> anyhow::Result<String> {
    let path = socket_path()?;
    let mut stream = UnixStream::connect(&path)
        .with_context(|| format!("jacin is not running (no socket at {})", path.display()))?;
    writeln!(stream, "{}", command.as_str())?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    let reply = line.trim();
    if reply.is_empty() {
        bail!("no reply from jacin");
    }
    Ok(reply.to_string())
}

/// Listen on the control socket, refusing to start next to a running jacin
pub fn listen() -> anyhow::Result<(UnixListener, PathBuf)> {
    let path = socket_path()?;
    if UnixStream::connect(&path).is_ok() {
        bail!("jacin is already running ({})", path.display());
    }
    // Left behind by a jacin that didn't exit cleanly
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() != ErrorKind::NotFound => {
            return Err(e).context(format!("removing {}", path.display()));
        }
        _ => {}
    }
    let listener =
        UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;
    listener.set_nonblocking(true)?;
    Ok((listener, path))
}

/// Read one command from a client just accepted
pub fn read_command(stream: &UnixStream) -> Option<Command> {
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(READ_TIMEOUT)).ok()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).ok()?;
    Command::parse(line.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_round_trip() {
        for command in [
            Command::Toggle,
            Command::On,
            Command::Off,
            Command::Status,
            Command::Focus,
        ] {
            assert_eq!(Command::parse(command.as_str()), Some(command));
        }
        assert_eq!(Command::parse("enable"), None);
    }

    #[test]
    fn focus_unset_is_an_error() {
        assert!(FOCUS_UNSET.starts_with(ERROR_PREFIX));
    }

    #[test]
    fn target_state() {
        assert!(Command::Toggle.target(false));
        assert!(!Command::Toggle.target(true));
        assert!(Command::On.target(true));
        assert!(!Command::Off.target(false));
        assert!(Command::Status.target(true));
        assert!(!Command::Status.target(false));
    }

    #[test]
    fn socket_name_uses_the_display_name() {
        assert_eq!(socket_name("wayland-1"), "jacin-wayland-1.sock");
        assert_eq!(
            socket_name("/run/user/1000/wayland-1"),
            "jacin-wayland-1.sock"
        );
    }
}
