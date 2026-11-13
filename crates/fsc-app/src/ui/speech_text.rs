//! Turns streaming reply text into speakable sentences, so speech starts
//! with the first sentence instead of waiting for the whole reply.

/// Below this many characters, keep accumulating rather than speaking a
/// fragment like "Yes." on its own (fewer gaps between clips).
const MIN_CHUNK: usize = 24;

#[derive(Default)]
pub struct SentenceBuffer {
    buf: String,
}

impl SentenceBuffer {
    pub fn clear(&mut self) {
        self.buf.clear();
    }

    /// Add streamed text; returns any sentences that are now complete.
    pub fn push(&mut self, text: &str) -> Vec<String> {
        self.buf.push_str(text);
        let mut out = Vec::new();
        while let Some(end) = self.boundary() {
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
        speakable(&s)
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

/// Strip markdown symbols that would be read aloud; `None` if nothing left.
fn speakable(s: &str) -> Option<String> {
    let cleaned: String = s.chars().filter(|c| !matches!(c, '*' | '`' | '#' | '|')).collect();
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    cleaned.chars().any(|c| c.is_alphanumeric()).then_some(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
