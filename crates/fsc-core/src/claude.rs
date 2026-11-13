//! A long-lived Claude Code session driven over its stream-json protocol:
//!
//! ```text
//! claude -p --input-format stream-json --output-format stream-json \
//!        --verbose --include-partial-messages ...
//! ```
//!
//! This is the same stdin/stdout protocol the official Agent SDKs use. Keeping
//! one process alive gives us a persistent conversation and avoids paying
//! CLI start-up time on every question. Auth comes from your normal
//! `claude` login, i.e. your subscription.

use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::mpsc;

use crate::config::ClaudeConfig;

const PERSONA: &str = include_str!("../../../voice-home/persona.md");
const SETTINGS: &str = include_str!("../../../voice-home/.claude/settings.json");

#[derive(Debug, Clone)]
pub enum ClaudeEvent {
    /// Session initialised; `model` is the resolved model id.
    Ready { session_id: String, model: String },
    /// A chunk of reply text.
    TextDelta(String),
    /// Claude started using a tool.
    ToolUse { name: String },
    /// The turn finished.
    TurnDone { ok: bool, interrupted: bool, error: Option<String> },
    /// The CLI process went away. It is restarted on the next prompt.
    Exited { reason: String },
}

enum Req {
    Ask(String),
    Interrupt,
    Restart,
}

#[derive(Clone)]
pub struct ClaudeHandle {
    tx: mpsc::UnboundedSender<Req>,
}

impl ClaudeHandle {
    /// Start the session actor. Must be called inside a tokio runtime.
    pub fn spawn<F>(cfg: ClaudeConfig, home: PathBuf, emit: F) -> Self
    where
        F: Fn(ClaudeEvent) + Send + Sync + 'static,
    {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(actor(cfg, home, rx, emit));
        Self { tx }
    }

    pub fn ask(&self, text: impl Into<String>) {
        let _ = self.tx.send(Req::Ask(text.into()));
    }

    pub fn interrupt(&self) {
        let _ = self.tx.send(Req::Interrupt);
    }

    /// Kill the session and start a fresh conversation.
    pub fn restart(&self) {
        let _ = self.tx.send(Req::Restart);
    }
}

/// Create the assistant's working directory with a default persona and
/// tool allow-list. Existing files are never overwritten.
pub fn ensure_home(home: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(home.join(".claude"))?;
    for (rel, body) in [("persona.md", PERSONA), (".claude/settings.json", SETTINGS)] {
        let p = home.join(rel);
        if !p.exists() {
            std::fs::write(p, body)?;
        }
    }
    Ok(())
}

struct Proc {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    stderr: Arc<Mutex<VecDeque<String>>>,
}

#[derive(Default)]
struct Turn {
    active: bool,
    has_text: bool,
    interrupted: bool,
}

async fn actor<F>(cfg: ClaudeConfig, home: PathBuf, mut rx: mpsc::UnboundedReceiver<Req>, emit: F)
where
    F: Fn(ClaudeEvent) + Send + Sync + 'static,
{
    if let Err(e) = ensure_home(&home) {
        eprintln!("fsc: could not prepare {}: {e}", home.display());
    }
    let mut proc = start(&cfg, &home);
    let mut turn = Turn::default();
    let mut req_id: u64 = 0;

    loop {
        tokio::select! {
            req = rx.recv() => match req {
                None => break,
                Some(Req::Ask(text)) => {
                    if proc.is_none() {
                        proc = start(&cfg, &home);
                    }
                    let Some(p) = proc.as_mut() else {
                        emit(ClaudeEvent::TurnDone {
                            ok: false,
                            interrupted: false,
                            error: Some(format!("could not start `{}`", cfg.bin)),
                        });
                        continue;
                    };
                    let msg = json!({
                        "type": "user",
                        "message": { "role": "user", "content": text },
                    });
                    if let Err(e) = write_line(&mut p.stdin, &msg).await {
                        emit(ClaudeEvent::TurnDone { ok: false, interrupted: false, error: Some(e.to_string()) });
                        proc = None;
                        continue;
                    }
                    turn = Turn { active: true, ..Default::default() };
                }
                Some(Req::Interrupt) => {
                    if let (Some(p), true) = (proc.as_mut(), turn.active) {
                        req_id += 1;
                        let msg = json!({
                            "type": "control_request",
                            "request_id": format!("fsc_{req_id}"),
                            "request": { "subtype": "interrupt" },
                        });
                        if write_line(&mut p.stdin, &msg).await.is_ok() {
                            turn.interrupted = true;
                        }
                    }
                }
                Some(Req::Restart) => {
                    if let Some(mut p) = proc.take() {
                        let _ = p.child.kill().await;
                    }
                    if turn.active {
                        emit(ClaudeEvent::TurnDone { ok: true, interrupted: true, error: None });
                    }
                    turn = Turn::default();
                    proc = start(&cfg, &home);
                }
            },
            line = next_line(&mut proc) => match line {
                Ok(Some(l)) => handle_line(&l, &mut turn, &emit),
                Ok(None) | Err(_) => {
                    let mut reason = "claude exited".to_string();
                    if let Some(mut p) = proc.take() {
                        if let Ok(status) = p.child.wait().await {
                            reason = format!("claude exited ({status})");
                        }
                        let tail: Vec<String> = p.stderr.lock().unwrap().iter().cloned().collect();
                        if !tail.is_empty() {
                            reason = format!("{reason}: {}", tail.join(" | "));
                        }
                    }
                    if turn.active {
                        emit(ClaudeEvent::TurnDone { ok: false, interrupted: false, error: Some(reason.clone()) });
                    }
                    turn = Turn::default();
                    emit(ClaudeEvent::Exited { reason });
                }
            },
        }
    }
}

