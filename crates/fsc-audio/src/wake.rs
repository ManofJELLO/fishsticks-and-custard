//! Wake phrase matching on whisper transcripts.
//!
//! Whisper spells an unusual phrase in many ways ("Fishsticks and Custard",
//! "fish sticks & custard", "Fishtix and custard"), so we compare with
//! spaces removed and allow a few character edits. The phrase must start
//! within the first few words ("hey fishsticks and custard ..." is fine;
//! a mention in the middle of a sentence is not a wake-up).

/// Words allowed before the phrase, e.g. "hey", "okay".
pub const MAX_LEADING_WORDS: usize = 2;

/// If `text` starts with the wake phrase, returns whatever follows it
/// (possibly empty). `None` if it doesn't.
pub fn match_wake(text: &str, phrase: &str) -> Option<String> {
    match_after(text, phrase, MAX_LEADING_WORDS)
}

/// `match_wake` allowing up to `max_leading` words before the phrase.
pub fn match_wake_after(text: &str, phrase: &str, max_leading: usize) -> Option<String> {
    match_after(text, phrase, max_leading)
}

/// Whether `text` ends partway into the phrase ("hey, uh... fishsticks
/// and"), so it may still be being said.
pub fn could_start(text: &str, phrase: &str) -> bool {
    let target: Vec<char> = normalize(phrase).split_whitespace().flat_map(str::chars).collect();
    let words: Vec<String> = normalize(text).split_whitespace().map(String::from).collect();
    let mut tail = String::new();
    for w in words.iter().rev().take(4) {
        tail.insert_str(0, w);
        let n = tail.chars().count();
        // Too short to mean anything, or as long as the whole phrase (which
        // `match_wake` would have found).
        if n < 4 || n >= target.len() {
            continue;
        }
        // Against phrase beginnings of about the same length, so a dropped
        // or extra letter ("fishstiks") still lines up.
        let close = (n.saturating_sub(2)..=(n + 2).min(target.len())).any(|m| {
            let head: String = target[..m].iter().collect();
            levenshtein(&tail, &head) <= (n / 4).max(1)
        });
        if close {
            return true;
        }
    }
    false
}

/// Whether the phrase appears anywhere in `text`. Used to recognise our
/// own voice saying it, picked up by the mic.
pub fn mentions(text: &str, phrase: &str) -> bool {
    match_after(text, phrase, usize::MAX).is_some()
}

fn match_after(text: &str, phrase: &str, max_leading: usize) -> Option<String> {
    let target: String = normalize(phrase).split_whitespace().collect();
    if target.is_empty() {
        return None;
    }
    let max_edits = (target.chars().count() / 5).max(2);

    // Normalized words, each remembering which original word it came from.
    let original: Vec<&str> = text.split_whitespace().collect();
    let words: Vec<(String, usize)> = original
        .iter()
        .enumerate()
        .flat_map(|(i, w)| normalize(w).split_whitespace().map(move |n| (n.to_string(), i)).collect::<Vec<_>>())
        .collect();

    for start in 0..=max_leading.min(words.len()) {
        let mut best: Option<(usize, usize)> = None; // (edits, end)
        let mut joined = String::new();
        for (end, (w, _)) in words.iter().enumerate().skip(start).take(6) {
            joined.push_str(w);
            let d = levenshtein(&joined, &target);
            if d <= max_edits && best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, end + 1));
            }
            if joined.chars().count() > target.chars().count() + max_edits {
                break;
            }
        }
        if let Some((_, end)) = best {
            // The phrase can end inside an original word ("custard...what"
            // normalizes to two words). Keep only that word's unmatched part,
            // then the original text after it.
            let last_orig = words[end - 1].1;
            let tail_of_word: Vec<&str> =
                words[end..].iter().take_while(|(_, i)| *i == last_orig).map(|(w, _)| w.as_str()).collect();
            let rest_from = words[end..].iter().map(|(_, i)| *i).find(|i| *i != last_orig).unwrap_or(original.len());
            let mut parts: Vec<&str> = tail_of_word;
            parts.extend(&original[rest_from.max(last_orig + 1).min(original.len())..]);
            let rest = parts.join(" ");
            let rest = rest.trim_start_matches(|c: char| !c.is_alphanumeric()).trim().to_string();
            return Some(rest);
        }
    }
    None
}

fn normalize(s: &str) -> String {
    s.to_lowercase()
        .replace('&', " and ")
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect()
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != *cb);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::{could_start, match_wake, match_wake_after, mentions};

    const P: &str = "fishsticks and custard";

    #[test]
    fn plain_and_variants() {
        assert_eq!(match_wake("Fishsticks and custard.", P).as_deref(), Some(""));
        assert_eq!(match_wake("Fish sticks & Custard!", P).as_deref(), Some(""));
        assert_eq!(match_wake("Fishtix and custard", P).as_deref(), Some(""));
        assert_eq!(match_wake("fish-sticks and custard", P).as_deref(), Some(""));
    }

    #[test]
    fn command_follows() {
        assert_eq!(
            match_wake("Fishsticks and Custard, what is the weather like today?", P).as_deref(),
            Some("what is the weather like today?")
        );
        assert_eq!(match_wake("Hey fish sticks and custard. Open Firefox.", P).as_deref(), Some("Open Firefox."));
    }

    #[test]
    fn phrase_glued_to_command() {
        assert_eq!(match_wake("Fishsticks and custard...what time is it", P).as_deref(), Some("what time is it"));
        assert_eq!(match_wake("Fishsticks and custard,what's up?", P).as_deref(), Some("what s up?"));
    }

    #[test]
    fn rejects_other_speech() {
        assert_eq!(match_wake("I had fish and chips for dinner.", P), None);
        assert_eq!(match_wake("What time is it?", P), None);
        assert_eq!(match_wake("", P), None);
        // Mentioned mid-sentence: not a wake-up.
        assert_eq!(match_wake("My kids were laughing about the Doctor eating fish sticks and custard.", P), None);
    }

    #[test]
    fn partway_into_the_phrase() {
        assert!(could_start("Hey, uh, fishsticks and", P));
        assert!(could_start("Fish sticks", P));
        assert!(could_start("Okay, fish sticks an", P));
        assert!(could_start("hey fishstiks", P));
        assert!(!could_start("What's the weather like", P));
        assert!(!could_start("I went to the shops", P));
        assert!(!could_start("", P));
    }

    #[test]
    fn more_leading_words_when_allowed() {
        let s = "and then they went home. Fishsticks and custard, play some music";
        assert_eq!(match_wake(s, P), None);
        assert_eq!(match_wake_after(s, P, 6).as_deref(), Some("play some music"));
    }

    #[test]
    fn mentioned_anywhere() {
        assert!(mentions("Sure. As Fishsticks and Custard, I can help with that.", P));
        assert!(mentions("Fish sticks & custard here.", P));
        assert!(!mentions("It's sunny and twelve degrees.", P));
    }
}
