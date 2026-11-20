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
use super::{Applied, Backend, Choice, Choices, Conn, Event, Select, Setting};
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
/// 3: also in the `settings.json` seeded before 0.5.0, whose denials won.
/// 4: opening a web page asks; reading `permission::SECRET_PATHS` is denied.
/// 5: the macOS ones among them too (`ADDED_IN_5`).
const SETTINGS_VERSION: u32 = 5;
/// The secret paths added in settings version 5. Only these are added to
/// a home at version 4, so one the user took out stays out.
const ADDED_IN_5: &[&str] = &[
    "~/Library/Keychains/**",
    "~/Library/Cookies/**",
    "~/Library/Safari/**",
    "~/Library/Mail/**",
    "~/Library/Messages/**",
    "~/Library/Application Support/Firefox/**",
    "~/Library/Application Support/librewolf/**",
    "~/Library/Application Support/zen/**",
    "~/Library/Application Support/Google/Chrome/**",
    "~/Library/Application Support/Chromium/**",
    "~/Library/Application Support/BraveSoftware/**",
    "~/Library/Application Support/Vivaldi/**",
    "~/Library/Application Support/Microsoft Edge/**",
    "~/Library/Application Support/Arc/**",
];
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
        let mut conn = start(&self.cfg, home, prompt)?;
        // Ask for the models this account can use; until the answer comes
        // the menu shows the usual three.
        if let Err(e) = conn.control_as(INIT_ID, json!({ "subtype": "initialize" })).await {
            eprintln!("fsc: could not ask claude for its models: {e}");
        }
        let fallback = choices(&conn.model, &conn.mode, None);
        Ok((conn, vec![Event::Choices(fallback)]))
    }

    fn remember(&mut self, which: Setting, value: &str) {
        match which {
            Setting::Model => self.cfg.model = value.into(),
            Setting::Mode => self.cfg.permission_mode = value.into(),
        }
    }
}

/// Request id of our `initialize`, whose answer lists the models.
const INIT_ID: &str = "fsc_init";

/// The models and permission modes on offer. `models`: as the CLI lists
/// them (`initialize`), else the usual aliases. Modes that skip questions
/// altogether (bypassPermissions, dontAsk) aren't offered: the user is asked.
fn choices(model: &str, mode: &str, models: Option<Vec<Choice>>) -> Choices {
    let fixed = |items: &[(&str, &str)]| -> Vec<Choice> {
        items.iter().map(|(id, name)| Choice { id: (*id).into(), name: (*name).into() }).collect()
    };
    let select = |mut options: Vec<Choice>, current: &str| {
        if !current.is_empty() && !options.iter().any(|o| o.id == current) {
            options.push(Choice { id: current.into(), name: current.into() });
        }
        Select { current: current.into(), options }
    };
    let models = models.filter(|m| !m.is_empty()).unwrap_or_else(|| fixed(&[("sonnet", "Sonnet"), ("opus", "Opus"), ("haiku", "Haiku")]));
    Choices {
        model: Some(select(models, model)),
        mode: Some(select(
            fixed(&[("default", "Ask"), ("acceptEdits", "Accept edits"), ("auto", "Auto"), ("plan", "Plan")]),
            if mode.is_empty() { "default" } else { mode },
        )),
    }
}

