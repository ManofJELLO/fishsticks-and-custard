//! Agents that speak the Agent Client Protocol (ACP): JSON-RPC 2.0, one
//! message per line over the agent's stdin and stdout.
//!
//! ```text
//! → initialize                      ← capabilities, auth methods
//! → session/new {cwd, systemPrompt} ← sessionId, models, modes
//! → session/prompt {prompt}         ← session/update notifications (text, tool calls)
//!                                   ← session/request_permission → our answer
//!                                   ← the prompt's result: {stopReason}
//! → session/cancel (notification)   stops the turn; it still gets its result
//! ```
//!
//! One process serves many sessions, so rotating the conversation opens a
//! new session rather than restarting the agent. The persona goes in as a
//! system prompt where the agent offers one (the `systemPrompt` session
//! capability, or Antigravity's `_meta` equivalent), and otherwise in front
//! of the session's first prompt.
//!
//! The agent works in `<home>/workspace`, an empty folder of its own, not
//! the assistant's home. Agents let themselves write in their workspace
//! without asking (Antigravity does, whatever its mode), and the home holds
//! the assistant's memory, persona and settings, which should never change
//! behind the user's back.

use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, Lines};
use tokio::process::Child;

use super::{Backend, Conn, Event};
use crate::config::{AcpAgentConfig, AgentKind};
use crate::permission::{Answer, PermissionAsk, phrase, truncate};

/// Longest wait for the agent to start or open a session. Antigravity
/// signs in to its servers first.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_DETAIL: usize = 300;
/// JSON-RPC: the agent asked for something we don't offer.
const METHOD_NOT_FOUND: i64 = -32601;
/// ACP: the agent needs signing in.
const AUTH_REQUIRED: i64 = -32000;

/// How to run one kind of ACP agent.
pub(crate) struct Acp {
    program: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
    /// Sign-in command to suggest when the agent says it needs one.
    login: Option<&'static str>,
    /// What to install when the program can't be found.
    install: Option<&'static str>,
    settings: Settings,
}

/// What's applied to every new session.
#[derive(Clone, Default)]
struct Settings {
    model: String,
    mode: String,
    auto_allow: Vec<String>,
}

impl Acp {
    /// `kind`'s usual set-up, changed by its own config table.
    pub(crate) fn new(kind: AgentKind, cfg: &AcpAgentConfig, auto_allow: &[String]) -> Self {
        let (program, mut args, login, install, mode): (&str, Vec<String>, _, _, &str) = match kind {
            AgentKind::Codex => (
                "codex-acp",
                Vec::new(),
                Some("codex login"),
                Some("codex-acp (npm i -g @agentclientprotocol/codex-acp)"),
                // Ask before edits, commands and network use.
                "read-only",
            ),
            AgentKind::Antigravity => ("agy", vec!["acp".into()], Some("agy acp --login"), Some("antigravity-cli"), ""),
            AgentKind::Acp | AgentKind::Claude => ("", Vec::new(), None, None, ""),
        };
        let program = if cfg.command.is_empty() {
            program.to_string()
        } else {
            // Your own command replaces the preset's, arguments and all.
            args.clear();
            cfg.command.clone()
        };
        args.extend(cfg.args.iter().cloned());
        Self {
            program,
            args,
            env: cfg.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            login,
            install,
            settings: Settings {
                model: cfg.model.clone(),
                mode: if cfg.mode.is_empty() { mode.into() } else { cfg.mode.clone() },
                auto_allow: auto_allow.to_vec(),
            },
        }
    }
}

impl Backend for Acp {
    type Conn = AcpConn;

    fn program(&self) -> &str {
        &self.program
    }

    fn prepare(&self, home: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(workspace(home))
    }

    async fn start(&self, home: &Path, prompt: &str) -> Result<(AcpConn, Vec<Event>), String> {
        if self.program.is_empty() {
            return Err("no agent command: set `command` in [acp]".into());
        }
        let cwd = workspace(home);
        std::fs::create_dir_all(&cwd).map_err(|e| format!("could not create {}: {e}", cwd.display()))?;
        let mut cmd = tokio::process::Command::new(&self.program);
        cmd.args(&self.args)
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| match (e.kind(), self.install) {
            (std::io::ErrorKind::NotFound, Some(what)) => format!("not installed; install {what}"),
            _ => e.to_string(),
        })?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err("no pipes to the agent".into());
        };
        let stderr = Arc::new(Mutex::new(VecDeque::new()));
        if let Some(err) = child.stderr.take() {
            let buf = stderr.clone();
            let name = self.program.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(err).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    eprintln!("{name}: {l}");
                    let mut b = buf.lock().unwrap();
                    b.push_back(l);
                    if b.len() > 5 {
                        b.pop_front();
                    }
                }
            });
        }
        let mut conn = AcpConn::new(Box::new(stdout), Box::new(stdin), self.settings.clone());
        conn.child = Some(child);
        conn.stderr = stderr;
        conn.program = self.program.clone();
        conn.login = self.login;
        match conn.open(&cwd, prompt).await {
            Ok(events) => Ok((conn, events)),
            Err(e) => {
                let reason = conn.close().await;
                // A crash says more than the timeout or broken pipe that followed it.
                Err(if e.starts_with("the agent") { reason } else { e })
            }
        }
    }
}

