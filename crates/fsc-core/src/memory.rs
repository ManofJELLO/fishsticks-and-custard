//! Long-term memory across Claude sessions.
//!
//! Sessions are rotated (after idle time, on `fsc new`, on quit) so context
//! doesn't grow forever. Before a session ends, Claude is asked, in that
//! session, to update a short list of durable facts about the user. The list
//! lives in `<home>/memory.md`, a plain file the user can read or edit, and
//! is appended to the system prompt of every new session. `fsc forget`
//! deletes it.

use std::path::{Path, PathBuf};

pub const MAX_FACTS: usize = 40;
const NONE: &str = "NONE";
const HEADER: &str = "# Memory\n\n<!-- Facts Fishsticks & Custard remembers between sessions. \
Edit freely; `fsc forget` deletes this file. -->\n\n";

pub fn path(home: &Path) -> PathBuf {
    home.join("memory.md")
}

/// The remembered facts as bullet lines ("- ..."), or empty.
pub fn load(home: &Path) -> Vec<String> {
    std::fs::read_to_string(path(home)).map(|s| bullets(&s)).unwrap_or_default()
}

pub fn forget(home: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path(home)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// The hidden end-of-session request.
pub fn update_prompt(current: &[String]) -> String {
    let current = if current.is_empty() { "(empty)".to_string() } else { current.join("\n") };
    format!(
        "[Internal maintenance request from the Fishsticks & Custard app, not from the user. \
Do not use any tools, and do not mention this request later.]\n\n\
Update your long-term memory about the user so future conversations don't make them repeat themselves.\n\n\
Current memory:\n{current}\n\n\
From this session, add durable facts worth remembering: the user's preferences, location, people and pets, \
devices and setup, ongoing projects or plans, and anything they asked you to remember. Merge with the current \
memory, removing duplicates and anything now outdated or contradicted. Leave out one-off questions and their \
answers, small talk, and sensitive details (passwords, keys, financial or health information) unless the user \
explicitly asked you to remember them. Don't copy things already stated in your instructions (such as the \
desktop setup); only what you learned from the user.\n\n\
Reply with only the complete updated memory as a Markdown bullet list, one fact per bullet, at most {MAX_FACTS} \
bullets. If there is nothing worth remembering at all, reply with exactly: {NONE}"
    )
}

/// Validate Claude's reply. `Some(facts)` to store (possibly empty: nothing
/// to remember), `None` if the reply isn't usable and the old memory should
/// be kept.
pub fn parse_reply(reply: &str, current: &[String]) -> Option<Vec<String>> {
    let trimmed = reply.trim();
    let facts = bullets(trimmed);
    if facts.is_empty() {
        // "NONE", "NONE.", "None - nothing new": nothing to add. That must
        // never wipe what we already had.
        let first = trimmed.split(|c: char| !c.is_alphanumeric()).next().unwrap_or_default();
        return first.eq_ignore_ascii_case(NONE).then(|| current.to_vec());
    }
    Some(facts.into_iter().take(MAX_FACTS).collect())
}

pub fn save(home: &Path, facts: &[String]) -> std::io::Result<()> {
    if facts.is_empty() {
        return forget(home);
    }
    let tmp = home.join("memory.md.tmp");
    std::fs::write(&tmp, format!("{HEADER}{}\n", facts.join("\n")))?;
    std::fs::rename(tmp, path(home))
}

/// persona + remembered facts, for `--append-system-prompt-file`.
pub fn session_prompt(persona: &str, facts: &[String]) -> String {
    if facts.is_empty() {
        return persona.to_string();
    }
    format!(
        "{}\n\n## What you remember about the user\n\n\
From earlier conversations. Use it naturally; don't recite it unless asked.\n\n{}\n",
        persona.trim_end(),
        facts.join("\n")
    )
}

fn bullets(s: &str) -> Vec<String> {
    s.lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")).or_else(|| l.strip_prefix("• ")))
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(|f| format!("- {f}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_bullets_and_ignores_chatter() {
        let reply = "Here you go:\n- Lives in Springfield\n* Prefers metric units\n\n• Has a cat named Rex\n";
        assert_eq!(
            parse_reply(reply, &[]).unwrap(),
            facts(&["- Lives in Springfield", "- Prefers metric units", "- Has a cat named Rex"])
        );
    }

    #[test]
    fn none_keeps_existing_memory() {
        let current = facts(&["- Likes jazz"]);
        assert_eq!(parse_reply("NONE", &current).unwrap(), current);
        assert_eq!(parse_reply(" none \n", &[]).unwrap(), Vec::<String>::new());
        assert_eq!(parse_reply("NONE.", &current).unwrap(), current);
        assert_eq!(parse_reply("None - nothing new this time.", &current).unwrap(), current);
    }

    #[test]
    fn unusable_reply_keeps_old() {
        assert_eq!(parse_reply("Sorry, I can't do that.", &facts(&["- x"])), None);
    }

    #[test]
    fn caps_fact_count() {
        let many: String = (0..100).map(|i| format!("- fact {i}\n")).collect();
        assert_eq!(parse_reply(&many, &[]).unwrap().len(), MAX_FACTS);
    }

    #[test]
    fn round_trip_file() {
        let dir = std::env::temp_dir().join(format!("fsc-mem-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = facts(&["- Lives in Springfield", "- Prefers metric units"]);
        save(&dir, &f).unwrap();
        assert_eq!(load(&dir), f);
        assert!(session_prompt("Persona.", &f).contains("- Prefers metric units"));
        forget(&dir).unwrap();
        assert!(load(&dir).is_empty());
        forget(&dir).unwrap(); // idempotent
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
