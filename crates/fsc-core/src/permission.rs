//! Permission prompts. When Claude wants a tool that the assistant's
//! settings don't already allow (or deny), the CLI asks us over the
//! stream-json control channel (`--permission-prompt-tool stdio`):
//!
//! ```text
//! ← {"type":"control_request","request_id":…,"request":{"subtype":"can_use_tool",
//!    "tool_name":"Bash","input":{…},"permission_suggestions":[…],…}}
//! → {"type":"control_response","response":{"subtype":"success","request_id":…,
//!    "response":{"behavior":"allow","updatedInput":{…}}}}
//! ```
//!
//! "Always" sends the CLI's own suggestions back as `updatedPermissions`,
//! which it saves (normally to `.claude/settings.local.json`).

use serde_json::{Value, json};

use crate::claude::pretty_tool;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Once,
    Always,
    Deny,
}

/// A permission question, ready to show and speak.
#[derive(Debug, Clone)]
pub struct PermissionAsk {
    pub id: String,
    /// "Bash", "Google Calendar · create event", ...
    pub tool: String,
    /// What it wants to do, as a phrase that follows "May I": "search the web".
    pub action: String,
    /// The exact command, URL, path or query, if there is one.
    pub detail: Option<String>,
    /// The question to read aloud.
    pub spoken: String,
    /// "Always" would save a rule.
    pub can_always: bool,
}

/// A question the CLI is waiting on, with what's needed to answer it.
#[derive(Debug, Clone)]
pub struct Pending {
    pub id: String,
    input: Value,
    suggestions: Value,
}

const MAX_DETAIL: usize = 300;

/// Read a `can_use_tool` request.
pub fn parse(request_id: &str, req: &Value) -> (PermissionAsk, Pending) {
    let name = req["tool_name"].as_str().unwrap_or("a tool");
    let input = &req["input"];
    let field = |k: &str| input[k].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let said = |k: &str| req[k].as_str().map(str::trim).filter(|s| !s.is_empty()).map(phrase);

    let (action, detail, spoken_detail) = match name {
        "Bash" => {
            let action = field("description").map(|d| phrase(&d)).unwrap_or_else(|| "run a command".into());
            (action, field("command"), None)
        }
        "WebSearch" => ("search the web".into(), field("query"), field("query")),
        "WebFetch" => ("open a web page".into(), field("url"), None),
        "Write" => ("write a file".into(), field("file_path"), None),
        "Edit" | "MultiEdit" => ("edit a file".into(), field("file_path"), None),
        "NotebookEdit" => ("edit a notebook".into(), field("notebook_path"), None),
        "Read" => ("read a file".into(), field("file_path"), None),
        "Glob" | "Grep" => ("search your files".into(), field("pattern"), None),
        _ => {
            let action = said("title")
                .or_else(|| said("description"))
                .unwrap_or_else(|| match pretty_tool(name).split_once(" · ") {
                    // "Google Calendar · create event" -> "use Google Calendar to create event"
                    Some((server, tool)) => format!("use {server} to {tool}"),
                    None => format!("use {name}"),
                });
            let detail = (!input.is_null() && input.as_object().is_none_or(|o| !o.is_empty())).then(|| input.to_string());
            (action, detail, None)
        }
    };
    let spoken = match &spoken_detail {
        Some(d) => format!("May I {action} for {d}?"),
        None => format!("May I {action}?"),
    };
    let suggestions = req["permission_suggestions"].clone();
    let can_always = suggestions.as_array().is_some_and(|a| !a.is_empty());
    let tool = req["display_name"].as_str().filter(|s| !s.is_empty()).map(pretty_tool).unwrap_or_else(|| pretty_tool(name));
    (
        PermissionAsk {
            id: request_id.to_string(),
            tool,
            action,
            detail: detail.map(|d| truncate(&d, MAX_DETAIL)),
            spoken,
            can_always,
        },
        Pending { id: request_id.to_string(), input: input.clone(), suggestions },
    )
}

/// The control_response that answers `p`.
pub fn response(p: &Pending, answer: Answer) -> Value {
    let r = match answer {
        Answer::Deny => json!({ "behavior": "deny", "message": "The user said no." }),
        Answer::Once => json!({ "behavior": "allow", "updatedInput": p.input }),
        Answer::Always => json!({
            "behavior": "allow",
            "updatedInput": p.input,
            "updatedPermissions": p.suggestions.as_array().cloned().unwrap_or_default(),
        }),
    };
    json!({ "type": "control_response", "response": { "subtype": "success", "request_id": p.id, "response": r } })
}

/// Refuse a control request we don't handle, so the CLI isn't left waiting.
pub fn unsupported(request_id: &str) -> Value {
    json!({ "type": "control_response",
            "response": { "subtype": "error", "request_id": request_id, "error": "not supported by this host" } })
}

