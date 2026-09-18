//! Probe Neovim's UI events with ext_multigrid, for Phase B design.
//!
//! Usage: cargo run --example grid_probe -- [--clean] [KEY...]
//!
//! Attaches like jacin (plus ext_multigrid), sends each KEY via nvim_input
//! with a pause in between, and prints a compact summary of every redraw
//! batch. Keys default to a small insert/newline/normal-mode scenario.

use std::time::Duration;

use async_trait::async_trait;
use nvim_rs::create::tokio::new_child_cmd;
use nvim_rs::{Handler, Neovim, Value};
use tokio::process::Command;

type Writer = nvim_rs::compat::tokio::Compat<tokio::process::ChildStdin>;

#[derive(Clone)]
struct Probe;

#[async_trait]
impl Handler for Probe {
    type Writer = Writer;

    async fn handle_notify(&self, name: String, args: Vec<Value>, _nvim: Neovim<Writer>) {
        if name != "redraw" {
            println!("notify {name}");
            return;
        }
        let mut hl_defs = 0;
        for group in &args {
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
                    | "default_colors_set" | "mouse_on" | "mouse_off" | "busy_start"
                    | "busy_stop" | "update_menu" => {}
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
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
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

    let mut cmd = Command::new("nvim");
    cmd.args(["--embed", "--headless", "--cmd", "let g:jacin = 1"]);
    if clean {
        cmd.arg("--clean");
    }
    let (nvim, _io, _child) = new_child_cmd(&mut cmd, Probe).await?;

    nvim.command("set nomore").await?;
    nvim.command("set buftype=nofile bufhidden=wipe").await?;
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
    nvim.call(
        "nvim_ui_attach",
        vec![Value::from(80), Value::from(24), Value::Map(opts)],
    )
    .await?
    .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    tokio::time::sleep(Duration::from_millis(500)).await;

    for key in keys {
        println!("\n=== key {key:?}");
        nvim.input(&key).await?;
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    let _ = nvim.command("qa!").await;
    Ok(())
}
