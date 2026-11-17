//! Wake phrase matching on whisper transcripts.
//!
//! Whisper spells an unusual phrase in many ways ("Fishsticks", "fish
//! sticks", "Fishtix"), so we compare with spaces removed and allow a few
//! character edits. The phrase must start within the first few words
//! ("hey fishsticks ..." is fine; a mention in the middle of a sentence is
//! not a wake-up). The phrase is `voice.wake_phrase`; the tests use the
//! old default, "fishsticks and custard", as well as the new one.

/// Fillers allowed before the phrase at the start of speech ("okay, um,
/// hey fishsticks and custard").
pub const MAX_LEADING_WORDS: usize = 3;

/// What may come before the phrase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lead {
    /// Only up to `MAX_LEADING_WORDS` fillers, so "we had fish sticks and
    /// custard for tea" isn't a wake-up. For speech that started after a pause.
    #[default]
    Fillers,
    /// Up to this many words of anything: for audio cut from the middle of
    /// continuing speech, where the cut lands among other words.
    Any(usize),
}

/// Words people put before a name when they call it: English's (the usual
/// phrase is English, and "hey" is everywhere) and the spoken language's.
/// Not every language's: Polish "no" or Czech "to" before the phrase would
/// wake it in an English sentence.
fn is_filler(word: &str, language: &str) -> bool {
    ["en", language]
        .iter()
        .filter_map(|c| fsc_core::lang::find(c))
        .any(|l| l.wake_fillers.contains(&word))
}

/// Letters a wake phrase needs: shorter ones match ordinary speech.
const MIN_LETTERS: usize = 5;
/// Words a wake phrase may have (the matcher joins up to six).
const MAX_WORDS: usize = 5;

/// `phrase` tidied (trimmed, single spaces), or why it can't be a wake phrase.
pub fn check_phrase(phrase: &str) -> Result<String, String> {
    let tidy = phrase.split_whitespace().collect::<Vec<_>>().join(" ");
    let norm = normalize(&tidy);
    // A Chinese character says as much as a couple of letters.
    let letters: usize = norm.chars().filter(|c| fsc_core::lang::is_word_char(*c)).map(|c| if fsc_core::lang::is_han(c) { 2 } else { 1 }).sum();
    let words = norm.split_whitespace().count();
    if letters < MIN_LETTERS {
        return Err(format!("{tidy:?} is too short for a wake phrase (at least {MIN_LETTERS} letters)"));
    }
    if words > MAX_WORDS {
        return Err(format!("{tidy:?} is too long for a wake phrase (at most {MAX_WORDS} words)"));
    }
    Ok(tidy)
}

/// The phrase as whisper should spell it ("fishsticks" -> "Fishsticks"),
/// for its initial prompt.
pub fn spelling(phrase: &str) -> String {
    phrase
        .split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
        })
        .collect::<Vec<String>>()
        .join(" ")
}

/// Character edits allowed when matching a phrase this long (spaces
/// removed). A short name gets one, or "travis" would wake "jarvis".
fn max_edits(len: usize) -> usize {
    if len < 8 { 1 } else { (len / 5).max(2) }
}

/// If `text` starts with the wake phrase (after a few fillers at most),
/// returns whatever follows it (possibly empty). `None` if it doesn't.
pub fn match_wake(text: &str, phrase: &str) -> Option<String> {
    match_after(text, phrase, Lead::Fillers, "en")
}

/// `match_wake` with `lead` deciding what may come before the phrase.
/// `language`: whose fillers may come before the phrase, besides English's.
pub fn match_wake_after(text: &str, phrase: &str, lead: Lead, language: &str) -> Option<String> {
    match_after(text, phrase, lead, language)
}

/// Whether `text` ends partway into the phrase ("hey, uh... fishsticks
/// and" for "fishsticks and custard"), so it may still be being said.
pub fn could_start(text: &str, phrase: &str) -> bool {
    let target: Vec<char> = sound(&normalize(phrase).split_whitespace().collect::<String>()).chars().collect();
    let words: Vec<String> = normalize(text).split_whitespace().map(sound).collect();
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
    match_after(text, phrase, Lead::Any(usize::MAX), "en").is_some()
}

