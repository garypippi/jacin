//! Probe Neovim's UI events with ext_multigrid.
//!
//! Usage: cargo run --example grid_probe -- [--clean] [KEY...]
//!
//! Attaches like jacin (plus ext_multigrid), sends each KEY via nvim_input
//! with a pause in between, and prints a compact summary of every redraw
//! batch. Keys default to a small insert/newline/normal-mode scenario.

#[allow(dead_code)]
#[path = "../src/neovim/process.rs"]
mod process;
#[allow(dead_code)]
#[path = "../src/neovim/rpc.rs"]
mod rpc;

use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use rmpv::Value;

use process::{NvimProcess, ReadStatus};
use rpc::{Message, Rpc};

struct Probe {
    process: NvimProcess,
    rpc: Rpc,
}

impl Probe {
    fn request(&mut self, method: &str, params: Vec<Value>) -> anyhow::Result<()> {
        let mut out = Vec::new();
        self.rpc.request(method, params, &mut out);
        self.process.write(&out)?;
        Ok(())
    }

    /// Print what Neovim sends for `duration`
    fn pump(&mut self, duration: Duration) -> anyhow::Result<()> {
        let deadline = Instant::now() + duration;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(());
            }
            let mut pfd = libc::pollfd {
                fd: self.process.stdout_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: pfd is a valid pollfd for the duration of the call
            if unsafe { libc::poll(&mut pfd, 1, remaining.as_millis() as i32) } <= 0 {
                continue;
            }
            let mut bytes = Vec::new();
            let status = self.process.read_available(&mut bytes)?;
            self.rpc.feed(&bytes);
            while let Some(message) = self.rpc.next_message()? {
                match message {
                    Message::Notification { method, params } if method == "redraw" => {
                        print_redraw(&params)
                    }
                    Message::Notification { method, .. } => println!("notify {method}"),
                    Message::Response { result: Err(e), .. } => {
                        println!("error {}", rpc::error_message(&e))
                    }
                    _ => {}
                }
            }
            if status == ReadStatus::Eof {
                anyhow::bail!("Neovim exited");
            }
        }
    }
}

fn print_redraw(args: &[Value]) {
    let mut hl_defs = 0;
    for group in args {
        let Some(arr) = group.as_array() else {
            continue;
        };
        let Some(event) = arr.first().and_then(Value::as_str) else {
            continue;
        };
        for params in &arr[1..] {
            match event {
                "hl_attr_define" => hl_defs += 1,
                "hl_group_set" | "option_set" | "mode_info_set" | "chdir"
                | "default_colors_set" | "mouse_on" | "mouse_off" | "busy_start" | "busy_stop"
                | "update_menu" => {}
                "grid_line" => {
                    let p = params.as_array().unwrap();
                    let text: String = p[3]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|c| {
                            let c = c.as_array().unwrap();
                            let t = c[0].as_str().unwrap_or("");
                            let n = c.get(2).and_then(Value::as_u64).unwrap_or(1) as usize;
                            t.repeat(n)
                        })
                        .collect();
                    println!(
                        "  grid_line g={} row={} col={} {:?}",
                        p[0],
                        p[1],
                        p[2],
                        text.trim_end()
                    );
                }
                _ => println!("  {event} {params}"),
            }
        }
    }
    if hl_defs > 0 {
        println!("  (hl_attr_define x{hl_defs})");
    }
    println!("-- batch end");
}

fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let clean = args.first().is_some_and(|a| a == "--clean");
    if clean {
        args.remove(0);
    }
    let keys = if args.is_empty() {
        ["i", "abc", "<CR>", "def", "<Esc>", "k"]
            .map(String::from)
            .to_vec()
    } else {
        args
    };

    let mut probe = Probe {
        process: NvimProcess::spawn(clean)?,
        rpc: Rpc::new(),
    };

    probe.request("nvim_command", vec!["set nomore".into()])?;
    probe.request(
        "nvim_command",
        vec!["set buftype=nofile bufhidden=wipe".into()],
    )?;
    let opts = [
        "ext_multigrid",
        "ext_linegrid",
        "ext_cmdline",
        "ext_popupmenu",
        "ext_messages",
    ]
    .map(|o| (Value::from(o), Value::from(true)))
    .to_vec();
    println!("=== ui_attach 80x24 multigrid (clean={clean})");
    probe.request(
        "nvim_ui_attach",
        vec![Value::from(80), Value::from(24), Value::Map(opts)],
    )?;
    probe.pump(Duration::from_millis(500))?;

    for key in keys {
        println!("\n=== key {key:?}");
        probe.request("nvim_input", vec![key.into()])?;
        probe.pump(Duration::from_millis(300))?;
    }

    probe.request("nvim_command", vec!["qa!".into()])?;
    probe.process.wait_timeout(Duration::from_secs(1))?;
    Ok(())
}
