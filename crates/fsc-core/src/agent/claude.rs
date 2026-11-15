//! Claude Code over its native stream-json protocol:
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
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};

use super::claude_ask::{self, Pending};
use super::{Backend, Conn, Event};
use crate::config::ClaudeConfig;
use crate::permission::Answer;

/// The assistant's allow/deny lists. In `settings.local.json` because Claude
/// Code ignores a folder's shared `settings.json` allow rules until you've
/// trusted the folder interactively; local settings are yours and always apply.
/// "Always" answers to permission questions are saved here too.
const SETTINGS: &str = include_str!("../../../../voice-home/.claude/settings.local.json");

/// Version of the seeded settings. Bump it with a step in `migrate_settings`
/// when an existing home's `settings.local.json` needs changing.
/// 1: Edit/Write/NotebookEdit denied. 2: they're asked about instead.
const SETTINGS_VERSION: u32 = 2;
const SETTINGS_VERSION_FILE: &str = ".claude/fsc-settings-version";

pub(crate) struct ClaudeCli {
    cfg: ClaudeConfig,
}

impl ClaudeCli {
    pub(crate) fn new(cfg: ClaudeConfig) -> Self {
        Self { cfg }
    }
}

impl Backend for ClaudeCli {
    type Conn = ClaudeConn;

    fn program(&self) -> &str {
        &self.cfg.bin
    }

    fn prepare(&self, home: &Path) -> std::io::Result<()> {
        seed_settings(home)
    }

    async fn start(&self, home: &Path, prompt: &str) -> Result<(ClaudeConn, Vec<Event>), String> {
        // The CLI says which model it's using once the first prompt is in.
        start(&self.cfg, home, prompt).map(|c| (c, Vec::new()))
    }
}

/// Seed the tool allow-list. An existing one is never overwritten, but older
/// seeded settings are migrated once (see `SETTINGS_VERSION`).
fn seed_settings(home: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(home.join(".claude"))?;
    let settings = home.join(".claude/settings.local.json");
    let version_file = home.join(SETTINGS_VERSION_FILE);
    // Whether the settings are now at SETTINGS_VERSION. Not when an invalid
    // file was left alone, so it's migrated once the user fixes it.
    let mut current = true;
    if settings.exists() {
        // No version file: seeded before versions were tracked, i.e. version 1.
        let from = std::fs::read_to_string(&version_file).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(1);
        if from < SETTINGS_VERSION {
            let text = std::fs::read_to_string(&settings)?;
            match migrate_settings(&text, from) {
                Some(new) => std::fs::write(&settings, new)?,
                None => {
                    eprintln!("fsc: {} isn't valid JSON; left as it is", settings.display());
                    current = false;
                }
            }
        }
    } else {
        std::fs::write(&settings, SETTINGS)?;
    }
    if current
        && std::fs::read_to_string(&version_file).ok().as_deref().map(str::trim) != Some(&SETTINGS_VERSION.to_string())
    {
        std::fs::write(&version_file, format!("{SETTINGS_VERSION}\n"))?;
    }
    Ok(())
}

/// Bring settings written for version `from` up to `SETTINGS_VERSION`,
/// keeping everything the user (or ALWAYS answers) added. `None` if the file
/// isn't a JSON object, so it's left alone.
fn migrate_settings(text: &str, from: u32) -> Option<String> {
    let mut v: Value = serde_json::from_str(text).ok()?;
    v.as_object()?;
    if from < 2 {
        // File edits are asked about now rather than refused.
        if let Some(deny) = v.pointer_mut("/permissions/deny").and_then(Value::as_array_mut) {
            deny.retain(|r| !matches!(r.as_str(), Some("Edit" | "Write" | "NotebookEdit")));
        }
    }
    let mut out = serde_json::to_string_pretty(&v).ok()?;
    out.push('\n');
    Some(out)
}

pub(crate) struct ClaudeConn {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    /// Permission questions the CLI is waiting on.
    pending: Vec<Pending>,
    /// Control responses to write back to the CLI.
    outbox: Vec<Value>,
    /// Numbers our own control requests.
    req_id: u64,
}

impl Conn for ClaudeConn {
    async fn recv(&mut self) -> std::io::Result<Option<String>> {
        self.lines.next_line().await
    }

