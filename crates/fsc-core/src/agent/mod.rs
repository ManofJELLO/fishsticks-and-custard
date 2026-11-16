//! The assistant's agent session, whichever agent runs it.
//!
//! One long-lived agent process holds the conversation. The session actor
//! here owns everything that doesn't depend on the agent: sending prompts,
//! interrupting, permission questions, rotating to a fresh session (after
//! idle time, `fsc new` or quit), saving long-term memory from a session
//! before it ends, and holding prompts that arrive meanwhile.
//!
//! A [`Backend`] starts the agent and speaks its protocol, turning its output
//! into [`Event`]s:
//! - [`claude`]: Claude Code over its native stream-json protocol.
//! - [`acp`]: any agent that speaks the Agent Client Protocol (Codex through
//!   codex-acp, Antigravity's `agy acp`, ...).

mod acp;
mod claude;
mod claude_ask;

use std::collections::VecDeque;
use std::future::Future;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;

use crate::config::{AgentKind, Config};
use crate::memory;
use crate::permission::{Answer, PermissionAsk};

pub use claude::pretty_tool;

/// The persona seeded into the assistant's home as `persona.md`.
const PERSONA: &str = include_str!("../../../../voice-home/persona.md");

#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// Session initialised; `model` is the resolved model id.
    Ready { session_id: String, model: String },
    /// A chunk of reply text.
    TextDelta(String),
    /// The agent started using a tool.
    ToolUse { name: String },
    /// The agent needs permission for a tool. Answer with `AgentHandle::answer`.
    Permission(PermissionAsk),
    /// The agent withdrew a permission question.
    PermissionClosed { id: String },
    /// The turn finished.
    TurnDone { ok: bool, interrupted: bool, error: Option<String> },
    /// The agent process went away. It is restarted on the next prompt.
    Exited { reason: String },
    /// A session was rotated and its memory update finished. `facts` is how
    /// many facts are now remembered.
    MemorySaved { ok: bool, facts: usize, error: Option<String> },
    /// `forget()` finished: memory deleted, fresh session started.
    Forgotten,
    /// The models and modes this agent offers, and which are in use.
    Choices(Choices),
    /// Something the user should know that isn't part of a reply ("the agent
    /// wouldn't change its mode").
    Notice(String),
}

/// What can be picked for the session (an agent may offer neither).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Choices {
    pub model: Option<Select>,
    pub mode: Option<Select>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Select {
    /// The `id` in use.
    pub current: String,
    pub options: Vec<Choice>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub id: String,
    pub name: String,
}

impl Select {
    /// The name of the option in use (its id if it isn't listed).
    pub fn current_name(&self) -> &str {
        self.options.iter().find(|o| o.id == self.current).map(|o| o.name.as_str()).unwrap_or(&self.current)
    }
}

/// Which session setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    Model,
    Mode,
}

impl Choices {
    pub fn get(&self, which: Setting) -> Option<&Select> {
        match which {
            Setting::Model => self.model.as_ref(),
            Setting::Mode => self.mode.as_ref(),
        }
    }

    fn get_mut(&mut self, which: Setting) -> Option<&mut Select> {
        match which {
            Setting::Model => self.model.as_mut(),
            Setting::Mode => self.mode.as_mut(),
        }
    }
}

enum Req {
    Ask(String),
    Interrupt,
    Answer { id: String, answer: Answer },
    /// End this session and start a fresh one. `save`: update long-term
    /// memory from it first. `last`: the app is quitting, so don't start one.
    Rotate { save: bool, last: bool },
    Forget,
    /// End this session (saving memory first if `save`) and carry on with
    /// another agent.
    Switch { cfg: Box<Config>, save: bool },
    /// Use another model or mode, now and in later sessions.
    Set { which: Setting, value: String },
}

#[derive(Clone)]
pub struct AgentHandle {
    tx: mpsc::UnboundedSender<Req>,
}

impl AgentHandle {
    /// Start the session actor. Must be called inside a tokio runtime.
    pub fn spawn<F>(cfg: &Config, home: PathBuf, emit: F) -> Self
    where
        F: Fn(AgentEvent) + Send + Sync + 'static,
    {
        *LANGUAGE.lock().unwrap_or_else(|e| e.into_inner()) = cfg.voice.spoken().instruction();
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(run(cfg.clone(), home, rx, emit));
        Self { tx }
    }

    /// Carry on with the agent `cfg` names: the current session ends
    /// (saving memory first if `save`, as `rotate`), and prompts asked
    /// meanwhile go to the new one.
    pub fn switch(&self, cfg: &Config, save: bool) {
        let _ = self.tx.send(Req::Switch { cfg: Box::new(cfg.clone()), save });
    }

    pub fn ask(&self, text: impl Into<String>) {
        let _ = self.tx.send(Req::Ask(text.into()));
    }

    pub fn interrupt(&self) {
        let _ = self.tx.send(Req::Interrupt);
    }

    /// Answer an `AgentEvent::Permission` question.
    pub fn answer(&self, id: impl Into<String>, answer: Answer) {
        let _ = self.tx.send(Req::Answer { id: id.into(), answer });
    }

    /// End the session (saving what's worth remembering) and start fresh.
    /// A prompt asked meanwhile is held until the new session is up.
    pub fn rotate(&self, save: bool) {
        let _ = self.tx.send(Req::Rotate { save, last: false });
    }

    /// Like `rotate`, for quitting: the session ends (saving memory first if
    /// `save`) and no new one is started.
    pub fn end(&self, save: bool) {
        let _ = self.tx.send(Req::Rotate { save, last: true });
    }

    /// Delete long-term memory and start a fresh session.
    pub fn forget(&self) {
        let _ = self.tx.send(Req::Forget);
    }

    /// Use model or mode `value` (an id from `AgentEvent::Choices`).
    pub fn set(&self, which: Setting, value: impl Into<String>) {
        let _ = self.tx.send(Req::Set { which, value: value.into() });
    }

    /// What every session from now on is told about the user's language
    /// (`lang::Language::instruction`); `None` for nothing.
    pub fn set_language(&self, instruction: Option<String>) {
        *LANGUAGE.lock().unwrap_or_else(|e| e.into_inner()) = instruction;
    }
}