fn match_after(text: &str, phrase: &str, lead: Lead, language: &str) -> Option<String> {
    let target: String = sound(&normalize(phrase).split_whitespace().collect::<String>());
    // Chinese a character at a time, as if spaced.
    let text = &fsc_core::lang::spaced(text);
    if target.is_empty() {
        return None;
    }
    let max_edits = max_edits(target.chars().count());

    // Normalized words, each remembering which original word it came from.
    let original: Vec<&str> = text.split_whitespace().collect();
    let words: Vec<(String, usize)> = original
        .iter()
        .enumerate()
        .flat_map(|(i, w)| normalize(w).split_whitespace().map(move |n| (n.to_string(), i)).collect::<Vec<_>>())
        .collect();

    let max_leading = match lead {
        Lead::Fillers => MAX_LEADING_WORDS,
        Lead::Any(n) => n,
    };
    for start in 0..=max_leading.min(words.len()) {
        if lead == Lead::Fillers && start > 0 && !is_filler(&words[start - 1].0, language) {
            break;
        }
        // The phrase must start here: without this, a short word before it
        // ("dad", "well") fits in the edit budget and slips through.
        if let Some((first, _)) = words.get(start) {
            let first = sound(first);
            let tchars: Vec<char> = target.chars().collect();
            let n = first.chars().count().min(tchars.len());
            // Against phrase beginnings of about the same length, so a
            // dropped or extra letter ("fistiks") still lines up. Not for
            // short words, which would then pass ("of" against "f").
            let slack = if n >= 5 { 2 } else { 0 };
            let close = (n.saturating_sub(slack)..=(n + slack).min(tchars.len())).any(|m| {
                let head: String = tchars[..m].iter().collect();
                levenshtein(&first, &head) <= (n / 3).max(1)
            });
            if !close {
                continue;
            }
        }
        let mut best: Option<(usize, usize)> = None; // (edits, end)
        let mut joined = String::new();
        for (end, (w, _)) in words.iter().enumerate().skip(start).take(6) {
            joined.push_str(&sound(w));
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
            let rest = fsc_core::lang::unspaced(&parts.join(" "));
            let rest = rest.trim_start_matches(|c: char| !fsc_core::lang::is_word_char(c)).trim().to_string();
            return Some(rest);
        }
    }
    None
}

fn normalize(s: &str) -> String {
    fsc_core::lang::spaced(&fsc_core::lang::simplified(&s.to_lowercase()))
        .replace('&', " and ")
        .chars()
        .map(|c| if fsc_core::lang::is_word_char(c) { c } else { ' ' })
        .collect()
}

