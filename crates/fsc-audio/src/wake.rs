//! Wake phrase matching on whisper transcripts.
//!
//! Whisper spells an unusual phrase in many ways ("Fishsticks and Custard",
//! "fish sticks & custard", "Fishtix and custard"), so we compare with
//! spaces removed and allow a few character edits. The phrase must start
//! within the first few words ("hey fishsticks and custard ..." is fine;
//! a mention in the middle of a sentence is not a wake-up).

/// Words allowed before the phrase, e.g. "hey", "okay".
const MAX_LEADING_WORDS: usize = 2;

/// If `text` starts with the wake phrase, returns whatever follows it
/// (possibly empty). `None` if it doesn't.
pub fn match_wake(text: &str, phrase: &str) -> Option<String> {
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

    for start in 0..=MAX_LEADING_WORDS.min(words.len()) {
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
    use super::match_wake;

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
}