type Reader = Lines<BufReader<Box<dyn AsyncRead + Unpin + Send>>>;
type Writer = Box<dyn AsyncWrite + Unpin + Send>;

/// A permission question the agent is waiting on.
struct Pending {
    /// Our id for it (the JSON-RPC id, as text).
    id: String,
    rpc_id: Value,
    /// (optionId, kind) pairs the agent offered.
    options: Vec<(String, String)>,
}

pub(crate) struct AcpConn {
    child: Option<Child>,
    program: String,
    login: Option<&'static str>,
    stdin: Writer,
    lines: Reader,
    stderr: Arc<Mutex<VecDeque<String>>>,
    settings: Settings,
    /// The agent's working folder.
    cwd: PathBuf,
    /// From `initialize`: what the agent can do.
    caps: Value,
    session: String,
    /// The persona, for the front of the next prompt, when the agent has no
    /// system prompt to put it in.
    preamble: Option<String>,
    next_id: u64,
    /// The running `session/prompt` request.
    prompt_id: Option<u64>,
    pending: Vec<Pending>,
    outbox: Vec<Value>,
    /// A tool ran since the last reply text: the next text is a new block.
    after_tool: bool,
}

impl AcpConn {
    fn new(reader: Box<dyn AsyncRead + Unpin + Send>, writer: Writer, settings: Settings) -> Self {
        Self {
            child: None,
            program: "the agent".into(),
            login: None,
            stdin: writer,
            lines: BufReader::new(reader).lines(),
            stderr: Default::default(),
            settings,
            cwd: PathBuf::new(),
            caps: Value::Null,
            session: String::new(),
            preamble: None,
            next_id: 0,
            prompt_id: None,
            pending: Vec::new(),
            outbox: Vec::new(),
            after_tool: false,
        }
    }

    /// `initialize`, then the first session, working in `cwd`.
    async fn open(&mut self, cwd: &Path, prompt: &str) -> Result<Vec<Event>, String> {
        self.cwd = cwd.to_path_buf();
        let init = self
            .call(
                "initialize",
                json!({
                    "protocolVersion": 1,
                    // The agent uses its own file and terminal tools.
                    "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
                    "clientInfo": { "name": "fsc", "title": "Fishsticks & Custard", "version": env!("CARGO_PKG_VERSION") },
                }),
            )
            .await?;
        if init["protocolVersion"].as_u64() != Some(1) {
            return Err(format!("unsupported ACP version {}", init["protocolVersion"]));
        }
        self.caps = init["agentCapabilities"].clone();
        self.session_new(prompt).await
    }

    async fn session_new(&mut self, prompt: &str) -> Result<Vec<Event>, String> {
        let mut params = json!({ "cwd": self.cwd, "mcpServers": [] });
        let standard = !self.caps.pointer("/sessionCapabilities/systemPrompt").is_none_or(Value::is_null);
        let antigravity = self.caps.pointer("/_meta/antigravity/systemPrompt/append") == Some(&json!(true));
        if standard {
            params["systemPrompt"] = json!({ "mode": "append", "content": [{ "type": "text", "text": prompt }] });
        }
        if antigravity {
            params["_meta"] = json!({ "antigravity": { "systemPrompt": { "append": prompt } } });
        }
        self.preamble = (!standard && !antigravity).then(|| prompt.to_string());
        let r = self.call("session/new", params).await?;
        self.session = r["sessionId"].as_str().ok_or("no session id from the agent")?.to_string();
        self.after_tool = false;

        let s = self.settings.clone();
        if !s.mode.is_empty() {
            self.set(&r, "mode", &s.mode, "session/set_mode", "modeId").await;
        }
        if !s.model.is_empty() {
            self.set(&r, "model", &s.model, "session/set_model", "modelId").await;
        }
        let model = if s.model.is_empty() { current_model(&r) } else { s.model };
        Ok(vec![Event::Ready { session_id: self.session.clone(), model }])
    }