    fn handle(&mut self, line: &str) -> Vec<Event> {
        let (event, reply) = parse_line(line, &mut self.pending);
        self.outbox.extend(reply);
        event.into_iter().collect()
    }

    async fn flush(&mut self) {
        for msg in std::mem::take(&mut self.outbox) {
            if let Err(e) = write_line(&mut self.stdin, &msg).await {
                eprintln!("fsc: could not answer claude: {e}");
            }
        }
    }

    async fn prompt(&mut self, text: &str) -> std::io::Result<()> {
        let msg = json!({ "type": "user", "message": { "role": "user", "content": text } });
        write_line(&mut self.stdin, &msg).await
    }

    async fn interrupt(&mut self) -> std::io::Result<()> {
        self.req_id += 1;
        let msg = json!({
            "type": "control_request",
            "request_id": format!("fsc_{}", self.req_id),
            "request": { "subtype": "interrupt" },
        });
        write_line(&mut self.stdin, &msg).await
    }

    fn answer(&mut self, id: &str, answer: Answer) -> bool {
        let Some(i) = self.pending.iter().position(|p| p.id == id) else { return false };
        let p = self.pending.remove(i);
        self.outbox.push(claude_ask::response(&p, answer));
        true
    }

    fn deny_all(&mut self) {
        for p in std::mem::take(&mut self.pending) {
            self.outbox.push(claude_ask::response(&p, Answer::Deny));
        }
    }

    /// The CLI holds one conversation; a new one means a new process.
    async fn new_session(&mut self, _: &Path, _: &str) -> Result<Vec<Event>, String> {
        Err(String::new())
    }

    async fn close(mut self) -> String {
        // A read error (e.g. invalid UTF-8) can happen with the CLI still
        // running; make sure it's gone before waiting on it.
        let _ = self.child.start_kill();
        let mut reason = match self.child.wait().await {
            Ok(status) => format!("claude exited ({status})"),
            Err(_) => "claude exited".to_string(),
        };
        let tail: Vec<String> = self.stderr.lock().unwrap().iter().cloned().collect();
        if !tail.is_empty() {
            reason = format!("{reason}: {}", tail.join(" | "));
        }
        reason
    }
}

fn start(cfg: &ClaudeConfig, home: &Path, prompt: &str) -> Result<ClaudeConn, String> {
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
        // Ask us (over stdin/stdout) when a tool needs permission.
        "--permission-prompt-tool",
        "stdio",
    ]);
    // Fall back to the bare persona if the combined prompt can't be written.
    let persona = home.join("persona.md");
    let prompt_file = home.join(".session-prompt.md");
    let prompt_file = match std::fs::write(&prompt_file, prompt) {
        Ok(()) => Some(prompt_file),
        Err(e) => {
            eprintln!("fsc: could not write session prompt: {e}");
            persona.exists().then_some(persona)
        }
    };
    if let Some(file) = prompt_file {
        cmd.arg("--append-system-prompt-file").arg(file);
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

    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err("no pipes to the CLI".into());
    };
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
    Ok(ClaudeConn {
        child,
        stdin,
        lines: BufReader::new(stdout).lines(),
        stderr: stderr_buf,
        pending: Vec::new(),
        outbox: Vec::new(),
        req_id: 0,
    })
}

async fn write_line(stdin: &mut ChildStdin, v: &Value) -> std::io::Result<()> {
    let mut s = v.to_string();
    s.push('\n');
    stdin.write_all(s.as_bytes()).await?;
    stdin.flush().await
}

