//! Neovim child process with non-blocking stdin/stdout pipes

use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Result of draining stdout
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadStatus {
    /// Everything available was read; more may come later
    Open,
    /// The child closed stdout (it exited or is exiting)
    Eof,
}

pub struct NvimProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    /// Bytes not yet accepted by the stdin pipe
    pending_write: Vec<u8>,
}

impl NvimProcess {
    /// Start `nvim --embed`. `clean` adds `--clean` (no user config/plugins).
    pub fn spawn(clean: bool) -> io::Result<Self> {
        let mut cmd = Command::new("nvim");
        cmd.args(["--embed", "--headless"]);
        // Lets user config detect jacin (`if vim.g.jacin then ... end`);
        // --cmd runs before init.lua
        cmd.args(["--cmd", "let g:jacin = 1"]);
        if clean {
            cmd.arg("--clean");
        }
        Self::spawn_command(cmd)
    }

    fn spawn_command(mut cmd: Command) -> io::Result<Self> {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        set_nonblocking(stdin.as_fd())?;
        set_nonblocking(stdout.as_fd())?;
        Ok(Self {
            child,
            stdin,
            stdout,
            pending_write: Vec::new(),
        })
    }

    pub fn stdout_fd(&self) -> BorrowedFd<'_> {
        self.stdout.as_fd()
    }

    pub fn stdin_fd(&self) -> BorrowedFd<'_> {
        self.stdin.as_fd()
    }

    /// Append everything currently readable from stdout to `out`
    pub fn read_available(&mut self, out: &mut Vec<u8>) -> io::Result<ReadStatus> {
        let mut chunk = [0u8; 64 * 1024];
        loop {
            match self.stdout.read(&mut chunk) {
                Ok(0) => return Ok(ReadStatus::Eof),
                Ok(n) => out.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(ReadStatus::Open),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// Queue bytes for stdin and write as much as the pipe accepts now
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.pending_write.extend_from_slice(bytes);
        self.flush()
    }

    /// Write queued bytes until done or the pipe is full
    pub fn flush(&mut self) -> io::Result<()> {
        while !self.pending_write.is_empty() {
            match self.stdin.write(&self.pending_write) {
                Ok(n) => {
                    self.pending_write.drain(..n);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Whether queued bytes are waiting for stdin to become writable
    pub fn wants_write(&self) -> bool {
        !self.pending_write.is_empty()
    }

    /// Reap the child if it has exited
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    /// Wait up to `timeout` for the child to exit
    pub fn wait_timeout(&mut self, timeout: Duration) -> io::Result<Option<ExitStatus>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(Some(status));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Kill the child (if still running) and reap it
    pub fn kill(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.child.try_wait()? {
            return Ok(status);
        }
        self.child.kill()?;
        self.child.wait()
    }
}

impl Drop for NvimProcess {
    fn drop(&mut self) {
        if let Err(e) = self.kill() {
            log::warn!("[NVIM] Failed to reap Neovim: {e}");
        }
    }
}

fn set_nonblocking(fd: BorrowedFd<'_>) -> io::Result<()> {
    let raw = fd.as_raw_fd();
    // SAFETY: fcntl on a valid fd owned by this process
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above
    if unsafe { libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spawn(program: &str, args: &[&str]) -> NvimProcess {
        let mut cmd = Command::new(program);
        cmd.args(args);
        NvimProcess::spawn_command(cmd).unwrap()
    }

    /// Wait until `fd` is ready for `events`, or panic after a second
    fn poll(fd: BorrowedFd<'_>, events: libc::c_short) {
        let mut pfd = libc::pollfd {
            fd: fd.as_raw_fd(),
            events,
            revents: 0,
        };
        // SAFETY: pfd is a valid pollfd for the duration of the call
        let ready = unsafe { libc::poll(&mut pfd, 1, 1000) };
        assert!(ready > 0, "fd not ready within 1s");
    }

    #[test]
    fn reads_output_then_eof() {
        let mut process = spawn("sh", &["-c", "printf hello"]);
        let mut out = Vec::new();
        loop {
            poll(process.stdout_fd(), libc::POLLIN);
            if process.read_available(&mut out).unwrap() == ReadStatus::Eof {
                break;
            }
        }
        assert_eq!(out, b"hello");
        let status = process.wait_timeout(Duration::from_secs(1)).unwrap();
        assert!(status.is_some_and(|s| s.success()));
    }

    #[test]
    fn queues_writes_the_pipe_cannot_take_yet() {
        let mut process = spawn("cat", &[]);
        // Larger than a pipe buffer, so cat's output must be drained
        // before the rest of the input fits
        let data: Vec<u8> = (0..1024 * 1024).map(|i| (i % 251) as u8).collect();
        process.write(&data).unwrap();
        assert!(process.wants_write());

        let mut out = Vec::new();
        while out.len() < data.len() {
            if process.wants_write() {
                process.flush().unwrap();
            }
            poll(process.stdout_fd(), libc::POLLIN);
            assert_eq!(process.read_available(&mut out).unwrap(), ReadStatus::Open);
        }
        assert!(!process.wants_write());
        assert!(out == data);
    }

    #[test]
    fn read_without_data_does_not_block() {
        let mut process = spawn("cat", &[]);
        let mut out = Vec::new();
        assert_eq!(process.read_available(&mut out).unwrap(), ReadStatus::Open);
        assert!(out.is_empty());
    }

    #[test]
    #[ignore]
    fn nvim_answers_a_request() {
        use super::super::rpc::{Message, Rpc};

        let mut process = NvimProcess::spawn(true).unwrap();
        let mut rpc = Rpc::new();
        let mut out = Vec::new();
        let msgid = rpc.request("nvim_get_mode", vec![], &mut out);
        process.write(&out).unwrap();

        let mut buf = Vec::new();
        let message = loop {
            if let Some(message) = rpc.next_message().unwrap() {
                break message;
            }
            poll(process.stdout_fd(), libc::POLLIN);
            buf.clear();
            assert_eq!(process.read_available(&mut buf).unwrap(), ReadStatus::Open);
            rpc.feed(&buf);
        };
        let Message::Response {
            msgid: got,
            result: Ok(mode),
        } = message
        else {
            panic!("unexpected message: {message:?}");
        };
        assert_eq!(got, msgid);
        let mode = mode.as_map().unwrap();
        assert!(
            mode.iter()
                .any(|(k, v)| k.as_str() == Some("mode") && v.as_str() == Some("n"))
        );

        let mut out = Vec::new();
        rpc.request("nvim_command", vec!["qa!".into()], &mut out);
        process.write(&out).unwrap();
        assert!(
            process
                .wait_timeout(Duration::from_secs(2))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn kills_a_child_that_does_not_exit() {
        let mut process = spawn("sleep", &["10"]);
        assert!(
            process
                .wait_timeout(Duration::from_millis(20))
                .unwrap()
                .is_none()
        );
        let status = process.kill().unwrap();
        assert!(!status.success());
        // Already reaped: a second kill reports the same exit
        assert_eq!(process.kill().unwrap(), status);
    }
}