/// Turn a reply ("yes", "yeah go ahead", "always", "no thanks") into an answer.
pub fn parse_answer(text: &str) -> Option<Answer> {
    let t: String = text
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '\'' { c } else { ' ' })
        .collect();
    let words: Vec<&str> = t.split_whitespace().collect();
    if words.is_empty() || words.len() > 8 {
        return None;
    }
    let s = format!(" {} ", words.join(" "));
    let has = |list: &[&str]| list.iter().any(|w| s.contains(&format!(" {w} ")));

    if has(&["always", "every time", "don't ask again", "dont ask again", "from now on"]) {
        return Some(Answer::Always);
    }
    if has(&["no", "n", "nope", "nah", "don't", "dont", "do not", "deny", "stop", "cancel", "never", "not now"]) {
        return Some(Answer::Deny);
    }
    if has(&[
        "yes", "y", "yeah", "yep", "yup", "yah", "sure", "ok", "okay", "allow", "go ahead", "do it", "fine",
        "please", "alright", "all right", "absolutely", "of course", "proceed", "go for it", "affirmative",
    ]) {
        return Some(Answer::Once);
    }
    None
}

/// "Write \"hi\" into note.txt." -> "write \"hi\" into note.txt", so it
/// reads after "May I". Leaves a leading proper noun or acronym alone.
fn phrase(s: &str) -> String {
    let s = s.trim().trim_end_matches('.');
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(a), Some(b)) if a.is_uppercase() && b.is_lowercase() => a.to_lowercase().chain(s[a.len_utf8()..].chars()).collect(),
        _ => s.to_string(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers() {
        for (said, want) in [
            ("Yes.", Some(Answer::Once)),
            ("Yeah, go ahead.", Some(Answer::Once)),
            ("y", Some(Answer::Once)),
            ("Okay", Some(Answer::Once)),
            ("Always.", Some(Answer::Always)),
            ("Yes, always.", Some(Answer::Always)),
            ("Yes, and don't ask again.", Some(Answer::Always)),
            ("No.", Some(Answer::Deny)),
            ("No thanks.", Some(Answer::Deny)),
            ("Don't do that.", Some(Answer::Deny)),
            ("n", Some(Answer::Deny)),
            ("What's the weather like?", None),
            ("", None),
            ("I was thinking we could maybe yes talk about something else instead", None),
        ] {
            assert_eq!(parse_answer(said), want, "{said:?}");
        }
    }

    #[test]
    fn bash_request() {
        let req = json!({
            "subtype": "can_use_tool", "tool_name": "Bash", "display_name": "Bash",
            "input": {"command": "echo hi > note.txt", "description": "Write \"hi\" into note.txt"},
            "permission_suggestions": [{"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "echo hi *"}],
                                        "behavior": "allow", "destination": "localSettings"}],
        });
        let (ask, pending) = parse("r1", &req);
        assert_eq!(ask.action, "write \"hi\" into note.txt");
        assert_eq!(ask.detail.as_deref(), Some("echo hi > note.txt"));
        assert_eq!(ask.spoken, "May I write \"hi\" into note.txt?");
        assert!(ask.can_always);

        let r = response(&pending, Answer::Always);
        assert_eq!(r["response"]["request_id"], "r1");
        assert_eq!(r["response"]["response"]["behavior"], "allow");
        assert_eq!(r["response"]["response"]["updatedInput"]["command"], "echo hi > note.txt");
        assert_eq!(r["response"]["response"]["updatedPermissions"][0]["destination"], "localSettings");
        assert!(response(&pending, Answer::Once)["response"]["response"].get("updatedPermissions").is_none());
        assert_eq!(response(&pending, Answer::Deny)["response"]["response"]["behavior"], "deny");
    }

    #[test]
    fn other_requests() {
        let (ask, _) = parse("r2", &json!({"tool_name": "WebSearch", "input": {"query": "weather tomorrow"}}));
        assert_eq!(ask.spoken, "May I search the web for weather tomorrow?");
        assert!(!ask.can_always);

        let (ask, _) = parse(
            "r3",
            &json!({"tool_name": "mcp__claude_ai_Google_Calendar__create_event", "input": {"summary": "Dentist"}}),
        );
        assert_eq!(ask.tool, "Google Calendar · create event");
        assert_eq!(ask.action, "use Google Calendar to create event");
        assert_eq!(ask.detail.as_deref(), Some(r#"{"summary":"Dentist"}"#));

        assert_eq!(phrase("Open Firefox."), "open Firefox");
        assert_eq!(phrase("List files"), "list files");
        assert_eq!(phrase("NASA site"), "NASA site");
    }
}
