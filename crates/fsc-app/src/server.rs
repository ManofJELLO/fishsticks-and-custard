//! Unix-socket control server used by the `fsc` CLI (and Hyprland binds).

use anyhow::{Result, bail};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;

use fsc_core::ipc::{Command, Reply, socket_path};

use crate::AppEvent;

/// Bind the socket, refusing to start if another fscd is already listening.
pub async fn bind() -> Result<UnixListener> {
    let path = socket_path();
    if path.exists() {
        if UnixStream::connect(&path).await.is_ok() {
            bail!("fscd is already running ({})", path.display());
        }
        let _ = std::fs::remove_file(&path);
    }
    Ok(UnixListener::bind(&path)?)
}

pub async fn serve(listener: UnixListener, tx: async_channel::Sender<AppEvent>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
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

async fn handle(stream: UnixStream, tx: async_channel::Sender<AppEvent>) -> Result<()> {
    let (r, mut w) = stream.into_split();
    let mut lines = BufReader::new(r).lines();
    while let Some(line) = lines.next_line().await? {
        let reply = match serde_json::from_str::<Command>(&line) {
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
    let _ = std::fs::remove_file(socket_path());
}
