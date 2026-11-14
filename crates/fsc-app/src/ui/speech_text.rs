//! Turns streaming reply text into speakable sentences, so speech starts
//! with the first sentence instead of waiting for the whole reply. Markdown,
//! links and code are tidied so they're said sensibly rather than spelled out.

/// Below this many characters, keep accumulating rather than speaking a
/// fragment like "Yes." on its own (fewer gaps between clips).
const MIN_CHUNK: usize = 24;
/// Inline code longer than this isn't read out.
const MAX_INLINE_CODE: usize = 40;
/// Said in place of a fenced code block.
const CODE_BLOCK: &str = "The code is on screen.";
/// Said in place of long inline code.
const LONG_CODE: &str = "the command on screen";

#[derive(Default)]
pub struct SentenceBuffer {
    buf: String,
    /// Inside a ``` block: its lines are skipped.
    in_code: bool,
}

impl SentenceBuffer {
    pub fn clear(&mut self) {
        self.buf.clear();
        self.in_code = false;
    }

    /// Add streamed text; returns any sentences that are now complete.
    pub fn push(&mut self, text: &str) -> Vec<String> {
        self.buf.push_str(text);
        let mut out = Vec::new();
        loop {
            let head = self.buf.trim_start();
            if self.in_code || "```".starts_with(head) || head.starts_with("```") {
                // Code fences work a whole line at a time.
                let Some(nl) = self.buf.find('\n') else { break };
                let line: String = self.buf.drain(..=nl).collect();
                let fence = line.trim_start().starts_with("```");
                if fence && !self.in_code {
                    out.push(CODE_BLOCK.to_string());
                }
                if fence {
                    self.in_code = !self.in_code;
                }
                if !fence && !self.in_code && let Some(s) = speakable(&line) {
                    out.push(s);
                }
                continue;
            }
            let Some(end) = self.boundary() else { break };
            let s: String = self.buf.drain(..end).collect();
            if let Some(s) = speakable(&s) {
                out.push(s);
            }
        }
        out
    }

    /// Whatever is left at the end of the reply.
    pub fn flush(&mut self) -> Option<String> {
        let s = std::mem::take(&mut self.buf);
        let in_code = std::mem::take(&mut self.in_code);
        if in_code || s.trim_start().starts_with("```") { None } else { speakable(&s) }
    }

    /// Byte index just past a sentence end: `.`/`!`/`?` followed by
    /// whitespace (so "3.5" and "e.g.x" don't split), or a newline.
    fn boundary(&self) -> Option<usize> {
        let mut prev: Option<(usize, char)> = None;
        for (i, c) in self.buf.char_indices() {
            if c == '\n' && self.buf[..i].trim().len() >= 2 {
                return Some(i + 1);
            }
            if let Some((_, p)) = prev
                && matches!(p, '.' | '!' | '?')
                && c.is_whitespace()
                && self.buf[..i].trim().chars().count() >= MIN_CHUNK
            {
                return Some(i + c.len_utf8());
            }
            prev = Some((i, c));
        }
        None
    }
}

/// Make a chunk of reply sayable: link text instead of links, site names
/// instead of URLs, no list bullets or markdown symbols. `None` if nothing
/// worth saying is left.
fn speakable(s: &str) -> Option<String> {
    let s = strip_list_marker(s.trim());
    let s = links_to_text(s);
    let s = inline_code(&s);
    let s: Vec<String> = s.split_whitespace().map(url_to_site).collect();
    let s = s.join(" ");
    let cleaned: String = s.chars().filter(|c| !matches!(c, '*' | '`' | '#' | '|' | '~')).collect();
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    cleaned.chars().any(|c| c.is_alphanumeric()).then_some(cleaned)
}

/// "- item", "* item", "3. item", "2) item", "> quote" -> "item".
fn strip_list_marker(s: &str) -> &str {
    for m in ["- ", "* ", "+ ", "• ", "> "] {
        if let Some(rest) = s.strip_prefix(m) {
            return rest.trim_start();
        }
    }
    let digits = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
    if (1..=3).contains(&digits) {
        let rest = &s[digits..];
        if let Some(rest) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return rest.trim_start();
        }
    }
    s
}

/// "[the docs](https://…)" -> "the docs".
fn links_to_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let Some(close) = after.find("](") else { break };
        let Some(end) = after[close + 2..].find(')') else { break };
        out.push_str(&rest[..open]);
        out.push_str(&after[..close]);
        rest = &after[close + 2 + end + 1..];
    }
    out.push_str(rest);
    out
}