/// The models in an `initialize` answer: aliases ("opus", "fable") and
/// pinned versions, named as Claude Code names them ("Opus 5.5").
fn listed_models(response: &Value) -> Option<Vec<Choice>> {
    let list = response["models"].as_array()?;
    Some(
        list.iter()
            .filter_map(|m| {
                let id = m["value"].as_str()?;
                Some(Choice { id: id.into(), name: m["displayName"].as_str().unwrap_or(id).into() })
            })
            .collect(),
    )
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
            current = migrate_file(&settings, from)?;
            // Before 0.5.0 the lists were seeded into the shared settings.json,
            // and later versions never touched it. Claude Code still reads it,
            // and a denial anywhere beats an ask, so its file-edit denials
            // kept refusing Write outright.
            // Its allow list also let web pages be opened unasked.
            let legacy = home.join(".claude/settings.json");
            if legacy.exists() {
                current &= migrate_file(&legacy, if from < 3 { 1 } else { from })?;
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

/// Migrate one settings file in place. False if it isn't valid JSON and was
/// left alone.
fn migrate_file(path: &Path, from: u32) -> std::io::Result<bool> {
    let text = std::fs::read_to_string(path)?;
    match migrate_settings(&text, from) {
        Some(new) => std::fs::write(path, new).map(|()| true),
        None => {
            eprintln!("fsc: {} isn't valid JSON; left as it is", path.display());
            Ok(false)
        }
    }
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
    if from < 4 {
        // A page can carry instructions, so opening one asks; "always"
        // answers for one site (`WebFetch(domain:...)`) stay.
        if let Some(allow) = v.pointer_mut("/permissions/allow").and_then(Value::as_array_mut) {
            allow.retain(|r| r != "WebFetch");
        }
    }
    if from < 5 {
        // ...and whatever it says, keys and passwords stay out of reach.
        let perms = v.as_object_mut()?.entry("permissions").or_insert_with(|| json!({}));
        let deny = perms.as_object_mut()?.entry("deny").or_insert_with(|| json!([])).as_array_mut()?;
        let new = crate::permission::SECRET_PATHS.iter().filter(|p| from < 4 || ADDED_IN_5.contains(p));
        for path in new {
            let rule = Value::from(format!("Read({path})"));
            if !deny.contains(&rule) {
                deny.push(rule);
            }
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
    /// A prompt has been sent. Before that, a model set over the control
    /// channel is overridden when the session starts.
    prompted: bool,
    /// The model and permission mode in use, and the models on offer.
    model: String,
    mode: String,
    models: Option<Vec<Choice>>,
}

impl ClaudeConn {
    async fn control(&mut self, request: Value) -> std::io::Result<()> {
        self.req_id += 1;
        let id = format!("fsc_{}", self.req_id);
        self.control_as(&id, request).await
    }

    async fn control_as(&mut self, id: &str, request: Value) -> std::io::Result<()> {
        let msg = json!({ "type": "control_request", "request_id": id, "request": request });
        write_line(&mut self.stdin, &msg).await
    }

    /// The answer to our `initialize`, if `line` is it: the menu with the
    /// account's models.
    fn init_reply(&mut self, line: &str) -> Option<Event> {
        if !line.contains(INIT_ID) {
            return None;
        }
        let v: Value = serde_json::from_str(line).ok()?;
        let r = &v["response"];
        if v["type"] != "control_response" || r["request_id"] != INIT_ID || r["subtype"] != "success" {
            return None;
        }
        self.models = listed_models(&r["response"]);
        if let Some(mode) = r["response"]["current_permission_mode"].as_str() {
            self.mode = mode.to_string();
        }
        Some(Event::Choices(choices(&self.model, &self.mode, self.models.clone())))
    }
}

impl Conn for ClaudeConn {
    async fn recv(&mut self) -> std::io::Result<Option<String>> {
        self.lines.next_line().await
    }

    fn handle(&mut self, line: &str) -> Vec<Event> {
        if let Some(e) = self.init_reply(line) {
            return vec![e];
        }
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
        write_line(&mut self.stdin, &msg).await?;
        self.prompted = true;
        Ok(())
    }

    async fn interrupt(&mut self) -> std::io::Result<()> {
        self.control(json!({ "subtype": "interrupt" })).await
    }

    async fn set(&mut self, which: Setting, value: &str) -> Result<Applied, String> {
        if !self.prompted {
            return Ok(Applied::NextSession);
        }
        let request = match which {
            Setting::Model => json!({ "subtype": "set_model", "model": value }),
            Setting::Mode => json!({ "subtype": "set_permission_mode", "mode": value }),
        };
        self.control(request).await.map_err(|e| e.to_string())?;
        match which {
            Setting::Model => self.model = value.into(),
            Setting::Mode => self.mode = value.into(),
        }
        Ok(Applied::Live)
    }

    /// Later sessions are new processes, started from the backend's config.
    fn remember(&mut self, _: Setting, _: &str) {}

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
    crate::own_env::restore(cmd.as_std_mut());
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
    if !cfg.permission_mode.is_empty() {
        cmd.arg("--permission-mode").arg(&cfg.permission_mode);
    }
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
        // The app keeps its own memory (memory.md), the same for every agent.
        // Claude Code's auto-memory would be a second, separate one.
        .env("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1")
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
        prompted: false,
        model: cfg.model.clone(),
        mode: cfg.permission_mode.clone(),
        models: None,
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
            // Claude's own allow-list decides what runs without asking.
            Event::Permission { ask, auto: false }
        }
        Some("control_cancel_request") => {
            let id = s(&v["request_id"]);
            let Some(i) = pending.iter().position(|p| p.id == id) else { return (None, None) };
            pending.remove(i);
            Event::PermissionClosed { id }
        }
        Some("control_response") => {
            let r = &v["response"];
            if r["subtype"] == "error" {
                eprintln!("fsc: claude refused {}: {}", s(&r["request_id"]), s(&r["error"]));
            }
            return (None, None);
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

    /// A deny list without the `Read(...)` rules for secrets.
    fn non_secret(deny: &Value) -> Value {
        deny.as_array().unwrap().iter().filter(|r| !r.as_str().unwrap().starts_with("Read(~/")).cloned().collect()
    }

    #[test]
    fn migration_lets_file_edits_ask_and_keeps_user_rules() {
        let old = r#"{"permissions":{"allow":["Read","Bash(git status:*)"],
            "deny":["Bash(sudo:*)","Edit","Write","NotebookEdit"]},"model":"x"}"#;
        let v: Value = serde_json::from_str(&migrate_settings(old, 1).unwrap()).unwrap();
        assert_eq!(non_secret(&v["permissions"]["deny"]), json!(["Bash(sudo:*)"]));
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
        assert!(!v["permissions"]["allow"].as_array().unwrap().iter().any(|r| r == "WebFetch"));
        for path in crate::permission::SECRET_PATHS {
            assert!(deny.contains(&Value::from(format!("Read({path})"))), "seed doesn't deny reading {path}");
        }
        // A fresh seed migrates to itself.
        let again: Value = serde_json::from_str(&migrate_settings(SETTINGS, 1).unwrap()).unwrap();
        assert_eq!(again, v);
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
        assert_eq!(non_secret(&read()["permissions"]["deny"]), json!([]));
        // A rule the user adds back afterwards stays.
        std::fs::write(&settings, r#"{"permissions":{"deny":["Write"]}}"#).unwrap();
        seed_settings(&home).unwrap();
        assert_eq!(non_secret(&read()["permissions"]["deny"]), json!(["Write"]));
        super::super::ensure_home(&home).unwrap();
        assert!(home.join("persona.md").exists());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn legacy_shared_settings_lose_edit_denials() {
        let home = std::env::temp_dir().join(format!("fsc-home-test-legacy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let local = home.join(".claude/settings.local.json");
        let legacy = home.join(".claude/settings.json");
        std::fs::write(&local, r#"{"permissions":{"deny":["Bash(rm:*)"]}}"#).unwrap();
        std::fs::write(home.join(SETTINGS_VERSION_FILE), "2\n").unwrap();
        std::fs::write(
            &legacy,
            r#"{"permissions":{"allow":["Read","WebFetch"],"deny":["Bash(sudo:*)","Edit","Write","NotebookEdit"]}}"#,
        )
        .unwrap();
        seed_settings(&home).unwrap();
        let read = |p: &Path| serde_json::from_str::<Value>(&std::fs::read_to_string(p).unwrap()).unwrap();
        let deny = |p: &Path| read(p)["permissions"]["deny"].as_array().unwrap().clone();
        assert!(!deny(&legacy).iter().any(|r| r == "Edit" || r == "Write" || r == "NotebookEdit"));
        assert!(deny(&legacy).contains(&json!("Bash(sudo:*)")));
        assert_eq!(read(&legacy)["permissions"]["allow"], json!(["Read"]));
        assert!(deny(&local).contains(&json!("Bash(rm:*)")));
        assert!(deny(&local).contains(&json!("Read(~/.ssh/**)")));
        assert_eq!(std::fs::read_to_string(home.join(SETTINGS_VERSION_FILE)).unwrap().trim(), "5");
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn version_5_adds_only_the_macos_secrets() {
        for p in ADDED_IN_5 {
            assert!(crate::permission::SECRET_PATHS.contains(p), "{p} isn't in SECRET_PATHS");
        }
        // At version 4, with ~/.aws taken back out by the user.
        let v4 = r#"{"permissions":{"deny":["Read(~/.ssh/**)"]}}"#;
        let v: Value = serde_json::from_str(&migrate_settings(v4, 4).unwrap()).unwrap();
        let deny = v["permissions"]["deny"].as_array().unwrap();
        assert_eq!(deny.len(), 1 + ADDED_IN_5.len());
        assert!(deny.contains(&json!("Read(~/Library/Keychains/**)")));
        assert!(!deny.contains(&json!("Read(~/.aws/**)")));
    }

    #[test]
    fn web_pages_ask_and_secrets_are_denied_once() {
        let home = std::env::temp_dir().join(format!("fsc-home-test-v4-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let local = home.join(".claude/settings.local.json");
        std::fs::write(home.join(SETTINGS_VERSION_FILE), "3\n").unwrap();
        std::fs::write(&local, r#"{"permissions":{"allow":["Read","WebFetch","WebFetch(domain:weather.gov)"]}}"#).unwrap();
        seed_settings(&home).unwrap();
        let read = || serde_json::from_str::<Value>(&std::fs::read_to_string(&local).unwrap()).unwrap();
        assert_eq!(read()["permissions"]["allow"], json!(["Read", "WebFetch(domain:weather.gov)"]));
        assert_eq!(read()["permissions"]["deny"].as_array().unwrap().len(), crate::permission::SECRET_PATHS.len());
        // The user takes one back out: it isn't re-added.
        let mut v = read();
        v["permissions"]["deny"].as_array_mut().unwrap().retain(|r| r != "Read(~/.aws/**)");
        std::fs::write(&local, v.to_string()).unwrap();
        seed_settings(&home).unwrap();
        assert!(!read()["permissions"]["deny"].as_array().unwrap().contains(&json!("Read(~/.aws/**)")));
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
        assert_eq!(non_secret(&v["permissions"]["deny"]), json!([]));
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
        assert!(matches!(e, Some(Event::Permission { ask, .. }) if ask.id == "r1"));
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
    fn models_come_from_the_cli() {
        let init = json!({ "models": [
            { "value": "default", "displayName": "Default (recommended)" },
            { "value": "fable", "displayName": "Fable 5.2" },
            { "value": "claude-opus-4-8", "displayName": "Opus 4.8" } ] });
        let c = choices("sonnet", "", listed_models(&init));
        let m = c.model.unwrap();
        assert_eq!(m.options.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(), ["default", "fable", "claude-opus-4-8", "sonnet"]);
        assert_eq!(m.current_name(), "sonnet");
        assert_eq!(c.mode.unwrap().current, "default");
        // No list: the usual aliases.
        let c = choices("opus", "plan", None);
        assert_eq!(c.model.unwrap().current_name(), "Opus");
        assert_eq!(c.mode.unwrap().current_name(), "Plan");
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