/// See `AgentHandle::set_language`. Kept here, not per session, so it
/// carries over agent switches.
static LANGUAGE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// What a backend makes of the agent's output.
#[derive(Debug)]
pub(crate) enum Event {
    /// The session is up.
    Ready { session_id: String, model: String },
    /// A new block of reply text starts (kept apart from the one before).
    TextStart,
    Text(String),
    ToolUse { name: String },
    /// A permission question. The backend keeps what it needs to answer it.
    /// `auto`: it may be allowed without asking (an `auto_allow` kind), if
    /// the turn may use tools at all.
    Permission { ask: PermissionAsk, auto: bool },
    /// The agent withdrew an open permission question.
    PermissionClosed { id: String },
    /// The turn ended. `error` says why when `ok` is false.
    TurnEnd { ok: bool, error: Option<String> },
    /// What the session offers, or a change to which is in use.
    Choices(Choices),
    /// For the user: see `AgentEvent::Notice`.
    Notice(String),
}

/// How a backend took a new setting.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Applied {
    /// In effect for the running session.
    Live,
    /// Only a new session picks it up.
    NextSession,
}

/// Starts an agent. One per configured agent; lives as long as the app.
pub(crate) trait Backend: Send + Sync + 'static {
    type Conn: Conn;
    /// The command, for "could not start" errors.
    fn program(&self) -> &str;
    /// Seed or migrate the agent's own files in the assistant's home.
    fn prepare(&self, home: &Path) -> std::io::Result<()>;
    /// Start the agent on a fresh session. `prompt` is the persona plus
    /// remembered facts, for its system prompt. Also returns events to apply
    /// straight away (such as `Ready`), or why it couldn't start.
    fn start(&self, home: &Path, prompt: &str)
    -> impl Future<Output = Result<(Self::Conn, Vec<Event>), String>> + Send;
    /// Use this model or mode for sessions started from now on.
    fn remember(&mut self, which: Setting, value: &str);
}

/// A running agent. Writes are queued and sent by `flush`, so reading
/// (`recv`) is the only await the actor races against other work.
pub(crate) trait Conn: Send {
    /// The next message from the agent; `None` when it has gone. Must be
    /// cancel-safe.
    fn recv(&mut self) -> impl Future<Output = std::io::Result<Option<String>>> + Send;
    /// Make sense of a message from `recv`. May queue replies to the agent.
    fn handle(&mut self, msg: &str) -> Vec<Event>;
    /// Send queued writes.
    fn flush(&mut self) -> impl Future<Output = ()> + Send;
    /// Send a user prompt, starting a turn.
    fn prompt(&mut self, text: &str) -> impl Future<Output = std::io::Result<()>> + Send;
    /// Ask the agent to stop the current turn. It still ends with `TurnEnd`.
    fn interrupt(&mut self) -> impl Future<Output = std::io::Result<()>> + Send;
    /// Queue the answer to an open permission question. `false` if `id`
    /// isn't open (already answered or withdrawn).
    fn answer(&mut self, id: &str, answer: Answer) -> bool;
    /// Queue a refusal for every open permission question.
    fn deny_all(&mut self);
    /// Change the running session's model or mode.
    fn set(&mut self, which: Setting, value: &str) -> impl Future<Output = Result<Applied, String>> + Send;
    /// Use this model or mode for sessions this agent starts from now on,
    /// without changing the running one.
    fn remember(&mut self, which: Setting, value: &str);
    /// Start a fresh session in the running agent, as `Backend::start` does
    /// for a new one. `Err` if it can't: the agent is then restarted.
    fn new_session(&mut self, home: &Path, prompt: &str) -> impl Future<Output = Result<Vec<Event>, String>> + Send;
    /// Stop the agent and say why it ended (exit status, last stderr lines).
    fn close(self) -> impl Future<Output = String> + Send;
}

/// Create the assistant's working directory with the default persona.
/// An existing persona is never overwritten.
pub fn ensure_home(home: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(home)?;
    let persona = home.join("persona.md");
    if !persona.exists() {
        std::fs::write(persona, PERSONA)?;
    }
    Ok(())
}