/// Spellings that sound alike made alike, so whisper's guesses at a made-up
/// word ("Fistix", "Fischsticks") are judged by how they sound.
fn sound(s: &str) -> String {
    s.replace("sch", "sh").replace("ck", "k").replace("ph", "f").replace('x', "ks").replace('ß', "ss")
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
    use super::{Lead, check_phrase, could_start, match_wake, match_wake_after, mentions, spelling};

    const P: &str = "fishsticks and custard";

    #[test]
    fn chinese_phrase_and_command() {
        let p = check_phrase("小助手").unwrap();
        assert_eq!(match_wake_after("小助手，今天天气怎么样？", &p, Lead::Fillers, "zh").as_deref(), Some("今天天气怎么样？"));
        // Traditional characters, and a filler first.
        assert_eq!(match_wake_after("嘿 小助手 打開音樂", &p, Lead::Fillers, "zh").as_deref(), Some("打開音樂"));
        // Mentioned mid-sentence: no.
        assert_eq!(match_wake_after("我昨天跟小助手说过了", &p, Lead::Fillers, "zh"), None);
        assert!(check_phrase("小助").is_err());
        assert!(check_phrase("助").is_err());
        // The usual phrase, then Chinese.
        assert_eq!(match_wake_after("Fishsticks and custard, 现在几点？", P, Lead::Fillers, "zh").as_deref(), Some("现在几点？"));
    }

    #[test]
    fn hindi_words_stay_whole() {
        assert_eq!(match_wake_after("अरे Fishsticks and custard, क्या समय हुआ है?", P, Lead::Fillers, "hi").as_deref(), Some("क्या समय हुआ है?"));
    }

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
    fn fillers_may_come_first() {
        assert_eq!(match_wake("Okay, um, hey fishsticks and custard, what's on?", P).as_deref(), Some("what's on?"));
        assert_eq!(match_wake("Hmm. Fishsticks and custard?", P).as_deref(), Some(""));
        assert_eq!(match_wake("Hello there, fishsticks and custard.", P).as_deref(), Some(""));
        // Too many, even of fillers.
        assert_eq!(match_wake("Um, uh, so, well, okay, fishsticks and custard", P), None);
        // One real word among them is enough to rule it out.
        assert_eq!(match_wake("Hey Dad fishsticks and custard", P), None);
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
        assert_eq!(match_wake_after(s, P, Lead::Any(6), "en").as_deref(), Some("play some music"));
    }

    #[test]
    fn mentioned_anywhere() {
        assert!(mentions("Sure. As Fishsticks and Custard, I can help with that.", P));
        assert!(mentions("Fish sticks & custard here.", P));
        assert!(!mentions("It's sunny and twelve degrees.", P));
    }
    const F: &str = "fishsticks";

    #[test]
    fn one_word_phrase() {
        assert_eq!(match_wake("Fishsticks, what time is it?", F).as_deref(), Some("what time is it?"));
        assert_eq!(match_wake("Fish sticks. Open Firefox.", F).as_deref(), Some("Open Firefox."));
        assert_eq!(match_wake("Hey Fishstick", F).as_deref(), Some(""));
        assert_eq!(match_wake("I had fish and chips for dinner.", F), None);
        assert_eq!(match_wake("This sticks to the pan.", F), None);
        // Only fillers may come first, so talk about fish sticks doesn't wake it.
        assert_eq!(match_wake("We had fish sticks for tea.", F), None);
        assert_eq!(match_wake("We had fish sticks and custard for tea.", P), None);
        assert_eq!(match_wake("I want fishsticks", F), None);
        assert!(could_start("Hey, um, fish", F));
        assert!(!could_start("What's the weather like", F));
    }

    #[test]
    fn short_names_allow_one_edit() {
        let p = "jarvis";
        assert_eq!(match_wake("Jarvis, lights on", p).as_deref(), Some("lights on"));
        assert_eq!(match_wake("Jarvus, lights on", p).as_deref(), Some("lights on"));
        assert_eq!(match_wake("Travis is here", p), None);
    }

    #[test]
    fn phrases_are_checked() {
        assert_eq!(check_phrase("  hey   computer ").as_deref(), Ok("hey computer"));
        assert_eq!(check_phrase("Fishsticks").as_deref(), Ok("Fishsticks"));
        assert!(check_phrase("").is_err());
        assert!(check_phrase("bob").is_err());
        assert!(check_phrase("!!! ...").is_err());
        assert!(check_phrase("one two three four five six").is_err());
        assert_eq!(spelling("fishsticks and custard"), "Fishsticks And Custard");
        assert_eq!(spelling("fishsticks"), "Fishsticks");
    }

    #[test]
    fn spellings_that_sound_alike() {
        let p = "fishsticks and custard";
        for heard in ["Fistix and Custard. Wie spät ist es?", "Fischstix and Custard.", "Fishtix and Custard, hi"] {
            assert!(match_wake(heard, p).is_some(), "{heard:?}");
        }
        assert_eq!(match_wake("Fistix and Custard. Wie spät ist es?", p).as_deref(), Some("Wie spät ist es?"));
        assert!(match_wake("we had fish sticks and custard for tea", p).is_none());
        assert!(could_start("Hey, Fistix and", p));
        assert!(match_wake("Of fishsticks and custard, what's on?", p).is_none());
    }

    #[test]
    fn fillers_follow_the_language() {
        let p = "fishsticks and custard";
        // Polish "no" isn't an English filler...
        assert!(match_wake("No fishsticks and custard tonight", p).is_none());
        assert!(match_wake_after("No, fishsticks and custard, która godzina?", p, Lead::Fillers, "pl").is_some());
    }
}