/// Keep short inline code (a command or file name reads fine); replace
/// long code with a pointer to the screen.
fn inline_code(s: &str) -> String {
    let parts: Vec<&str> = s.split('`').collect();
    if parts.len() < 3 {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    for (i, part) in parts.iter().enumerate() {
        // Odd parts are inside backticks (an unmatched last one isn't).
        let code = i % 2 == 1 && i + 1 < parts.len();
        if code && part.chars().count() > MAX_INLINE_CODE {
            out.push_str(LONG_CODE);
        } else {
            out.push_str(part);
        }
    }
    out
}

/// "https://www.example.com/a/b?c," -> "example.com,". Other words unchanged.
fn url_to_site(word: &str) -> String {
    let start = word.find(|c: char| c.is_alphanumeric()).unwrap_or(0);
    let (lead, w) = word.split_at(start);
    let body = w
        .strip_prefix("https://")
        .or_else(|| w.strip_prefix("http://"))
        .or_else(|| w.starts_with("www.").then_some(w));
    let Some(body) = body else { return word.to_string() };
    let host_end = body.find(['/', '?', '#', ':']).unwrap_or(body.len());
    let host = body[..host_end].strip_prefix("www.").unwrap_or(&body[..host_end]);
    // Keep punctuation that ended the sentence or clause after the URL.
    let tail = &w[w.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '>', '"', '\'']).len()..];
    let host = host.trim_end_matches(['.', ',', ';', ')', '>', '"', '\'']);
    if host.is_empty() {
        return word.to_string();
    }
    let lead = lead.trim_start_matches(['(', '<']);
    let tail = tail.trim_start_matches([')', '>']);
    format!("{lead}{host}{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(text: &str) -> Vec<String> {
        let mut b = SentenceBuffer::default();
        let mut out = b.push(text);
        out.extend(b.flush());
        out
    }

    /// Feed one character at a time, as a worst-case stream.
    fn trickled(text: &str) -> Vec<String> {
        let mut b = SentenceBuffer::default();
        let mut out = Vec::new();
        for c in text.chars() {
            out.extend(b.push(&c.to_string()));
        }
        out.extend(b.flush());
        out
    }

    #[test]
    fn splits_streamed_sentences() {
        let mut b = SentenceBuffer::default();
        assert!(b.push("The kernel is version 6").is_empty());
        assert!(b.push(".12.3, released last week").is_empty());
        let out = b.push(". It has the fix you wanted. Ok");
        assert_eq!(
            out,
            vec!["The kernel is version 6.12.3, released last week.", "It has the fix you wanted."]
        );
        assert_eq!(b.flush().as_deref(), Some("Ok"));
    }

    #[test]
    fn short_sentences_are_merged() {
        let mut b = SentenceBuffer::default();
        assert!(b.push("Yes. ").is_empty());
        assert_eq!(b.push("That is correct, it's running. "), vec!["Yes. That is correct, it's running."]);
    }

    #[test]
    fn strips_markdown() {
        let mut b = SentenceBuffer::default();
        b.push("**Bold** and `code`\n");
        assert_eq!(b.flush(), None);
        let mut b = SentenceBuffer::default();
        assert_eq!(b.push("**Bold** and `code`\n"), vec!["Bold and code"]);
    }

    #[test]
    fn urls_become_site_names() {
        assert_eq!(
            all("The wiki at https://wiki.archlinux.org/title/PipeWire#Bluetooth covers it."),
            vec!["The wiki at wiki.archlinux.org covers it."]
        );
        assert_eq!(all("See (https://www.example.com/a?b=c), then retry."), vec!["See example.com, then retry."]);
        assert_eq!(all("Try www.example.com."), vec!["Try example.com."]);
        assert_eq!(all("Version 6.12 is out."), vec!["Version 6.12 is out."]);
    }

    #[test]
    fn markdown_links_read_their_text() {
        assert_eq!(
            all("The [release notes](https://example.com/notes) list the fix."),
            vec!["The release notes list the fix."]
        );
    }

    #[test]
    fn list_markers_are_dropped() {
        let text = "Three things:\n- Update the system\n* Reboot\n2. Check the logs\n10) Done\n> Quoted\n";
        assert_eq!(all(text), vec!["Three things:", "Update the system", "Reboot", "Check the logs", "Done", "Quoted"]);
        // Negative numbers and sentences starting with a number survive.
        assert_eq!(all("-5 degrees outside.\n"), vec!["-5 degrees outside."]);
        assert_eq!(all("2020 was a long year.\n"), vec!["2020 was a long year."]);
    }

    #[test]
    fn code_blocks_are_not_read() {
        let text = "Run this:\n```bash\nsudo pacman -Syu. && reboot\necho done\n```\nThen it restarts.";
        let want = vec!["Run this:", CODE_BLOCK, "Then it restarts."];
        assert_eq!(all(text), want);
        assert_eq!(trickled(text), want);
        // A reply that ends inside an unclosed block says nothing more.
        assert_eq!(all("Here:\n```\nls -la\n"), vec!["Here:", CODE_BLOCK]);
    }

    #[test]
    fn long_inline_code_is_not_read() {
        assert_eq!(all("Run `uname -r` to check."), vec!["Run uname -r to check."]);
        assert_eq!(
            all("Run `find / -name '*.pacnew' -newer /etc/pacman.conf 2>/dev/null` now."),
            vec![format!("Run {LONG_CODE} now.")]
        );
    }

    #[test]
    fn inline_code_at_line_start_still_speaks() {
        assert_eq!(trickled("`ls` lists files. More text here."), vec!["ls lists files. More text here."]);
    }
}