    /// Choose a session setting: by config option where the agent has one
    /// for it, else by its dedicated method. Failures are only logged.
    async fn set(&mut self, session: &Value, category: &str, value: &str, method: &str, key: &str) {
        let option = session["configOptions"]
            .as_array()
            .and_then(|o| o.iter().find(|o| o["category"] == category || o["id"] == category))
            .and_then(|o| o["id"].as_str())
            .map(str::to_string);
        let sid = self.session.clone();
        let r = match option {
            Some(id) => self.call("session/set_config_option", json!({ "sessionId": sid, "configId": id, "value": value })).await,
            None => self.call(method, json!({ "sessionId": sid, key: value })).await,
        };
        if let Err(e) = r {
            eprintln!("fsc: could not set the agent's {category} to {value:?}: {e}");
        }
    }

    async fn write(&mut self, msg: &Value) -> std::io::Result<()> {
        let mut s = msg.to_string();
        s.push('\n');
        self.stdin.write_all(s.as_bytes()).await?;
        self.stdin.flush().await
    }

    async fn request(&mut self, method: &str, params: Value) -> std::io::Result<u64> {
        self.next_id += 1;
        let id = self.next_id;
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).await?;
        Ok(id)
    }

    /// Send a request and wait for its result. Only for setting up
    /// sessions, while no turn is running: anything else that arrives
    /// meanwhile is answered or dropped.
    async fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let gone = |e: std::io::Error| format!("the agent went away ({e})");
        let id = self.request(method, params).await.map_err(gone)?;
        let wait = async {
            loop {
                let Some(line) = self.lines.next_line().await.map_err(gone)? else {
                    return Err("the agent exited".to_string());
                };
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                if v.get("method").is_some() {
                    if let Some(reply) = refuse_request(&v) {
                        self.write(&reply).await.map_err(gone)?;
                    }
                    continue;
                }
                if v["id"] == json!(id) {
                    return match v.get("error") {
                        Some(e) => Err(self.describe(e)),
                        None => Ok(v["result"].clone()),
                    };
                }
            }
        };
        tokio::time::timeout(HANDSHAKE_TIMEOUT, wait)
            .await
            .map_err(|_| format!("the agent didn't answer {method} in {} s", HANDSHAKE_TIMEOUT.as_secs()))?
    }

    /// A JSON-RPC error, put for the user.
    fn describe(&self, e: &Value) -> String {
        let msg = e["message"].as_str().unwrap_or("unknown error");
        let detail = e["data"]["message"].as_str().or(e["data"].as_str()).filter(|d| *d != msg);
        let mut text = match detail {
            Some(d) => format!("{msg}: {d}"),
            None => msg.to_string(),
        };
        if (e["code"].as_i64() == Some(AUTH_REQUIRED) || msg.to_lowercase().contains("auth"))
            && let Some(cmd) = self.login
        {
            text = format!("not signed in; run `{cmd}` ({text})");
        }
        text
    }

    /// Answer every open permission question "cancelled", as ACP asks once
    /// a turn is cancelled.
    fn cancel_pending(&mut self) {
        for p in std::mem::take(&mut self.pending) {
            self.outbox.push(json!({ "jsonrpc": "2.0", "id": p.rpc_id, "result": { "outcome": { "outcome": "cancelled" } } }));
        }
    }

    fn on_request(&mut self, v: &Value) -> Vec<Event> {
        if v["method"] != "session/request_permission" || v["params"]["sessionId"] != self.session.as_str() {
            self.outbox.extend(refuse_request(v));
            return Vec::new();
        }
        let call = &v["params"]["toolCall"];
        let options: Vec<(String, String)> = v["params"]["options"]
            .as_array()
            .map(|o| {
                o.iter()
                    .filter_map(|o| Some((o["optionId"].as_str()?.to_string(), o["kind"].as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        let kind = call["kind"].as_str().unwrap_or("other");
        let rpc = v["id"].as_str().map(str::to_string).unwrap_or_else(|| v["id"].to_string());
        let pending = Pending { id: format!("acp-{rpc}"), rpc_id: v["id"].clone(), options };
        if self.settings.auto_allow.iter().any(|k| k == kind)
            && let Some(opt) = pick(&pending.options, &["allow_once"])
        {
            eprintln!("fsc: auto-allowed a {kind} tool");
            self.outbox.push(selected(&pending.rpc_id, &opt));
            return Vec::new();
        }
        let ask = describe_tool(&pending.id, call, pending.options.iter().any(|(_, k)| k == "allow_always"));
        self.pending.push(pending);
        vec![Event::Permission(ask)]
    }

    fn on_update(&mut self, params: &Value) -> Vec<Event> {
        // Late output from a session we've moved on from.
        if params["sessionId"] != self.session.as_str() {
            return Vec::new();
        }
        let u = &params["update"];
        match u["sessionUpdate"].as_str() {
            Some("agent_message_chunk") => {
                let Some(text) = u["content"]["text"].as_str().filter(|_| u["content"]["type"] == "text") else {
                    return Vec::new();
                };
                let mut out = Vec::new();
                if std::mem::take(&mut self.after_tool) {
                    out.push(Event::TextStart);
                }
                out.push(Event::Text(text.to_string()));
                out
            }
            Some("tool_call") => {
                self.after_tool = true;
                let name = u["title"].as_str().or(u["kind"].as_str()).unwrap_or("a tool");
                vec![Event::ToolUse { name: name.to_string() }]
            }
            _ => Vec::new(),
        }
    }

    fn on_response(&mut self, v: &Value) -> Vec<Event> {
        if self.prompt_id.is_none() || v["id"] != json!(self.prompt_id) {
            if let Some(e) = v.get("error") {
                eprintln!("fsc: the agent refused a request: {}", self.describe(e));
            }
            return Vec::new();
        }
        self.prompt_id = None;
        self.after_tool = false;
        self.cancel_pending();
        let end = match v.get("error") {
            Some(e) => Event::TurnEnd { ok: false, error: Some(self.describe(e)) },
            None => match v["result"]["stopReason"].as_str() {
                Some("refusal") => Event::TurnEnd { ok: false, error: Some("the agent declined to answer".into()) },
                _ => Event::TurnEnd { ok: true, error: None },
            },
        };
        vec![end]
    }
}

impl Conn for AcpConn {
    async fn recv(&mut self) -> std::io::Result<Option<String>> {
        self.lines.next_line().await
    }

    fn handle(&mut self, msg: &str) -> Vec<Event> {
        let Ok(v) = serde_json::from_str::<Value>(msg) else { return Vec::new() };
        match (v.get("method"), v.get("id")) {
            (Some(_), Some(_)) => self.on_request(&v),
            (Some(m), None) if m == "session/update" => self.on_update(&v["params"]),
            (Some(_), None) => Vec::new(),
            (None, Some(_)) => self.on_response(&v),
            (None, None) => Vec::new(),
        }
    }

    async fn flush(&mut self) {
        for msg in std::mem::take(&mut self.outbox) {
            if let Err(e) = self.write(&msg).await {
                eprintln!("fsc: could not answer the agent: {e}");
            }
        }
    }

    async fn prompt(&mut self, text: &str) -> std::io::Result<()> {
        let text = match self.preamble.take() {
            Some(p) => format!("{p}\n\n---\n\n{text}"),
            None => text.to_string(),
        };
        let params = json!({ "sessionId": self.session, "prompt": [{ "type": "text", "text": text }] });
        self.prompt_id = Some(self.request("session/prompt", params).await?);
        Ok(())
    }

    async fn interrupt(&mut self) -> std::io::Result<()> {
        let msg = json!({ "jsonrpc": "2.0", "method": "session/cancel", "params": { "sessionId": self.session } });
        self.write(&msg).await?;
        self.cancel_pending();
        self.flush().await;
        Ok(())
    }

    fn answer(&mut self, id: &str, answer: Answer) -> bool {
        let Some(i) = self.pending.iter().position(|p| p.id == id) else { return false };
        let p = self.pending.remove(i);
        let kinds: &[&str] = match answer {
            Answer::Once => &["allow_once"],
            Answer::Always => &["allow_always", "allow_once"],
            Answer::Deny => &["reject_once", "reject_always"],
        };
        self.outbox.push(match pick(&p.options, kinds) {
            Some(opt) => selected(&p.rpc_id, &opt),
            // Nothing that fits: don't guess at an allow.
            None => json!({ "jsonrpc": "2.0", "id": p.rpc_id, "result": { "outcome": { "outcome": "cancelled" } } }),
        });
        true
    }

    fn deny_all(&mut self) {
        let ids: Vec<String> = self.pending.iter().map(|p| p.id.clone()).collect();
        for id in ids {
            self.answer(&id, Answer::Deny);
        }
    }

    async fn new_session(&mut self, _home: &Path, prompt: &str) -> Result<Vec<Event>, String> {
        let old = std::mem::take(&mut self.session);
        if self.prompt_id.take().is_some() {
            let _ = self.write(&json!({ "jsonrpc": "2.0", "method": "session/cancel", "params": { "sessionId": old } })).await;
        }
        self.cancel_pending();
        self.flush().await;
        if self.caps.pointer("/sessionCapabilities/close").is_some() {
            // Free the old one; its answer is ignored.
            let _ = self.request("session/close", json!({ "sessionId": old })).await;
        }
        self.session_new(prompt).await
    }

    async fn close(mut self) -> String {
        let mut reason = format!("{} exited", self.program);
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            if let Ok(status) = child.wait().await {
                reason = format!("{} exited ({status})", self.program);
            }
        }
        let tail: Vec<String> = self.stderr.lock().unwrap().iter().cloned().collect();
        if !tail.is_empty() {
            reason = format!("{reason}: {}", tail.join(" | "));
        }
        reason
    }
}

/// The reply to an agent request we don't handle: refuse a permission
/// question we can't place, and say other methods don't exist (we offered
/// no file system or terminal).
/// Where ACP agents work: see the module docs.
fn workspace(home: &Path) -> PathBuf {
    home.join("workspace")
}

fn refuse_request(v: &Value) -> Option<Value> {
    let id = v.get("id")?;
    Some(if v["method"] == "session/request_permission" {
        json!({ "jsonrpc": "2.0", "id": id, "result": { "outcome": { "outcome": "cancelled" } } })
    } else {
        json!({ "jsonrpc": "2.0", "id": id, "error": { "code": METHOD_NOT_FOUND, "message": "not supported by this client" } })
    })
}

/// The first option of the first kind in `kinds` that's on offer.
fn pick(options: &[(String, String)], kinds: &[&str]) -> Option<String> {
    kinds.iter().find_map(|k| options.iter().find(|(_, kind)| kind == k).map(|(id, _)| id.clone()))
}

fn selected(rpc_id: &Value, option: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": rpc_id, "result": { "outcome": { "outcome": "selected", "optionId": option } } })
}

/// The model a new session runs: its model list's current one, else its
/// model config option's.
fn current_model(session: &Value) -> String {
    session["models"]["currentModelId"]
        .as_str()
        .or_else(|| {
            session["configOptions"]
                .as_array()?
                .iter()
                .find(|o| o["category"] == "model")?["currentValue"]
                .as_str()
        })
        .unwrap_or_default()
        .to_string()
}

/// Put an ACP tool call as a permission question.
fn describe_tool(id: &str, call: &Value, can_always: bool) -> PermissionAsk {
    let title = call["title"].as_str().map(str::trim).filter(|t| !t.is_empty());
    let input = &call["rawInput"];
    // codex-acp says "command", Antigravity "CommandLine".
    let command = match input.get("command").unwrap_or(&input["CommandLine"]) {
        Value::String(s) => Some(s.clone()),
        Value::Array(a) => Some(a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")),
        _ => None,
    };
    let path = call["locations"][0]["path"].as_str().map(str::to_string);
    let url = input["url"].as_str().map(str::to_string);
    let (tool, action, detail) = match call["kind"].as_str().unwrap_or("other") {
        "execute" => ("Command", "run a command".to_string(), command.or(title.map(String::from))),
        "edit" => ("Edit", "change a file".into(), path.or(title.map(String::from))),
        "delete" => ("Delete", "delete a file".into(), path.or(title.map(String::from))),
        "move" => ("Move", "move a file".into(), title.map(String::from)),
        "read" => ("Read", "read a file".into(), path.or(title.map(String::from))),
        "search" => ("Search", "search".into(), title.map(String::from)),
        "fetch" => ("Web", "open a web page".into(), url.or(title.map(String::from))),
        "think" => ("Think", "think it through".into(), None),
        _ => ("Tool", title.map(phrase).unwrap_or_else(|| "use a tool".into()), None),
    };
    PermissionAsk {
        id: id.to_string(),
        tool: tool.into(),
        spoken: format!("May I {action}?"),
        action,
        detail: detail.map(|d| truncate(&d, MAX_DETAIL)),
        can_always,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// The agent's end of the pipe: read what the client sent, reply.
    struct FakeAgent {
        lines: Lines<BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>,
        out: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    }

    impl FakeAgent {
        async fn read(&mut self) -> Value {
            let line = tokio::time::timeout(Duration::from_secs(2), self.lines.next_line()).await.unwrap().unwrap().unwrap();
            serde_json::from_str(&line).unwrap()
        }
        async fn send(&mut self, v: Value) {
            self.out.write_all(format!("{v}\n").as_bytes()).await.unwrap();
        }
        async fn reply(&mut self, id: &Value, result: Value) {
            self.send(json!({ "jsonrpc": "2.0", "id": id, "result": result })).await;
        }
        async fn update(&mut self, sid: &str, u: Value) {
            self.send(json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": sid, "update": u } }))
                .await;
        }
        /// initialize + session/new, checking the system prompt went in.
        async fn handshake(&mut self, caps: Value, sid: &str) -> Value {
            let init = self.read().await;
            assert_eq!(init["method"], "initialize");
            self.reply(&init["id"], json!({ "protocolVersion": 1, "agentCapabilities": caps })).await;
            let new = self.read().await;
            assert_eq!(new["method"], "session/new");
            self.reply(&new["id"], json!({ "sessionId": sid, "models": { "currentModelId": "fake-1" } })).await;
            new
        }
    }

    fn pair(settings: Settings) -> (AcpConn, FakeAgent) {
        let (client, agent) = tokio::io::duplex(1 << 16);
        let (cr, cw) = tokio::io::split(client);
        let (ar, aw) = tokio::io::split(agent);
        (AcpConn::new(Box::new(cr), Box::new(cw), settings), FakeAgent { lines: BufReader::new(ar).lines(), out: aw })
    }

    fn settings() -> Settings {
        Settings { auto_allow: vec!["read".into()], ..Default::default() }
    }

    /// Read and handle messages until `n` events have come out.
    async fn events(c: &mut AcpConn, n: usize) -> Vec<Event> {
        let mut out = Vec::new();
        while out.len() < n {
            let line = tokio::time::timeout(Duration::from_secs(2), c.recv()).await.unwrap().unwrap().unwrap();
            out.extend(c.handle(&line));
            c.flush().await;
        }
        out
    }

    #[tokio::test]
    async fn a_turn_with_tools_and_a_question() {
        let (mut c, mut a) = pair(settings());
        let home = Path::new("/tmp");
        let (opened, new) = tokio::join!(
            c.open(home, "You are Custard."),
            a.handshake(json!({ "sessionCapabilities": { "systemPrompt": {} } }), "s1")
        );
        assert!(matches!(&opened.unwrap()[..], [Event::Ready { model, .. }] if model == "fake-1"));
        assert_eq!(new["params"]["systemPrompt"]["content"][0]["text"], "You are Custard.");
        assert_eq!(new["params"]["cwd"], "/tmp");

        c.prompt("hello").await.unwrap();
        let p = a.read().await;
        assert_eq!(p["method"], "session/prompt");
        assert_eq!(p["params"]["prompt"][0]["text"], "hello");

        a.update("s1", json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "Let me look." } }))
            .await;
        // Another session's output is ignored.
        a.update("old", json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "stale" } }))
            .await;
        a.update("s1", json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Read notes.txt", "kind": "read" }))
            .await;
        let opts = json!([{ "optionId": "y", "name": "Yes", "kind": "allow_once" },
                          { "optionId": "yy", "name": "Always", "kind": "allow_always" },
                          { "optionId": "n", "name": "No", "kind": "reject_once" }]);
        // A read is allowed without asking...
        a.send(json!({ "jsonrpc": "2.0", "id": 0, "method": "session/request_permission",
            "params": { "sessionId": "s1", "options": opts, "toolCall": { "toolCallId": "t1", "kind": "read", "title": "Read notes.txt" } } }))
            .await;
        // ...a command is asked about.
        a.send(json!({ "jsonrpc": "2.0", "id": 1, "method": "session/request_permission",
            "params": { "sessionId": "s1", "options": opts, "toolCall": { "toolCallId": "t2", "kind": "execute",
                "title": "printf hi > hello.txt", "rawInput": { "command": "printf hi > hello.txt" } } } }))
            .await;
        let ev = events(&mut c, 3).await;
        assert!(matches!(&ev[0], Event::Text(t) if t == "Let me look."));
        assert!(matches!(&ev[1], Event::ToolUse { name } if name == "Read notes.txt"));
        let Event::Permission(ask) = &ev[2] else { panic!("{ev:?}") };
        assert_eq!(ask.tool, "Command");
        assert_eq!(ask.spoken, "May I run a command?");
        assert_eq!(ask.detail.as_deref(), Some("printf hi > hello.txt"));
        assert!(ask.can_always);
        let auto = a.read().await;
        assert_eq!((auto["id"].clone(), auto["result"]["outcome"]["optionId"].clone()), (json!(0), json!("y")));

        assert!(c.answer(&ask.id, Answer::Always));
        assert!(!c.answer(&ask.id, Answer::Deny));
        c.flush().await;
        let r = a.read().await;
        assert_eq!((r["id"].clone(), r["result"]["outcome"]["optionId"].clone()), (json!(1), json!("yy")));

        // Text after a tool starts a new block.
        a.update("s1", json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "Done." } })).await;
        a.reply(&p["id"], json!({ "stopReason": "end_turn" })).await;
        let ev = events(&mut c, 3).await;
        assert!(matches!(&ev[..], [Event::TextStart, Event::Text(_), Event::TurnEnd { ok: true, error: None }]));
    }

    #[tokio::test]
    async fn cancelling_and_new_sessions() {
        let (mut c, mut a) = pair(settings());
        let home = Path::new("/tmp");
        // No system prompt on offer: the persona leads the first prompt.
        let caps = json!({ "sessionCapabilities": { "close": {} } });
        let (opened, new) = tokio::join!(c.open(home, "You are Custard."), a.handshake(caps, "s1"));
        opened.unwrap();
        assert!(new["params"].get("systemPrompt").is_none());

        c.prompt("hi").await.unwrap();
        let p = a.read().await;
        assert!(p["params"]["prompt"][0]["text"].as_str().unwrap().starts_with("You are Custard."));
        a.send(json!({ "jsonrpc": "2.0", "id": "q", "method": "session/request_permission",
            "params": { "sessionId": "s1", "options": [{ "optionId": "y", "kind": "allow_once" }],
                        "toolCall": { "kind": "edit", "title": "Edit a.txt", "locations": [{ "path": "/tmp/a.txt" }] } } }))
            .await;
        let ev = events(&mut c, 1).await;
        assert!(matches!(&ev[0], Event::Permission(ask) if ask.detail.as_deref() == Some("/tmp/a.txt")));
        let ask = describe_tool("x", &json!({ "kind": "execute", "title": "uname", "rawInput": { "CommandLine": "uname -r" } }), false);
        assert_eq!(ask.detail.as_deref(), Some("uname -r"));

        // Stop: cancel the turn, and the open question is cancelled.
        c.interrupt().await.unwrap();
        assert_eq!(a.read().await["method"], "session/cancel");
        assert_eq!(a.read().await["result"]["outcome"]["outcome"], "cancelled");
        a.reply(&p["id"], json!({ "stopReason": "cancelled" })).await;
        assert!(matches!(&events(&mut c, 1).await[..], [Event::TurnEnd { ok: true, .. }]));

        // A new session: the old one is closed, the persona leads again.
        let (renewed, ()) = tokio::join!(c.new_session(home, "You are Custard."), async {
            let close = a.read().await;
            assert_eq!(close["method"], "session/close");
            assert_eq!(close["params"]["sessionId"], "s1");
            let new = a.read().await;
            assert_eq!(new["method"], "session/new");
            a.reply(&new["id"], json!({ "sessionId": "s2" })).await;
        });
        renewed.unwrap();
        c.prompt("again").await.unwrap();
        assert!(a.read().await["params"]["prompt"][0]["text"].as_str().unwrap().starts_with("You are Custard."));
    }

    #[tokio::test]
    async fn errors_are_explained() {
        let (mut c, mut a) = pair(settings());
        c.login = Some("agy acp --login");
        let (opened, ()) = tokio::join!(c.open(Path::new("/tmp"), "p"), async {
            let init = a.read().await;
            a.reply(&init["id"], json!({ "protocolVersion": 1, "agentCapabilities": {} })).await;
            let new = a.read().await;
            a.send(json!({ "jsonrpc": "2.0", "id": new["id"], "error": { "code": -32000, "message": "Authentication required" } }))
                .await;
        });
        assert_eq!(opened.unwrap_err(), "not signed in; run `agy acp --login` (Authentication required)");

        // Requests we don't serve are refused rather than left hanging.
        let r = refuse_request(&json!({ "jsonrpc": "2.0", "id": 5, "method": "fs/read_text_file", "params": {} })).unwrap();
        assert_eq!(r["error"]["code"], METHOD_NOT_FOUND);
    }

    #[test]
    fn presets() {
        let mut cfg = AcpAgentConfig::default();
        let a = Acp::new(AgentKind::Antigravity, &cfg, &[]);
        assert_eq!((a.program.as_str(), a.args.clone()), ("agy", vec!["acp".to_string()]));
        let a = Acp::new(AgentKind::Codex, &cfg, &[]);
        assert_eq!((a.program.as_str(), a.settings.mode.as_str()), ("codex-acp", "read-only"));
        cfg.command = "/opt/my-agent".into();
        cfg.args = vec!["--acp".into()];
        cfg.mode = "auto".into();
        let a = Acp::new(AgentKind::Codex, &cfg, &[]);
        assert_eq!((a.program.as_str(), a.args.clone(), a.settings.mode.as_str()), ("/opt/my-agent", vec!["--acp".to_string()], "auto"));
    }
}