/// persona.md plus remembered facts (and the user's language, when it isn't
/// English), fresh for each session.
fn session_prompt(home: &Path, memory: bool) -> String {
    let persona = std::fs::read_to_string(home.join("persona.md")).unwrap_or_else(|_| PERSONA.to_string());
    let persona = match LANGUAGE.lock().unwrap_or_else(|e| e.into_inner()).as_deref() {
        Some(note) => format!("{}\n\n## Language\n\n{note}\n", persona.trim_end()),
        None => persona,
    };
    let facts = if memory { memory::load(home) } else { Vec::new() };
    memory::session_prompt(&persona, &facts)
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

/// What `on_event` leaves for the caller to do.
#[derive(Debug)]
enum After {
    Nothing,
    /// Refuse this permission question: it came during a hidden turn.
    Refuse(String),
    /// Allow it without asking: an `auto_allow` kind in a visible turn.
    Allow(String),
    /// The interrupted user turn ended: the memory update can be sent now.
    Drained,
    /// The memory update finished.
    Memory { ok: bool, reply: String },
    /// The session's choices: keep and show them.
    Choices(Choices),
}

/// Everything the actor owns, so its steps can be methods.
struct Session<B: Backend, F> {
    backend: B,
    home: PathBuf,
    memory: bool,
    emit: F,
    conn: Option<B::Conn>,
    turn: Turn,
    /// The session has had real exchanges (worth saving to memory).
    dirty: bool,
    /// Prompts that arrived while the memory update was running.
    queued: VecDeque<String>,
    /// The app is quitting: sessions that end aren't replaced.
    closing: bool,
    /// Why the agent last failed to start, and when.
    start_error: Option<String>,
    start_failed: Option<std::time::Instant>,
    /// Hand over to this agent once the session has ended.
    switch_to: Option<Config>,
    /// What the session offers, as last reported.
    choices: Choices,
}

impl<B: Backend, F: Fn(AgentEvent)> Session<B, F> {
    async fn start(&mut self) {
        let prompt = session_prompt(&self.home, self.memory);
        match self.backend.start(&self.home, &prompt).await {
            Ok((conn, events)) => {
                self.conn = Some(conn);
                self.start_error = None;
                self.start_failed = None;
                self.apply(events);
            }
            Err(e) => {
                eprintln!("fsc: could not start {}: {e}", self.backend.program());
                self.start_error = Some(e);
                self.start_failed = Some(std::time::Instant::now());
            }
        }
    }

    /// Events that need nothing more than showing (from starting a session).
    fn apply(&mut self, events: Vec<Event>) {
        for e in events {
            if let After::Choices(c) = on_event(e, &mut self.turn, &self.emit) {
                self.show_choices(c);
            }
        }
    }

    fn show_choices(&mut self, c: Choices) {
        self.choices = c;
        (self.emit)(AgentEvent::Choices(self.choices.clone()));
    }

    /// End the current session and start a new one: in the same agent if
    /// it can, else by restarting it. When the app is closing the agent is
    /// just stopped (a new session would only be killed again on exit; one
    /// is still started on demand if a prompt does arrive).
    async fn restart(&mut self) {
        self.dirty = false;
        if !self.closing && let Some(c) = self.conn.as_mut() {
            let prompt = session_prompt(&self.home, self.memory);
            match c.new_session(&self.home, &prompt).await {
                Ok(events) => return self.apply(events),
                Err(e) if !e.is_empty() => eprintln!("fsc: new session failed, restarting the agent: {e}"),
                Err(_) => {}
            }
        }
        if let Some(c) = self.conn.take() {
            c.close().await;
        }
        if !self.closing {
            self.start().await;
        }
    }

    /// Say no to every open permission question (the turn is being stopped).
    async fn deny_pending(&mut self) {
        if let Some(c) = self.conn.as_mut() {
            c.deny_all();
            c.flush().await;
        }
    }

    /// Send a user prompt, starting the agent if needed.
    async fn ask(&mut self, text: &str) {
        // Not again straight after a failure: a hung agent would make the
        // prompt wait out a second handshake.
        if self.conn.is_none() && self.start_failed.is_none_or(|t| t.elapsed() > START_RETRY_AFTER) {
            self.start().await;
        }
        let Some(c) = self.conn.as_mut() else {
            let mut error = format!("could not start `{}`", self.backend.program());
            if let Some(e) = &self.start_error {
                error = format!("{error}: {e}");
            }
            (self.emit)(AgentEvent::TurnDone { ok: false, interrupted: false, error: Some(error) });
            return;
        };
        if let Err(e) = c.prompt(text).await {
            (self.emit)(AgentEvent::TurnDone { ok: false, interrupted: false, error: Some(e.to_string()) });
            self.conn = None;
            return;
        }
        self.dirty = true;
        self.turn = Turn { active: true, ..Default::default() };
    }

    async fn ask_queued(&mut self) {
        if self.switch_to.is_some() {
            // They're for the next agent.
            return;
        }
        for text in std::mem::take(&mut self.queued) {
            self.ask(&text).await;
        }
    }

    /// Abandon prompts held behind a memory save, ending their turn in the UI.
    fn drop_queued(&mut self) {
        if !self.queued.is_empty() {
            self.queued.clear();
            (self.emit)(AgentEvent::TurnDone { ok: true, interrupted: true, error: None });
        }
    }

    fn memory_failed(&self, error: impl Into<String>) {
        (self.emit)(AgentEvent::MemorySaved {
            ok: false,
            facts: memory::load(&self.home).len(),
            error: Some(error.into()),
        });
    }

    /// Ask the session for its memory update. `false` (and a failed
    /// `MemorySaved`) if it couldn't be sent.
    async fn begin_save(&mut self) -> bool {
        let prompt = memory::update_prompt(&memory::load(&self.home));
        let sent = match self.conn.as_mut() {
            Some(c) => c.prompt(&prompt).await,
            None => Err(std::io::Error::other("the agent is not running")),
        };
        match sent {
            Ok(()) => {
                eprintln!("fsc: saving memory before starting a new session");
                self.turn = Turn { active: true, internal: true, ..Default::default() };
                true
            }
            Err(e) => {
                self.memory_failed(e.to_string());
                false
            }
        }
    }

    async fn set(&mut self, which: Setting, value: String) {
        if let Some(next) = self.switch_to.as_mut() {
            // Switching: it's for the agent that's taking over.
            eprintln!("fsc: {which:?} will be {value} for {}", next.agent_name());
            next.set_choice(next.agent.backend, which == Setting::Mode, &value);
            return;
        }
        self.backend.remember(which, &value);
        if let Some(c) = self.conn.as_mut() {
            c.remember(which, &value);
        }
        let applied = match self.conn.as_mut() {
            // A save is running: the next session gets it.
            Some(_) if self.turn.active && self.turn.internal => Ok(Applied::NextSession),
            Some(c) => c.set(which, &value).await,
            None => Ok(Applied::NextSession),
        };
        match applied {
            Ok(Applied::Live) => {
                eprintln!("fsc: {which:?} is now {value}");
                if let Some(sel) = self.choices.get_mut(which) {
                    sel.current = value;
                    (self.emit)(AgentEvent::Choices(self.choices.clone()));
                }
            }
            // Nothing said yet: a fresh session costs nothing, so start one.
            Ok(Applied::NextSession) if self.conn.is_some() && !self.dirty && !self.turn.active => {
                eprintln!("fsc: {which:?} is now {value} (new session)");
                self.restart().await;
            }
            Ok(Applied::NextSession) => eprintln!("fsc: {which:?} will be {value} from the next session"),
            Err(e) => {
                eprintln!("fsc: could not change the {which:?} to {value}: {e}");
                // Show the one still in use.
                (self.emit)(AgentEvent::Choices(self.choices.clone()));
            }
        }
    }

    /// Answer a permission question the user isn't asked.
    async fn reply_to(&mut self, id: &str, answer: Answer) {
        if let Some(c) = self.conn.as_mut() {
            c.answer(id, answer);
            c.flush().await;
        }
    }

    async fn interrupt(&mut self) -> bool {
        match self.conn.as_mut() {
            Some(c) => c.interrupt().await.is_ok(),
            None => false,
        }
    }

    async fn on_request(&mut self, req: Req) {
        let req = match req {
            // Like quitting, but another agent takes over afterwards.
            Req::Switch { cfg, save } => {
                self.switch_to = Some(*cfg);
                Req::Rotate { save, last: true }
            }
            // Quitting: a switch under way is called off.
            Req::Rotate { last: true, save } => {
                self.switch_to = None;
                Req::Rotate { save, last: true }
            }
            req => req,
        };
        match req {
            Req::Ask(text) => {
                if self.turn.active && self.turn.internal {
                    // Saving memory before a new session: ask once it's up.
                    self.queued.push_back(text);
                    return;
                }
                self.ask(&text).await;
            }
            Req::Answer { id, answer } => {
                if let Some(c) = self.conn.as_mut()
                    && c.answer(&id, answer)
                {
                    eprintln!("fsc: permission {answer:?}");
                    c.flush().await;
                }
            }
            Req::Interrupt => {
                if self.turn.active && self.turn.internal {
                    // The save carries on; the prompts waiting behind it don't.
                    self.drop_queued();
                    return;
                }
                self.deny_pending().await;
                if self.turn.active && self.interrupt().await {
                    self.turn.interrupted = true;
                }
            }
            Req::Rotate { save, last } => {
                self.closing |= last;
                if self.turn.active && self.turn.internal {
                    // Already rotating: the new session follows the save.
                    // Prompts queued for it belonged to the conversation
                    // being left, so they're dropped.
                    self.drop_queued();
                    return;
                }
                let save = save && self.memory;
                self.deny_pending().await;
                if self.turn.active {
                    (self.emit)(AgentEvent::TurnDone { ok: true, interrupted: true, error: None });
                    if save && self.dirty && self.conn.is_some() {
                        // The agent is still producing that reply: stop it and
                        // ask for the memory update once it has ended, so its
                        // output isn't mistaken for the update.
                        if self.turn.interrupted || self.interrupt().await {
                            self.turn = Turn { active: true, internal: true, draining: true, ..Default::default() };
                            return;
                        }
                    }
                    self.turn = Turn::default();
                }
                if save {
                    if self.dirty && self.conn.is_some() {
                        if self.begin_save().await {
                            return;
                        }
                    } else {
                        // Still answer, so a quit waiting on the save goes ahead.
                        self.memory_failed("this session had nothing to save");
                    }
                }
                self.restart().await;
                eprintln!("fsc: new agent session");
            }
            Req::Forget => {
                if let Err(e) = memory::forget(&self.home) {
                    eprintln!("fsc: could not delete memory: {e}");
                }
                if self.turn.active && self.turn.internal {
                    // A save was in flight: say it's cancelled (a quit may be waiting on it).
                    (self.emit)(AgentEvent::MemorySaved { ok: false, facts: 0, error: Some("cancelled by forget".into()) });
                } else if self.turn.active {
                    (self.emit)(AgentEvent::TurnDone { ok: true, interrupted: true, error: None });
                }
                self.turn = Turn::default();
                self.restart().await;
                eprintln!("fsc: memory forgotten, new agent session");
                (self.emit)(AgentEvent::Forgotten);
                self.ask_queued().await;
            }
            Req::Switch { .. } => unreachable!("handled above"),
            Req::Set { which, value } => self.set(which, value).await,
        }
    }

    /// A message from the agent.
    async fn on_message(&mut self, msg: &str) {
        let Some(c) = self.conn.as_mut() else { return };
        let events = c.handle(msg);
        c.flush().await;
        for event in events {
            let saving = match on_event(event, &mut self.turn, &self.emit) {
                After::Nothing => continue,
                After::Choices(c) => {
                    self.show_choices(c);
                    continue;
                }
                After::Refuse(id) => {
                    self.reply_to(&id, Answer::Deny).await;
                    continue;
                }
                After::Allow(id) => {
                    self.reply_to(&id, Answer::Once).await;
                    continue;
                }
                After::Drained => self.begin_save().await,
                After::Memory { ok, reply } => {
                    // The memory update finished: store it, then start the new session.
                    let event = finish_save(&self.home, ok, reply);
                    eprintln!("fsc: {event:?}");
                    (self.emit)(event);
                    false
                }
            };
            if !saving {
                self.restart().await;
                self.ask_queued().await;
            }
            // The rest of this message belonged to the turn that just ended.
            return;
        }
    }

    /// The agent's output ended: it exited or can't be read.
    async fn on_gone(&mut self) {
        let reason = match self.conn.take() {
            Some(c) => c.close().await,
            None => "the agent exited".into(),
        };
        if self.turn.active && self.turn.internal {
            self.memory_failed(reason.clone());
        } else if self.turn.active {
            (self.emit)(AgentEvent::TurnDone { ok: false, interrupted: false, error: Some(reason.clone()) });
        }
        self.turn = Turn::default();
        self.dirty = false;
        (self.emit)(AgentEvent::Exited { reason });
        self.ask_queued().await;
    }

    /// A hidden turn ran too long.
    async fn on_timeout(&mut self) {
        eprintln!("fsc: memory update timed out; starting a new session without it");
        self.memory_failed("memory update timed out");
        self.turn = Turn::default();
        self.restart().await;
        self.ask_queued().await;
    }
}

/// Run agents, one after another as the user switches between them.
async fn run<F>(mut cfg: Config, home: PathBuf, mut rx: mpsc::UnboundedReceiver<Req>, emit: F)
where
    F: Fn(AgentEvent) + Send + Sync + 'static,
{
    let mut carried = VecDeque::new();
    loop {
        let memory = cfg.memory();
        let next = match cfg.agent.backend {
            AgentKind::Claude => {
                let backend = claude::ClaudeCli::new(cfg.claude.clone());
                actor(backend, memory, home.clone(), &mut rx, &emit, carried).await
            }
            kind => {
                let backend = acp::Acp::new(kind, cfg.acp_agent(kind), &cfg.agent.auto_allow);
                actor(backend, memory, home.clone(), &mut rx, &emit, carried).await
            }
        };
        let Some((next, queued)) = next else { return };
        eprintln!("fsc: switching to {}", next.agent_name());
        (cfg, carried) = (next, queued);
    }
}

/// Run one agent until the app ends (`None`) or the user switches to
/// another: then its config and the prompts waiting for it.
async fn actor<B, F>(
    backend: B,
    memory: bool,
    home: PathBuf,
    rx: &mut mpsc::UnboundedReceiver<Req>,
    emit: &F,
    queued: VecDeque<String>,
) -> Option<(Config, VecDeque<String>)>
where
    B: Backend,
    F: Fn(AgentEvent) + Send + Sync,
{
    if let Err(e) = ensure_home(&home).and_then(|()| backend.prepare(&home)) {
        eprintln!("fsc: could not prepare {}: {e}", home.display());
    }
    let mut s = Session {
        backend,
        home,
        memory,
        emit,
        conn: None,
        turn: Turn::default(),
        dirty: false,
        queued,
        closing: false,
        start_error: None,
        start_failed: None,
        switch_to: None,
        choices: Choices::default(),
    };
    s.start().await;
    s.ask_queued().await;
    // A hidden (drain + memory update) turn must finish by this time, or it's
    // abandoned so a queued prompt can't hang behind it.
    let mut internal_deadline: Option<tokio::time::Instant> = None;

    loop {
        // Switching, and the old session is done with (memory saved).
        if s.switch_to.is_some() && !(s.turn.active && s.turn.internal) {
            if let Some(c) = s.conn.take() {
                c.close().await;
            }
            return s.switch_to.take().map(|cfg| (cfg, s.queued));
        }
        if s.turn.active && s.turn.internal {
            internal_deadline.get_or_insert_with(|| tokio::time::Instant::now() + INTERNAL_TURN_TIMEOUT);
        } else {
            internal_deadline = None;
        }
        tokio::select! {
            _ = sleep_until_opt(internal_deadline) => s.on_timeout().await,
            req = rx.recv() => match req {
                None => return None,
                Some(req) => s.on_request(req).await,
            },
            msg = recv(&mut s.conn) => match msg {
                Ok(Some(m)) => s.on_message(&m).await,
                Ok(None) | Err(_) => s.on_gone().await,
            },
        }
    }
}

/// Apply one backend event to the turn, emitting what the UI should see.
fn on_event<F: Fn(AgentEvent)>(event: Event, turn: &mut Turn, emit: &F) -> After {
    match event {
        Event::Ready { session_id, model } => emit(AgentEvent::Ready { session_id, model }),
        // Keep separate text blocks on separate lines.
        Event::TextStart if turn.internal => {
            if !turn.buf.is_empty() {
                turn.buf.push('\n');
            }
        }
        Event::TextStart => {
            if turn.has_text {
                emit(AgentEvent::TextDelta("\n\n".into()));
            }
        }
        Event::Text(text) if turn.internal => turn.buf.push_str(&text),
        Event::Text(text) => {
            turn.has_text = true;
            emit(AgentEvent::TextDelta(text));
        }
        Event::ToolUse { name } => {
            if !turn.internal {
                emit(AgentEvent::ToolUse { name });
            }
        }
        Event::Permission { ask, auto } => {
            if !(turn.active && !turn.internal && !turn.draining) {
                // A hidden turn (memory update, or a reply winding down)
                // never gets to use tools, not even ones allowed outright.
                return After::Refuse(ask.id);
            }
            if auto {
                eprintln!("fsc: allowed without asking: {} · {}", ask.tool, ask.action);
                return After::Allow(ask.id);
            }
            eprintln!("fsc: permission asked: {} · {}", ask.tool, ask.action);
            emit(AgentEvent::Permission(ask));
        }
        Event::Notice(text) => emit(AgentEvent::Notice(text)),
        Event::PermissionClosed { id } => emit(AgentEvent::PermissionClosed { id }),
        Event::Choices(c) => return After::Choices(c),
        Event::TurnEnd { ok: success, error } => {
            if !turn.active {
                return After::Nothing;
            }
            let error = || error.clone().unwrap_or_else(|| "the agent reported an error".into());
            if turn.internal {
                let done = if turn.draining {
                    After::Drained
                } else if success {
                    After::Memory { ok: true, reply: std::mem::take(&mut turn.buf) }
                } else {
                    After::Memory { ok: false, reply: error() }
                };
                *turn = Turn::default();
                return done;
            }
            let ok = success || turn.interrupted;
            emit(AgentEvent::TurnDone { ok, interrupted: turn.interrupted, error: (!ok).then(error) });
            *turn = Turn::default();
        }
    }
    After::Nothing
}

/// Store the memory update's reply and report the outcome.
fn finish_save(home: &Path, ok: bool, reply: String) -> AgentEvent {
    let current = memory::load(home);
    match (ok, memory::parse_reply(&reply, &current)) {
        (true, Some(facts)) => match memory::save(home, &facts) {
            Ok(()) => AgentEvent::MemorySaved { ok: true, facts: facts.len(), error: None },
            Err(e) => AgentEvent::MemorySaved { ok: false, facts: current.len(), error: Some(e.to_string()) },
        },
        (true, None) => AgentEvent::MemorySaved {
            ok: false,
            facts: current.len(),
            error: Some("unusable memory reply; kept the old memory".into()),
        },
        (false, _) => AgentEvent::MemorySaved { ok: false, facts: current.len(), error: Some(reply) },
    }
}

/// A start that failed this recently isn't retried for a prompt.
const START_RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(5);

/// Longest a hidden end-of-session turn (drain + memory update) may take.
const INTERNAL_TURN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

async fn sleep_until_opt(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending().await,
    }
}