/// Read one line of CLI output: the event in it, if any, and a control
/// response to send back, if one is needed.
fn parse_line(line: &str, pending: &mut Vec<Pending>) -> (Option<Event>, Option<Value>) {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return (None, None) };
    let s = |v: &Value| v.as_str().unwrap_or_default().to_string();

    let event = match v["type"].as_str() {
        Some("system") if v["subtype"] == "init" => Event::Ready { session_id: s(&v["session_id"]), model: s(&v["model"]) },
        Some("stream_event") => {
            // Ignore output from sub-agents; only the top-level reply is shown.
            if !v["parent_tool_use_id"].is_null() {
                return (None, None);
            }
            let ev = &v["event"];
            match ev["type"].as_str() {
                Some("content_block_start") => {
                    let block = &ev["content_block"];
                    match block["type"].as_str() {
                        Some("text") => Event::TextStart,
                        Some("tool_use") => Event::ToolUse { name: s(&block["name"]) },
                        _ => return (None, None),
                    }
                }
                Some("content_block_delta") if ev["delta"]["type"] == "text_delta" => Event::Text(s(&ev["delta"]["text"])),
                _ => return (None, None),
            }
        }
        Some("control_request") => {
            let id = s(&v["request_id"]);
            let req = &v["request"];
            if req["subtype"] != "can_use_tool" {
                return (None, Some(claude_ask::unsupported(&id)));
            }
            let (ask, p) = claude_ask::parse(&id, req);
            pending.push(p);
            Event::Permission(ask)
        }
        Some("control_cancel_request") => {
            let id = s(&v["request_id"]);
            let Some(i) = pending.iter().position(|p| p.id == id) else { return (None, None) };
            pending.remove(i);
            Event::PermissionClosed { id }
        }
        Some("result") => {
            // The turn is over, so nothing it asked is still waiting.
            pending.clear();
            let ok = v["subtype"] == "success" && !v["is_error"].as_bool().unwrap_or(false);
            let error = (!ok).then(|| v["result"].as_str().map(str::to_string).unwrap_or_else(|| s(&v["subtype"])));
            Event::TurnEnd { ok, error }
        }
        _ => return (None, None),
    };
    (Some(event), None)
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
    fn migration_lets_file_edits_ask_and_keeps_user_rules() {
        let old = r#"{"permissions":{"allow":["Read","Bash(git status:*)"],
            "deny":["Bash(sudo:*)","Edit","Write","NotebookEdit"]},"model":"x"}"#;
        let v: Value = serde_json::from_str(&migrate_settings(old, 1).unwrap()).unwrap();
        assert_eq!(v["permissions"]["deny"], json!(["Bash(sudo:*)"]));
        assert_eq!(v["permissions"]["allow"], json!(["Read", "Bash(git status:*)"]));
        assert_eq!(v["model"], "x");
        assert!(migrate_settings("not json", 1).is_none());
        assert!(migrate_settings("[1]", 1).is_none());
    }

    #[test]
    fn seeded_settings_match_current_version() {
        let v: Value = serde_json::from_str(SETTINGS).unwrap();
        let deny = v["permissions"]["deny"].as_array().unwrap();
        assert!(!deny.iter().any(|r| r == "Edit" || r == "Write"));
    }

    #[test]
    fn settings_migrate_once() {
        let home = std::env::temp_dir().join(format!("fsc-home-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let settings = home.join(".claude/settings.local.json");
        std::fs::write(&settings, r#"{"permissions":{"deny":["Edit","Write"]}}"#).unwrap();
        seed_settings(&home).unwrap();
        let read = || serde_json::from_str::<Value>(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(read()["permissions"]["deny"], json!([]));
        // A rule the user adds back afterwards stays.
        std::fs::write(&settings, r#"{"permissions":{"deny":["Write"]}}"#).unwrap();
        seed_settings(&home).unwrap();
        assert_eq!(read()["permissions"]["deny"], json!(["Write"]));
        super::super::ensure_home(&home).unwrap();
        assert!(home.join("persona.md").exists());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn broken_settings_migrate_once_fixed() {
        let home = std::env::temp_dir().join(format!("fsc-home-test-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let settings = home.join(".claude/settings.local.json");
        // Broken JSON is left alone and not marked as migrated...
        std::fs::write(&settings, r#"{"permissions":{"deny":["Edit",]}}"#).unwrap();
        seed_settings(&home).unwrap();
        assert!(!home.join(SETTINGS_VERSION_FILE).exists());
        // ...so it's migrated once the user fixes it.
        std::fs::write(&settings, r#"{"permissions":{"deny":["Edit"]}}"#).unwrap();
        seed_settings(&home).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(v["permissions"]["deny"], json!([]));
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn stream_lines_become_events() {
        let mut pending = Vec::new();
        let mut parse = |v: Value| parse_line(&v.to_string(), &mut pending);

        let (e, _) = parse(json!({"type": "system", "subtype": "init", "session_id": "s", "model": "claude-x"}));
        assert!(matches!(e, Some(Event::Ready { model, .. }) if model == "claude-x"));
        let (e, _) = parse(json!({"type": "stream_event", "parent_tool_use_id": null,
            "event": {"type": "content_block_delta", "delta": {"type": "text_delta", "text": "hi"}}}));
        assert!(matches!(e, Some(Event::Text(t)) if t == "hi"));
        // A sub-agent's output isn't shown.
        let (e, _) = parse(json!({"type": "stream_event", "parent_tool_use_id": "t1",
            "event": {"type": "content_block_delta", "delta": {"type": "text_delta", "text": "no"}}}));
        assert!(e.is_none());
        let (e, _) = parse(json!({"type": "stream_event", "parent_tool_use_id": null,
            "event": {"type": "content_block_start", "content_block": {"type": "tool_use", "name": "Bash"}}}));
        assert!(matches!(e, Some(Event::ToolUse { name }) if name == "Bash"));

        // Permission asked, withdrawn; unknown requests are refused.
        let (e, r) = parse(json!({"type": "control_request", "request_id": "r1",
            "request": {"subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": "ls"}}}));
        assert!(matches!(e, Some(Event::Permission(a)) if a.id == "r1"));
        assert!(r.is_none());
        let (e, _) = parse(json!({"type": "control_cancel_request", "request_id": "r1"}));
        assert!(matches!(e, Some(Event::PermissionClosed { id }) if id == "r1"));
        let (e, _) = parse(json!({"type": "control_cancel_request", "request_id": "r1"}));
        assert!(e.is_none());
        let (e, r) = parse(json!({"type": "control_request", "request_id": "r2", "request": {"subtype": "mystery"}}));
        assert!(e.is_none());
        assert_eq!(r.unwrap()["response"]["subtype"], "error");

        let (e, _) = parse(json!({"type": "result", "subtype": "error_during_execution", "is_error": true}));
        assert!(matches!(e, Some(Event::TurnEnd { ok: false, error: Some(m) }) if m == "error_during_execution"));
        let (e, _) = parse(json!({"type": "result", "subtype": "success", "is_error": false, "result": "done"}));
        assert!(matches!(e, Some(Event::TurnEnd { ok: true, error: None })));
        assert!(parse_line("not json", &mut Vec::new()).0.is_none());
    }

    #[test]
    fn tool_names() {
        assert_eq!(pretty_tool("Bash"), "Bash");
        assert_eq!(pretty_tool("mcp__claude_ai_Google_Calendar__list_events"), "Google Calendar · list events");
    }
}

/// One real exchange with the installed `claude`, through the whole session
/// actor. Uses a little of your subscription, so it only runs when asked:
/// `cargo test -p fsc-core -- --ignored real_claude`.
#[cfg(test)]
mod real {
    use super::super::{AgentEvent, AgentHandle};
    use crate::config::ClaudeConfig;

    #[tokio::test]
    #[ignore]
    async fn real_claude_round_trip() {
        let home = std::env::temp_dir().join(format!("fsc-real-claude-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = crate::config::Config {
            claude: ClaudeConfig { model: "haiku".into(), ..Default::default() },
            agent: crate::config::AgentConfig { memory: Some(false), ..Default::default() },
            ..Default::default()
        };
        let agent = AgentHandle::spawn(&cfg, home.clone(), move |e| {
            let _ = tx.send(e);
        });
        agent.ask("Reply with just the word pong.");
        let mut text = String::new();
        let done = tokio::time::timeout(std::time::Duration::from_secs(90), async {
            while let Some(e) = rx.recv().await {
                match e {
                    AgentEvent::TextDelta(t) => text.push_str(&t),
                    AgentEvent::TurnDone { ok, error, .. } => return (ok, error),
                    AgentEvent::Exited { reason } => return (false, Some(reason)),
                    _ => {}
                }
            }
            (false, Some("events ended".into()))
        })
        .await
        .expect("no reply within 90 s");
        agent.end(false);
        let _ = std::fs::remove_dir_all(&home);
        assert_eq!(done, (true, None));
        assert!(text.to_lowercase().contains("pong"), "{text:?}");
    }
}