/// Real agents, through the whole session actor: a turn, a new session in
/// the same process, another turn. Uses a little of your quota, so only
/// when asked: `cargo test -p fsc-core -- --ignored real_acp`.
#[cfg(test)]
mod real {
    use super::super::{AgentEvent, AgentHandle};
    use crate::config::{AgentKind, Config};

    async fn round_trip(kind: AgentKind) {
        let home = std::env::temp_dir().join(format!("fsc-real-acp-{kind:?}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut cfg = Config::default();
        cfg.agent.backend = kind;
        cfg.agent.memory = Some(false);
        let agent = AgentHandle::spawn(&cfg, home.clone(), move |e| {
            let _ = tx.send(e);
        });
        let mut turn = async |q: &str| {
            agent.ask(q);
            let mut text = String::new();
            let mut model = String::new();
            tokio::time::timeout(std::time::Duration::from_secs(120), async {
                while let Some(e) = rx.recv().await {
                    eprintln!("{kind:?}: {e:?}");
                    match e {
                        AgentEvent::Ready { model: m, .. } => model = m,
                        AgentEvent::TextDelta(t) => text.push_str(&t),
                        AgentEvent::TurnDone { ok: true, .. } => return,
                        AgentEvent::TurnDone { error, .. } => panic!("turn failed: {error:?}"),
                        AgentEvent::Exited { reason } => panic!("agent exited: {reason}"),
                        _ => {}
                    }
                }
            })
            .await
            .expect("no reply within 120 s");
            (text, model)
        };
        let (text, model) = turn("What's your name? Reply with just the name.").await;
        assert!(!model.is_empty());
        // The persona went in as the system prompt.
        assert!(text.to_lowercase().contains("fishsticks"), "{text:?}");
        agent.rotate(false);
        let (text, _) = turn("Reply with just the word pong.").await;
        assert!(text.to_lowercase().contains("pong"), "{text:?}");
        agent.end(false);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Changing the assistant's own files must be asked about. Refused
    /// here, so the file must be unchanged.
    async fn asks_before_editing_home(kind: AgentKind) {
        // Not under /tmp: Antigravity writes there without asking.
        let cache = directories::BaseDirs::new().unwrap().cache_dir().to_path_buf();
        let home = cache.join(format!("fsc-real-acp-ask-{kind:?}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let memory = home.join("memory.md");
        std::fs::write(&memory, "- old fact\n").unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut cfg = Config::default();
        cfg.agent.backend = kind;
        cfg.agent.memory = Some(false);
        let agent = AgentHandle::spawn(&cfg, home.clone(), move |e| {
            let _ = tx.send(e);
        });
        agent.ask(format!("Append the line '- likes jam' to the file {}. Then say done.", memory.display()));
        let mut asked = Vec::new();
        let mut text = String::new();
        tokio::time::timeout(std::time::Duration::from_secs(150), async {
            while let Some(e) = rx.recv().await {
                if !matches!(e, AgentEvent::TextDelta(_)) {
                    eprintln!("{kind:?}: {e:?}");
                }
                match e {
                    AgentEvent::TextDelta(t) => text.push_str(&t),
                    AgentEvent::Permission(ask) => {
                        agent.answer(&ask.id, crate::permission::Answer::Deny);
                        asked.push(ask.tool);
                    }
                    AgentEvent::TurnDone { .. } | AgentEvent::Exited { .. } => return,
                    _ => {}
                }
            }
        })
        .await
        .expect("no end of turn within 150 s");
        agent.end(false);
        let after = std::fs::read_to_string(&memory).unwrap();
        let _ = std::fs::remove_dir_all(&home);
        eprintln!("{kind:?} said: {text}");
        assert!(!asked.is_empty(), "never asked");
        assert_eq!(after, "- old fact\n");
    }

    #[tokio::test]
    #[ignore]
    async fn real_acp_codex_asks() {
        asks_before_editing_home(AgentKind::Codex).await;
    }

    #[tokio::test]
    #[ignore]
    async fn real_acp_antigravity_asks() {
        asks_before_editing_home(AgentKind::Antigravity).await;
    }

    #[tokio::test]
    #[ignore]
    async fn real_acp_codex() {
        round_trip(AgentKind::Codex).await;
    }

    #[tokio::test]
    #[ignore]
    async fn real_acp_antigravity() {
        round_trip(AgentKind::Antigravity).await;
    }
}
