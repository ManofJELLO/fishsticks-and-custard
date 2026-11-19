//! Control server used by the `fsc` CLI (and Hyprland binds), over the
//! platform's local IPC (`fsc_core::ipc::Listener`).

use anyhow::Result;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;

use fsc_core::ipc::{Command, Conn, Listener, Reply};

use crate::AppEvent;

/// Bind the control socket, refusing to start if another fscd is listening.
pub async fn bind() -> Result<Listener> {
    Listener::bind().await
}

pub async fn serve(mut listener: Listener, tx: async_channel::Sender<AppEvent>) {
    loop {
        match listener.accept().await {
            Ok(stream) => {
                let tx = tx.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle(stream, tx).await {
                        eprintln!("fsc: client error: {e}");
                    }
                });
            }
            Err(e) => {
                eprintln!("fsc: accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

async fn handle(stream: Conn, tx: async_channel::Sender<AppEvent>) -> Result<()> {
    let (r, mut w) = tokio::io::split(stream);
    let mut lines = BufReader::new(r).lines();
    while let Some(line) = lines.next_line().await? {
        let reply = match serde_json::from_str::<Command>(&line) {
            Ok(Command::Quit) => {
                // Answer before handing over: once the UI quits, the runtime
                // (and this task) may be gone before a reply could be written.
                w.write_all(b"{\"ok\":true}\n").await?;
                w.flush().await?;
                tx.send(AppEvent::Command(Command::Quit, None)).await?;
                return Ok(());
            }
            Ok(cmd) => {
                let (otx, orx) = oneshot::channel();
                tx.send(AppEvent::Command(cmd, Some(otx))).await?;
                match tokio::time::timeout(Duration::from_secs(3), orx).await {
                    Ok(Ok(r)) => r,
                    _ => Reply::err("no reply from UI"),
                }
            }
            Err(e) => Reply::err(format!("bad command: {e}")),
        };
        let mut out = serde_json::to_string(&reply)?;
        out.push('\n');
        w.write_all(out.as_bytes()).await?;
    }
    Ok(())
}

pub fn cleanup() {
    fsc_core::ipc::cleanup();
}