fn start(cfg: &ClaudeConfig, home: &Path) -> Option<Proc> {
    let mut cmd = tokio::process::Command::new(&cfg.bin);
    cmd.args([
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--model",
        &cfg.model,
        "--setting-sources",
        &cfg.setting_sources,
    ]);
    let persona = home.join("persona.md");
    if persona.exists() {
        cmd.arg("--append-system-prompt-file").arg(persona);
    }
    cmd.args(&cfg.extra_args)
        .current_dir(home)
        // Don't let a parent Claude Code session (e.g. while developing this
        // app from inside one) leak into the child.
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("fsc: failed to start {}: {e}", cfg.bin);
            return None;
        }
    };
    let stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    let stderr_buf = Arc::new(Mutex::new(VecDeque::new()));
    if let Some(err) = child.stderr.take() {
        let buf = stderr_buf.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                eprintln!("claude: {l}");
                let mut b = buf.lock().unwrap();
                b.push_back(l);
                if b.len() > 5 {
                    b.pop_front();
                }
            }
        });
    }
    Some(Proc { child, stdin, lines: BufReader::new(stdout).lines(), stderr: stderr_buf })
}

async fn next_line(proc: &mut Option<Proc>) -> std::io::Result<Option<String>> {
    match proc {
        Some(p) => p.lines.next_line().await,
        None => std::future::pending().await,
    }
}

async fn write_line(stdin: &mut ChildStdin, v: &Value) -> std::io::Result<()> {
    let mut s = v.to_string();
    s.push('\n');
    stdin.write_all(s.as_bytes()).await?;
    stdin.flush().await
}

fn handle_line<F: Fn(ClaudeEvent)>(line: &str, turn: &mut Turn, emit: &F) {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return };
    let s = |v: &Value| v.as_str().unwrap_or_default().to_string();

    match v["type"].as_str() {
        Some("system") if v["subtype"] == "init" => emit(ClaudeEvent::Ready {
            session_id: s(&v["session_id"]),
            model: s(&v["model"]),
        }),
        Some("stream_event") => {
            // Ignore output from sub-agents; only the top-level reply is shown.
            if !v["parent_tool_use_id"].is_null() {
                return;
            }
            let ev = &v["event"];
            match ev["type"].as_str() {
                Some("content_block_start") => {
                    let block = &ev["content_block"];
                    match block["type"].as_str() {
                        Some("text") if turn.has_text => emit(ClaudeEvent::TextDelta("\n\n".into())),
                        Some("tool_use") => emit(ClaudeEvent::ToolUse { name: s(&block["name"]) }),
                        _ => {}
                    }
                }
                Some("content_block_delta") if ev["delta"]["type"] == "text_delta" => {
                    turn.has_text = true;
                    emit(ClaudeEvent::TextDelta(s(&ev["delta"]["text"])));
                }
                _ => {}
            }
        }
        Some("result") => {
            if !turn.active {
                return;
            }
            let success = v["subtype"] == "success" && !v["is_error"].as_bool().unwrap_or(false);
            let ok = success || turn.interrupted;
            let error = (!ok).then(|| {
                v["result"].as_str().map(str::to_string).unwrap_or_else(|| s(&v["subtype"]))
            });
            emit(ClaudeEvent::TurnDone { ok, interrupted: turn.interrupted, error });
            *turn = Turn::default();
        }
        _ => {}
    }
}

/// "claude-sonnet-5-5" -> "SONNET 5.5", "sonnet" -> "SONNET".
pub fn pretty_model(model: &str) -> String {
    let m = model.strip_prefix("claude-").unwrap_or(model);
    let mut parts = m.split('-');
    let name = parts.next().unwrap_or_default().to_uppercase();
    let ver: Vec<&str> = parts.take_while(|p| p.chars().all(|c| c.is_ascii_digit()) && p.len() <= 2).collect();
    if ver.is_empty() { name } else { format!("{name} {}", ver.join(".")) }
}

/// "mcp__claude_ai_Google_Calendar__list_events" -> "Google Calendar · list events".
pub fn pretty_tool(name: &str) -> String {
    let mut it = name.split("__");
    match (it.next(), it.next(), it.next()) {
        (Some("mcp"), Some(server), Some(tool)) => {
            let server = server.strip_prefix("claude_ai_").unwrap_or(server).replace('_', " ");
            format!("{server} · {}", tool.replace('_', " "))
        }
        _ => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_names() {
        assert_eq!(pretty_model("claude-sonnet-5-5"), "SONNET 5.5");
        assert_eq!(pretty_model("sonnet"), "SONNET");
        assert_eq!(pretty_model("claude-haiku-5-5-20261001"), "HAIKU 5.5");
    }

    #[test]
    fn tool_names() {
        assert_eq!(pretty_tool("Bash"), "Bash");
        assert_eq!(
            pretty_tool("mcp__claude_ai_Google_Calendar__list_events"),
            "Google Calendar · list events"
        );
    }
}
