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
use crate::memory;

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
    /// A session was rotated and its memory update finished. `facts` is how
    /// many facts are now remembered.
    MemorySaved { ok: bool, facts: usize, error: Option<String> },
    /// `forget()` finished: memory deleted, fresh session started.
    Forgotten,
}

enum Req {
    Ask(String),
    Interrupt,
    /// End this session and start a fresh one. `save`: update long-term
    /// memory from it first.
    Rotate { save: bool },
    Forget,
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

    /// End the session (saving what's worth remembering) and start fresh.
    /// A prompt asked meanwhile is held until the new session is up.
    pub fn rotate(&self, save: bool) {
        let _ = self.tx.send(Req::Rotate { save });
    }

    /// Delete long-term memory and start a fresh session.
    pub fn forget(&self) {
        let _ = self.tx.send(Req::Forget);
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
    /// The hidden end-of-session memory update: text is captured, not shown.
    internal: bool,
    /// Internal: a user reply cut short by a rotation is still winding down.
    /// Its output is discarded; the memory update is sent once it ends.
    draining: bool,
    buf: String,
}

/// What `handle_line` reports about a hidden turn.
enum Internal {
    /// The interrupted user turn ended: the memory update can be sent now.
    Drained,
    /// The memory update finished.
    Memory { ok: bool, reply: String },
}

/// Kill the current process (if any) and start a new session.
async fn restart(proc: &mut Option<Proc>, cfg: &ClaudeConfig, home: &Path) {
    if let Some(mut p) = proc.take() {
        let _ = p.child.kill().await;
    }
    *proc = start(cfg, home);
}

async fn send_user(proc: &mut Option<Proc>, text: &str) -> std::io::Result<()> {
    let p = proc.as_mut().ok_or_else(|| std::io::Error::other("claude is not running"))?;
    let msg = json!({ "type": "user", "message": { "role": "user", "content": text } });
    write_line(&mut p.stdin, &msg).await
}

async fn send_interrupt(p: &mut Proc, req_id: &mut u64) -> std::io::Result<()> {
    *req_id += 1;
    let msg = json!({
        "type": "control_request",
        "request_id": format!("fsc_{req_id}"),
        "request": { "subtype": "interrupt" },
    });
    write_line(&mut p.stdin, &msg).await
}

/// Ask the session for its memory update. `false` (and a failed
/// `MemorySaved`) if it couldn't be sent.
async fn begin_save<F: Fn(ClaudeEvent)>(proc: &mut Option<Proc>, turn: &mut Turn, home: &Path, emit: &F) -> bool {
    let prompt = memory::update_prompt(&memory::load(home));
    match send_user(proc, &prompt).await {
        Ok(()) => {
            eprintln!("fsc: saving memory before starting a new session");
            *turn = Turn { active: true, internal: true, ..Default::default() };
            true
        }
        Err(e) => {
            emit(ClaudeEvent::MemorySaved { ok: false, facts: memory::load(home).len(), error: Some(e.to_string()) });
            false
        }
    }
}

/// Store the memory update's reply and report the outcome.
fn finish_save(home: &Path, ok: bool, reply: String) -> ClaudeEvent {
    let current = memory::load(home);
    match (ok, memory::parse_reply(&reply, &current)) {
        (true, Some(facts)) => match memory::save(home, &facts) {
            Ok(()) => ClaudeEvent::MemorySaved { ok: true, facts: facts.len(), error: None },
            Err(e) => ClaudeEvent::MemorySaved { ok: false, facts: current.len(), error: Some(e.to_string()) },
        },
        (true, None) => ClaudeEvent::MemorySaved {
            ok: false,
            facts: current.len(),
            error: Some("unusable memory reply; kept the old memory".into()),
        },
        (false, _) => ClaudeEvent::MemorySaved { ok: false, facts: current.len(), error: Some(reply) },
    }
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
    // The session has had real exchanges (worth saving to memory).
    let mut dirty = false;
    // Prompts that arrived while the memory update was running.
    let mut queued: VecDeque<String> = VecDeque::new();
    // A hidden (drain + memory update) turn must finish by this time, or it's
    // abandoned so a queued prompt can't hang behind it.
    let mut internal_deadline: Option<tokio::time::Instant> = None;

    loop {
        if turn.active && turn.internal {
            internal_deadline.get_or_insert_with(|| tokio::time::Instant::now() + INTERNAL_TURN_TIMEOUT);
        } else {
            internal_deadline = None;
        }
        tokio::select! {
            _ = sleep_until_opt(internal_deadline) => {
                eprintln!("fsc: memory update timed out; starting a new session without it");
                emit(ClaudeEvent::MemorySaved {
                    ok: false,
                    facts: memory::load(&home).len(),
                    error: Some("memory update timed out".into()),
                });
                turn = Turn::default();
                restart(&mut proc, &cfg, &home).await;
                dirty = false;
                for text in queued.drain(..) {
                    ask(&mut proc, &mut turn, &mut dirty, &cfg, &home, &text, &emit).await;
                }
            }
            req = rx.recv() => match req {
                None => break,
                Some(Req::Ask(text)) => {
                    if turn.active && turn.internal {
                        // Saving memory before a new session: ask once it's up.
                        queued.push_back(text);
                        continue;
                    }
                    ask(&mut proc, &mut turn, &mut dirty, &cfg, &home, &text, &emit).await;
                }
                Some(Req::Interrupt) => {
                    if let (Some(p), true) = (proc.as_mut(), turn.active && !turn.internal)
                        && send_interrupt(p, &mut req_id).await.is_ok()
                    {
                        turn.interrupted = true;
                    }
                }
                Some(Req::Rotate { save }) => {
                    if turn.active && turn.internal {
                        continue; // already rotating
                    }
                    let save = save && cfg.memory;
                    if turn.active {
                        emit(ClaudeEvent::TurnDone { ok: true, interrupted: true, error: None });
                        if save && dirty && let Some(p) = proc.as_mut() {
                            // The CLI is still producing that reply: stop it and
                            // ask for the memory update once it has ended, so
                            // its output isn't mistaken for the update.
                            let stopping = turn.interrupted || send_interrupt(p, &mut req_id).await.is_ok();
                            if stopping {
                                turn = Turn { active: true, internal: true, draining: true, ..Default::default() };
                                continue;
                            }
                        }
                        turn = Turn::default();
                    }
                    if save {
                        if dirty && proc.is_some() {
                            if begin_save(&mut proc, &mut turn, &home, &emit).await {
                                continue;
                            }
                        } else {
                            // Still answer, so a quit waiting on the save goes ahead.
                            emit(ClaudeEvent::MemorySaved {
                                ok: false,
                                facts: memory::load(&home).len(),
                                error: Some("this session had nothing to save".into()),
                            });
                        }
                    }
                    restart(&mut proc, &cfg, &home).await;
                    dirty = false;
                    eprintln!("fsc: new claude session");
                }
                Some(Req::Forget) => {
                    if let Err(e) = memory::forget(&home) {
                        eprintln!("fsc: could not delete memory: {e}");
                    }
                    if turn.active && turn.internal {
                        // A save was in flight: say it's cancelled (a quit may be waiting on it).
                        emit(ClaudeEvent::MemorySaved { ok: false, facts: 0, error: Some("cancelled by forget".into()) });
                    } else if turn.active {
                        emit(ClaudeEvent::TurnDone { ok: true, interrupted: true, error: None });
                    }
                    turn = Turn::default();
                    restart(&mut proc, &cfg, &home).await;
                    dirty = false;
                    eprintln!("fsc: memory forgotten, new claude session");
                    emit(ClaudeEvent::Forgotten);
                    for text in queued.drain(..) {
                        ask(&mut proc, &mut turn, &mut dirty, &cfg, &home, &text, &emit).await;
                    }
                }
            },
            line = next_line(&mut proc) => match line {
                Ok(Some(l)) => {
                    let Some(done) = handle_line(&l, &mut turn, &emit) else { continue };
                    let saving = match done {
                        Internal::Drained => begin_save(&mut proc, &mut turn, &home, &emit).await,
                        Internal::Memory { ok, reply } => {
                            // The memory update finished: store it, then start the new session.
                            let event = finish_save(&home, ok, reply);
                            eprintln!("fsc: {event:?}");
                            emit(event);
                            false
                        }
                    };
                    if !saving {
                        restart(&mut proc, &cfg, &home).await;
                        dirty = false;
                        for text in queued.drain(..) {
                            ask(&mut proc, &mut turn, &mut dirty, &cfg, &home, &text, &emit).await;
                        }
                    }
                }
                Ok(None) | Err(_) => {
                    let mut reason = "claude exited".to_string();
                    if let Some(mut p) = proc.take() {
                        // A read error (e.g. invalid UTF-8) can happen with the CLI still
                        // running; make sure it's gone before waiting on it.
                        let _ = p.child.start_kill();
                        if let Ok(status) = p.child.wait().await {
                            reason = format!("claude exited ({status})");
                        }
                        let tail: Vec<String> = p.stderr.lock().unwrap().iter().cloned().collect();
                        if !tail.is_empty() {
                            reason = format!("{reason}: {}", tail.join(" | "));
                        }
                    }
                    if turn.active && turn.internal {
                        emit(ClaudeEvent::MemorySaved {
                            ok: false,
                            facts: memory::load(&home).len(),
                            error: Some(reason.clone()),
                        });
                    } else if turn.active {
                        emit(ClaudeEvent::TurnDone { ok: false, interrupted: false, error: Some(reason.clone()) });
                    }
                    turn = Turn::default();
                    dirty = false;
                    emit(ClaudeEvent::Exited { reason });
                    for text in queued.drain(..) {
                        ask(&mut proc, &mut turn, &mut dirty, &cfg, &home, &text, &emit).await;
                    }
                }
            },
        }
    }
}

/// Send a user prompt, starting the process if needed.
async fn ask<F: Fn(ClaudeEvent)>(
    proc: &mut Option<Proc>,
    turn: &mut Turn,
    dirty: &mut bool,
    cfg: &ClaudeConfig,
    home: &Path,
    text: &str,
    emit: &F,
) {
    if proc.is_none() {
        *proc = start(cfg, home);
    }
    if proc.is_none() {
        emit(ClaudeEvent::TurnDone { ok: false, interrupted: false, error: Some(format!("could not start `{}`", cfg.bin)) });
        return;
    }
    if let Err(e) = send_user(proc, text).await {
        emit(ClaudeEvent::TurnDone { ok: false, interrupted: false, error: Some(e.to_string()) });
        *proc = None;
        return;
    }
    *dirty = true;
    *turn = Turn { active: true, ..Default::default() };
}

/// persona.md plus remembered facts, written fresh for each session.
fn write_session_prompt(cfg: &ClaudeConfig, home: &Path) -> Option<PathBuf> {
    let persona = std::fs::read_to_string(home.join("persona.md")).unwrap_or_else(|_| PERSONA.to_string());
    let facts = if cfg.memory { memory::load(home) } else { Vec::new() };
    let path = home.join(".session-prompt.md");
    match std::fs::write(&path, memory::session_prompt(&persona, &facts)) {
        Ok(()) => Some(path),
        Err(e) => {
            eprintln!("fsc: could not write session prompt: {e}");
            None
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
    // Fall back to the bare persona if the combined prompt can't be written.
    let persona = home.join("persona.md");
    if let Some(prompt) = write_session_prompt(cfg, home).or_else(|| persona.exists().then_some(persona)) {
        cmd.arg("--append-system-prompt-file").arg(prompt);
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

/// Longest a hidden end-of-session turn (drain + memory update) may take.
const INTERNAL_TURN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

async fn sleep_until_opt(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending().await,
    }
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

/// Process one line of CLI output. Returns `Some` when a hidden turn ends,
/// so the caller can carry on with the memory update.
fn handle_line<F: Fn(ClaudeEvent)>(line: &str, turn: &mut Turn, emit: &F) -> Option<Internal> {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return None };
    let s = |v: &Value| v.as_str().unwrap_or_default().to_string();

    match v["type"].as_str() {
        Some("system") if v["subtype"] == "init" => emit(ClaudeEvent::Ready {
            session_id: s(&v["session_id"]),
            model: s(&v["model"]),
        }),
        Some("stream_event") => {
            // Ignore output from sub-agents; only the top-level reply is shown.
            if !v["parent_tool_use_id"].is_null() {
                return None;
            }
            let ev = &v["event"];
            match ev["type"].as_str() {
                Some("content_block_start") => {
                    let block = &ev["content_block"];
                    match block["type"].as_str() {
                        // Keep separate text blocks on separate lines.
                        Some("text") if turn.internal && !turn.buf.is_empty() => turn.buf.push('\n'),
                        Some("text") if !turn.internal && turn.has_text => emit(ClaudeEvent::TextDelta("\n\n".into())),
                        Some("tool_use") if !turn.internal => emit(ClaudeEvent::ToolUse { name: s(&block["name"]) }),
                        _ => {}
                    }
                }
                Some("content_block_delta") if ev["delta"]["type"] == "text_delta" => {
                    let text = s(&ev["delta"]["text"]);
                    if turn.internal {
                        turn.buf.push_str(&text);
                    } else {
                        turn.has_text = true;
                        emit(ClaudeEvent::TextDelta(text));
                    }
                }
                _ => {}
            }
        }
        Some("result") => {
            if !turn.active {
                return None;
            }
            let success = v["subtype"] == "success" && !v["is_error"].as_bool().unwrap_or(false);
            let error_text = || v["result"].as_str().map(str::to_string).unwrap_or_else(|| s(&v["subtype"]));
            if turn.internal {
                let done = if turn.draining {
                    Internal::Drained
                } else if success {
                    Internal::Memory { ok: true, reply: std::mem::take(&mut turn.buf) }
                } else {
                    Internal::Memory { ok: false, reply: error_text() }
                };
                *turn = Turn::default();
                return Some(done);
            }
            let ok = success || turn.interrupted;
            let error = (!ok).then(error_text);
            emit(ClaudeEvent::TurnDone { ok, interrupted: turn.interrupted, error });
            *turn = Turn::default();
        }
        _ => {}
    }
    None
}

/// "claude-sonnet-5-5" -> "SONNET 5.5", "claude-3-5-sonnet-20241022" -> "SONNET 3.5",
/// "sonnet" -> "SONNET".
pub fn pretty_model(model: &str) -> String {
    let m = model.strip_prefix("claude-").unwrap_or(model);
    let parts: Vec<&str> = m.split('-').collect();
    let name = parts.iter().find(|p| p.chars().next().is_some_and(|c| c.is_ascii_alphabetic()));
    let Some(name) = name else { return model.to_uppercase() };
    // Version numbers are short numeric parts; long ones are dates.
    let ver: Vec<&str> =
        parts.iter().copied().filter(|p| !p.is_empty() && p.len() <= 2 && p.chars().all(|c| c.is_ascii_digit())).collect();
    let name = name.to_uppercase();
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
        assert_eq!(pretty_model("claude-3-5-sonnet-20241022"), "SONNET 3.5");
        assert_eq!(pretty_model("opus"), "OPUS");
    }

    #[test]
    fn hidden_turns_stay_hidden() {
        let shown = std::cell::RefCell::new(Vec::new());
        let emit = |e: ClaudeEvent| shown.borrow_mut().push(format!("{e:?}"));
        let delta = |t: &str| {
            json!({"type": "stream_event", "parent_tool_use_id": null,
                   "event": {"type": "content_block_delta", "delta": {"type": "text_delta", "text": t}}})
            .to_string()
        };
        let text_start = json!({"type": "stream_event", "parent_tool_use_id": null,
            "event": {"type": "content_block_start", "content_block": {"type": "text"}}})
        .to_string();
        let result = json!({"type": "result", "subtype": "success", "is_error": false}).to_string();

        // A reply cut short by a rotation: discarded, then reported as drained.
        let mut turn = Turn { active: true, internal: true, draining: true, ..Default::default() };
        assert!(handle_line(&delta("- not a memory"), &mut turn, &emit).is_none());
        assert!(matches!(handle_line(&result, &mut turn, &emit), Some(Internal::Drained)));

        // The memory update: text blocks are kept on separate lines.
        let mut turn = Turn { active: true, internal: true, ..Default::default() };
        for l in [text_start.clone(), delta("- a"), text_start, delta("- b")] {
            assert!(handle_line(&l, &mut turn, &emit).is_none());
        }
        match handle_line(&result, &mut turn, &emit) {
            Some(Internal::Memory { ok: true, reply }) => assert_eq!(reply, "- a\n- b"),
            _ => panic!("expected the memory reply"),
        }
        assert!(shown.borrow().is_empty());
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