async fn recv<C: Conn>(conn: &mut Option<C>) -> std::io::Result<Option<String>> {
    match conn {
        Some(c) => c.recv().await,
        None => std::future::pending().await,
    }
}

/// "claude-sonnet-5-5" -> "SONNET 5.5", "claude-3-5-sonnet-20241022" -> "SONNET 3.5",
/// "sonnet" -> "SONNET".
pub fn pretty_model(model: &str) -> String {
    let m = model.strip_prefix("claude-").unwrap_or(model);
    let parts: Vec<&str> = m.split('-').collect();
    let name = parts.iter().find(|p| p.chars().next().is_some_and(|c| c.is_ascii_alphabetic()));
    let Some(name) = name else { return model.to_uppercase() };
    // Version numbers are short numeric parts; long ones are dates.
    let ver: Vec<&str> = parts
        .iter()
        .copied()
        .filter(|p| {
            p.len() <= 4
                && p.starts_with(|c: char| c.is_ascii_digit())
                && p.chars().all(|c| c.is_ascii_digit() || c == '.')
        })
        .collect();
    let name = name.to_uppercase();
    if ver.is_empty() { name } else { format!("{name} {}", ver.join(".")) }
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
        assert_eq!(pretty_model("gemini-3.8-flash-high"), "GEMINI 3.8");
        assert_eq!(pretty_model("gpt-6-luna[medium]"), "GPT 6");
    }

    #[test]
    fn hidden_turns_stay_hidden() {
        let shown = std::cell::RefCell::new(Vec::new());
        let emit = |e: AgentEvent| shown.borrow_mut().push(format!("{e:?}"));
        let end = || Event::TurnEnd { ok: true, error: None };

        // A reply cut short by a rotation: discarded, then reported as drained.
        let mut turn = Turn { active: true, internal: true, draining: true, ..Default::default() };
        assert!(matches!(on_event(Event::Text("- not a memory".into()), &mut turn, &emit), After::Nothing));
        assert!(matches!(on_event(end(), &mut turn, &emit), After::Drained));

        // The memory update: text blocks are kept on separate lines.
        let mut turn = Turn { active: true, internal: true, ..Default::default() };
        for e in [Event::TextStart, Event::Text("- a".into()), Event::TextStart, Event::Text("- b".into())] {
            assert!(matches!(on_event(e, &mut turn, &emit), After::Nothing));
        }
        match on_event(end(), &mut turn, &emit) {
            After::Memory { ok: true, reply } => assert_eq!(reply, "- a\n- b"),
            other => panic!("expected the memory reply, got {other:?}"),
        }

        // Tools asked for in a hidden turn are refused without asking.
        let mut turn = Turn { active: true, internal: true, ..Default::default() };
        let ask = PermissionAsk {
            id: "p1".into(),
            tool: "Bash".into(),
            action: "run a command".into(),
            detail: None,
            spoken: "May I run a command?".into(),
            kind: crate::permission::AskKind::Command,
            can_always: false,
        };
        // ...even ones that would be allowed outright in a visible turn.
        assert!(matches!(on_event(Event::Permission { ask: ask.clone(), auto: true }, &mut turn, &emit), After::Refuse(id) if id == "p1"));
        assert!(matches!(on_event(Event::Permission { ask: ask.clone(), auto: false }, &mut turn, &emit), After::Refuse(_)));
        let mut turn = Turn { active: true, internal: true, draining: true, ..Default::default() };
        assert!(matches!(on_event(Event::Permission { ask: ask.clone(), auto: true }, &mut turn, &emit), After::Refuse(_)));
        let mut turn = Turn { active: true, ..Default::default() };
        assert!(matches!(on_event(Event::Permission { ask, auto: true }, &mut turn, &emit), After::Allow(id) if id == "p1"));
        assert!(shown.borrow().is_empty());
    }

    #[test]
    fn visible_turns_show_text_and_errors() {
        let shown = std::cell::RefCell::new(Vec::new());
        let emit = |e: AgentEvent| shown.borrow_mut().push(e);
        let mut turn = Turn { active: true, ..Default::default() };
        for e in [Event::TextStart, Event::Text("a".into()), Event::TextStart, Event::Text("b".into())] {
            on_event(e, &mut turn, &emit);
        }
        on_event(Event::TurnEnd { ok: false, error: Some("boom".into()) }, &mut turn, &emit);
        let shown = shown.borrow();
        let text: String = shown.iter().filter_map(|e| if let AgentEvent::TextDelta(t) = e { Some(t.as_str()) } else { None }).collect();
        assert_eq!(text, "a\n\nb");
        assert!(matches!(shown.last(), Some(AgentEvent::TurnDone { ok: false, error: Some(e), .. }) if e == "boom"));
        assert!(!turn.active);
    }

    /// A scripted agent. The test plays its output with `say`; what the
    /// actor sends it lands in `log`.
    mod fake {
        use super::super::*;
        use std::sync::{Arc, Mutex};

        #[derive(Clone, Default)]
        pub struct Fake {
            pub log: Arc<Mutex<Vec<String>>>,
            /// The running session's output.
            out: Arc<Mutex<Option<mpsc::UnboundedSender<String>>>>,
        }

        impl Fake {
            /// The agent says `msg`: "text:..", "start", "ask:<id>", "end", "fail:..".
            pub fn say(&self, msg: &str) {
                self.out.lock().unwrap().as_ref().expect("no session").send(msg.into()).unwrap();
            }
            pub fn log(&self) -> Vec<String> {
                self.log.lock().unwrap().clone()
            }
            fn note(&self, s: String) {
                self.log.lock().unwrap().push(s);
            }
        }

        pub struct FakeConn {
            fake: Fake,
            rx: mpsc::UnboundedReceiver<String>,
            pending: Vec<String>,
            outbox: Vec<String>,
        }

        impl Backend for Fake {
            type Conn = FakeConn;
            fn program(&self) -> &str {
                "fake"
            }
            fn prepare(&self, _: &Path) -> std::io::Result<()> {
                Ok(())
            }
            fn remember(&mut self, which: Setting, value: &str) {
                self.note(format!("remember {which:?} {value}"));
            }
            async fn start(&self, _: &Path, prompt: &str) -> Result<(FakeConn, Vec<Event>), String> {
                let (tx, rx) = mpsc::unbounded_channel();
                *self.out.lock().unwrap() = Some(tx);
                self.note(format!("start {}", prompt.contains("- likes tea")));
                Ok((FakeConn { fake: self.clone(), rx, pending: Vec::new(), outbox: Vec::new() }, Vec::new()))
            }
        }

        impl Conn for FakeConn {
            async fn recv(&mut self) -> std::io::Result<Option<String>> {
                Ok(self.rx.recv().await)
            }
            fn handle(&mut self, msg: &str) -> Vec<Event> {
                let (kind, arg) = msg.split_once(':').unwrap_or((msg, ""));
                vec![match kind {
                    "text" => Event::Text(arg.into()),
                    "start" => Event::TextStart,
                    "ask" | "auto" => {
                        self.pending.push(arg.into());
                        let ask = PermissionAsk {
                            id: arg.into(),
                            tool: "Bash".into(),
                            action: "run a command".into(),
                            detail: None,
                            spoken: "May I run a command?".into(),
                            kind: crate::permission::AskKind::Command,
                            can_always: false,
                        };
                        Event::Permission { ask, auto: kind == "auto" }
                    }
                    "end" => Event::TurnEnd { ok: true, error: None },
                    "fail" => Event::TurnEnd { ok: false, error: Some(arg.into()) },
                    _ => return Vec::new(),
                }]
            }
            async fn flush(&mut self) {
                for m in std::mem::take(&mut self.outbox) {
                    self.fake.note(m);
                }
            }
            async fn prompt(&mut self, text: &str) -> std::io::Result<()> {
                let shown = if text.contains("Internal maintenance request") { "<memory update>" } else { text };
                self.fake.note(format!("prompt {shown}"));
                Ok(())
            }
            async fn interrupt(&mut self) -> std::io::Result<()> {
                self.fake.note("interrupt".into());
                Ok(())
            }
            fn answer(&mut self, id: &str, answer: Answer) -> bool {
                let Some(i) = self.pending.iter().position(|p| p == id) else { return false };
                self.pending.remove(i);
                self.outbox.push(format!("answer {id} {answer:?}"));
                true
            }
            fn deny_all(&mut self) {
                for id in std::mem::take(&mut self.pending) {
                    self.outbox.push(format!("answer {id} Deny"));
                }
            }
            async fn new_session(&mut self, _: &Path, _: &str) -> Result<Vec<Event>, String> {
                Err(String::new())
            }
            async fn set(&mut self, which: Setting, value: &str) -> Result<Applied, String> {
                self.fake.note(format!("set {which:?} {value}"));
                Ok(Applied::Live)
            }
            fn remember(&mut self, which: Setting, value: &str) {
                self.fake.note(format!("conn remember {which:?} {value}"));
            }
            async fn close(self) -> String {
                self.fake.note("close".into());
                "fake exited".into()
            }
        }
    }

    struct Harness {
        fake: fake::Fake,
        tx: mpsc::UnboundedSender<Req>,
        events: mpsc::UnboundedReceiver<AgentEvent>,
        home: PathBuf,
        done: tokio::task::JoinHandle<Option<(Config, VecDeque<String>)>>,
    }

    impl Harness {
        fn new(name: &str) -> Self {
            let home = std::env::temp_dir().join(format!("fsc-agent-test-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&home);
            let fake = fake::Fake::default();
            let (tx, rx) = mpsc::unbounded_channel();
            let (etx, events) = mpsc::unbounded_channel();
            let (f, h) = (fake.clone(), home.clone());
            let done = tokio::spawn(async move {
                let mut rx = rx;
                let emit = move |e| {
                    let _ = etx.send(e);
                };
                actor(f, true, h, &mut rx, &emit, VecDeque::new()).await
            });
            Self { fake, tx, events, home, done }
        }

        fn send(&self, r: Req) {
            self.tx.send(r).unwrap();
        }

        async fn next(&mut self) -> AgentEvent {
            tokio::time::timeout(std::time::Duration::from_secs(2), self.events.recv()).await.unwrap().unwrap()
        }

        /// Let the actor catch up with everything sent so far.
        async fn settle(&self) {
            for _ in 0..20 {
                tokio::task::yield_now().await;
            }
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }

    #[tokio::test]
    async fn a_turn_and_a_permission() {
        let mut h = Harness::new("turn");
        h.send(Req::Ask("hello".into()));
        h.settle().await;
        h.fake.say("text:hi");
        assert!(matches!(h.next().await, AgentEvent::TextDelta(t) if t == "hi"));
        h.fake.say("ask:p1");
        assert!(matches!(h.next().await, AgentEvent::Permission(a) if a.id == "p1"));
        h.send(Req::Answer { id: "p1".into(), answer: Answer::Once });
        // Answering twice does nothing the second time.
        h.send(Req::Answer { id: "p1".into(), answer: Answer::Deny });
        h.fake.say("end");
        assert!(matches!(h.next().await, AgentEvent::TurnDone { ok: true, interrupted: false, .. }));
        assert_eq!(h.fake.log(), ["start false", "prompt hello", "answer p1 Once"]);
    }

    #[tokio::test]
    async fn rotation_saves_memory_and_holds_prompts() {
        let mut h = Harness::new("rotate");
        h.send(Req::Ask("I like tea".into()));
        h.settle().await;
        h.fake.say("end");
        assert!(matches!(h.next().await, AgentEvent::TurnDone { ok: true, .. }));

        h.send(Req::Rotate { save: true, last: false });
        // A prompt during the save waits for the new session.
        h.send(Req::Ask("next".into()));
        h.settle().await;
        // The update's own permission requests are refused without asking.
        h.fake.say("ask:p9");
        h.fake.say("start");
        h.fake.say("text:- likes tea");
        h.fake.say("end");
        assert!(matches!(h.next().await, AgentEvent::MemorySaved { ok: true, facts: 1, .. }));
        h.settle().await;
        assert_eq!(
            h.fake.log(),
            [
                "start false",
                "prompt I like tea",
                "prompt <memory update>",
                "answer p9 Deny",
                "close",
                // The new session knows the fact.
                "start true",
                "prompt next",
            ]
        );
    }

    #[tokio::test]
    async fn rotation_mid_reply_drains_first() {
        let mut h = Harness::new("drain");
        h.send(Req::Ask("tell me a story".into()));
        h.settle().await;
        h.fake.say("text:once upon");
        assert!(matches!(h.next().await, AgentEvent::TextDelta(_)));
        h.fake.say("ask:p1");
        assert!(matches!(h.next().await, AgentEvent::Permission(_)));

        h.send(Req::Rotate { save: true, last: true });
        assert!(matches!(h.next().await, AgentEvent::TurnDone { ok: true, interrupted: true, .. }));
        // The rest of the cut-short reply isn't shown or saved.
        h.fake.say("text: a time");
        h.fake.say("end");
        h.settle().await;
        h.fake.say("text:NONE");
        h.fake.say("end");
        assert!(matches!(h.next().await, AgentEvent::MemorySaved { ok: true, facts: 0, .. }));
        h.settle().await;
        // Quitting: no new session afterwards.
        assert_eq!(
            h.fake.log(),
            ["start false", "prompt tell me a story", "answer p1 Deny", "interrupt", "prompt <memory update>", "close"]
        );
    }

    #[tokio::test]
    async fn switching_saves_then_hands_over() {
        let mut h = Harness::new("switch");
        h.send(Req::Ask("I like tea".into()));
        h.settle().await;
        h.fake.say("end");
        assert!(matches!(h.next().await, AgentEvent::TurnDone { ok: true, .. }));

        let mut next = Config::default();
        next.agent.backend = AgentKind::Codex;
        h.send(Req::Switch { cfg: Box::new(next), save: true });
        // Asked during the save: goes to the next agent.
        h.send(Req::Ask("hello codex".into()));
        h.settle().await;
        h.fake.say("text:- likes tea");
        h.fake.say("end");
        assert!(matches!(h.next().await, AgentEvent::MemorySaved { ok: true, facts: 1, .. }));
        let (cfg, queued) = tokio::time::timeout(std::time::Duration::from_secs(2), &mut h.done)
            .await
            .unwrap()
            .unwrap()
            .expect("handed over");
        assert_eq!(cfg.agent.backend, AgentKind::Codex);
        assert_eq!(queued, ["hello codex"]);
        // The old agent was stopped, not given a new session.
        assert_eq!(h.fake.log(), ["start false", "prompt I like tea", "prompt <memory update>", "close"]);
    }

    #[tokio::test]
    async fn choices_during_a_switch_are_for_the_next_agent() {
        let mut h = Harness::new("switch-set");
        h.send(Req::Ask("hi".into()));
        h.settle().await;
        h.fake.say("end");
        assert!(matches!(h.next().await, AgentEvent::TurnDone { .. }));
        let mut next = Config::default();
        next.agent.backend = AgentKind::Codex;
        h.send(Req::Switch { cfg: Box::new(next), save: true });
        h.send(Req::Set { which: Setting::Model, value: "gpt-x".into() });
        h.settle().await;
        h.fake.say("text:NONE");
        h.fake.say("end");
        let (cfg, _) = tokio::time::timeout(std::time::Duration::from_secs(2), &mut h.done).await.unwrap().unwrap().unwrap();
        assert_eq!(cfg.codex.model, "gpt-x");
        // The old agent never heard of it.
        assert!(!h.fake.log().iter().any(|l| l.contains("gpt-x")));
    }

    #[tokio::test]
    async fn quitting_calls_off_a_switch() {
        let mut h = Harness::new("switch-quit");
        h.send(Req::Ask("hi".into()));
        h.settle().await;
        h.fake.say("end");
        assert!(matches!(h.next().await, AgentEvent::TurnDone { .. }));
        h.send(Req::Switch { cfg: Box::default(), save: true });
        h.send(Req::Rotate { save: true, last: true });
        h.settle().await;
        h.fake.say("text:NONE");
        h.fake.say("end");
        assert!(matches!(h.next().await, AgentEvent::MemorySaved { .. }));
        // The app is closing: no handing over, and the actor waits to end.
        h.settle().await;
        assert!(!h.done.is_finished());
    }

    #[tokio::test]
    async fn reads_run_without_asking_in_visible_turns() {
        let mut h = Harness::new("auto");
        h.send(Req::Ask("look".into()));
        h.settle().await;
        h.fake.say("auto:r1");
        h.fake.say("end");
        assert!(matches!(h.next().await, AgentEvent::TurnDone { ok: true, .. }));
        assert!(h.fake.log().contains(&"answer r1 Once".to_string()));
    }

    #[tokio::test]
    async fn a_setting_during_a_save_reaches_the_next_session() {
        let mut h = Harness::new("set-save");
        h.send(Req::Ask("hi".into()));
        h.settle().await;
        h.fake.say("end");
        assert!(matches!(h.next().await, AgentEvent::TurnDone { .. }));
        h.send(Req::Rotate { save: true, last: false });
        h.send(Req::Set { which: Setting::Model, value: "m2".into() });
        h.settle().await;
        // Both the backend and the running agent keep it for later sessions.
        let log = h.fake.log();
        assert!(log.contains(&"remember Model m2".to_string()), "{log:?}");
        assert!(log.contains(&"conn remember Model m2".to_string()), "{log:?}");
        assert!(!log.contains(&"set Model m2".to_string()), "{log:?}");
    }

    #[tokio::test]
    async fn switching_an_idle_session_is_immediate() {
        let mut h = Harness::new("switch-idle");
        h.send(Req::Switch { cfg: Box::default(), save: false });
        let r = tokio::time::timeout(std::time::Duration::from_secs(2), &mut h.done).await.unwrap().unwrap();
        assert!(r.is_some_and(|(_, q)| q.is_empty()));
        assert_eq!(h.fake.log(), ["start false", "close"]);
    }

    #[tokio::test]
    async fn failed_turns_report_the_error() {
        let mut h = Harness::new("fail");
        h.send(Req::Ask("hi".into()));
        h.settle().await;
        h.fake.say("fail:rate limited");
        assert!(matches!(h.next().await, AgentEvent::TurnDone { ok: false, error: Some(e), .. } if e == "rate limited"));
        // Interrupted turns count as fine whatever the agent says.
        h.send(Req::Ask("again".into()));
        h.send(Req::Interrupt);
        h.settle().await;
        h.fake.say("fail:aborted");
        assert!(matches!(h.next().await, AgentEvent::TurnDone { ok: true, interrupted: true, error: None }));
    }
}
