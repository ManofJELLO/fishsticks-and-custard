//! Permission questions: what the agent wants to do, put so it can be shown
//! on a card and asked aloud, and reading a spoken answer. Each backend turns
//! its agent's requests into a [`PermissionAsk`] and an [`Answer`] back into
//! its reply.

pub use crate::lang::AskKind;
use crate::lang::{AnswerWords, Language};

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
    /// The question to read aloud, in English. Other languages ask by `kind`.
    pub spoken: String,
    /// What it is, to ask in other languages.
    pub kind: AskKind,
    /// "Always" would save a rule.
    pub can_always: bool,
}

/// Turn a reply ("yes", "yeah go ahead", "always", "no thanks") into an answer.
///
/// Errs towards safety: anything negative denies (even with "always" in it,
/// as in "no, always ask me first"), and a yes only counts when the reply is
/// nothing but agreement. Questions, hedges ("I'm not sure") and requests
/// ("please tell me what it does first") give `None`, so the question is
/// asked again.
///
/// `lang`'s words count as well as English's.
pub fn parse_answer(text: &str, lang: &Language) -> Option<Answer> {
    let english = &crate::lang::english().answers;
    let lists: Vec<&AnswerWords> = if lang.is_english() { vec![english] } else { vec![&lang.answers, english] };
    // An English word that's an everyday word in `lang` means what it means
    // there: Polish "no dobra" is "yeah, OK", Dutch "is goed" is "fine".
    let ordinary = |w: &str| !lang.is_english() && (lang.answers.consent.contains(&w) || lang.answers.filler.contains(&w));
    let any = |f: fn(&AnswerWords) -> &'static [&'static str]| -> Vec<&'static str> {
        lists
            .iter()
            .flat_map(|l| {
                let own = std::ptr::eq(*l, &lang.answers);
                f(l).iter().copied().filter(move |w| own || !ordinary(w))
            })
            .collect()
    };
    let (always_words, deny, hedges, question_starts, consent, filler, agree) = (
        any(|l| l.always),
        any(|l| l.deny),
        any(|l| l.hedges),
        any(|l| l.question_starts),
        any(|l| l.consent),
        any(|l| l.filler),
        any(|l| l.agree),
    );
    // A question isn't a yes, but it can be a no ("Nie?", as whisper
    // sometimes hears a flat "nie").
    let asked = text.contains(['?', '¿', '？']);
    let t: String = text
        .to_lowercase()
        .replace(['’', '‘'], "'")
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '\'' { c } else { ' ' })
        .collect();
    let words: Vec<&str> = t.split_whitespace().map(|w| match w {
        "dont" => "don't",
        "im" => "i'm",
        w => w,
    }).collect();
    if words.is_empty() || words.len() > 8 {
        return None;
    }
    if question_starts.contains(&words[0]) {
        return None;
    }
    let mut s = format!(" {} ", words.join(" "));
    let has = |s: &str, list: &[&str]| list.iter().any(|w| s.contains(&format!(" {w} ")));

    if has(&s, &hedges) {
        return None;
    }
    // Agreeing phrases that contain a negative word.
    let mut agreed = false;
    for p in &agree {
        if let Some(i) = s.find(&format!(" {p} ")) {
            s.replace_range(i..i + p.len() + 1, "");
            agreed = true;
        }
    }
    let mut always = false;
    for p in &always_words {
        while let Some(i) = s.find(&format!(" {p} ")) {
            s.replace_range(i..i + p.len() + 1, "");
            always = true;
        }
    }
    if has(&s, &deny) {
        return Some(Answer::Deny);
    }
    if asked {
        return None;
    }
    let rest: Vec<&str> = s.split_whitespace().collect();
    if !rest.iter().all(|w| consent.contains(w) || filler.contains(w)) {
        return None;
    }
    agreed |= rest.iter().any(|w| consent.contains(w));
    match (always, agreed) {
        (true, _) => Some(Answer::Always),
        (false, true) => Some(Answer::Once),
        (false, false) => None,
    }
}

/// "Write \"hi\" into note.txt." -> "write \"hi\" into note.txt", so it
/// reads after "May I". Leaves a leading proper noun or acronym alone.
pub(crate) fn phrase(s: &str) -> String {
    let s = s.trim().trim_end_matches('.');
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(a), Some(b)) if a.is_uppercase() && b.is_lowercase() => a.to_lowercase().chain(s[a.len_utf8()..].chars()).collect(),
        _ => s.to_string(),
    }
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
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
            // Negatives win over "always".
            ("No, always ask me first.", Some(Answer::Deny)),
            ("No, and don't ask again.", Some(Answer::Deny)),
            ("Never.", Some(Answer::Deny)),
            ("Not now.", Some(Answer::Deny)),
            ("Wait.", None),
            // Unsure, a question or a request: ask again.
            ("I'm not sure.", None),
            ("Im not sure", None),
            ("Maybe.", None),
            ("Is that okay?", None),
            ("Is that okay", None),
            ("Can you do it", None),
            ("Please tell me what it does first.", None),
            ("Please summarize that file.", None),
            ("Always ask.", None),
            ("Ask me every time.", None),
            // Plain agreement, in many words.
            ("Sure, go for it.", Some(Answer::Once)),
            ("Yes please.", Some(Answer::Once)),
            ("Of course.", Some(Answer::Once)),
            ("Okay, that's fine.", Some(Answer::Once)),
            ("No problem, go ahead.", Some(Answer::Once)),
            ("Allow it this time.", Some(Answer::Once)),
            ("Yes, from now on.", Some(Answer::Always)),
            ("Always allow it.", Some(Answer::Always)),
            ("Yes, and dont ask me again.", Some(Answer::Always)),
        ] {
            assert_eq!(parse_answer(said, crate::lang::english()), want, "{said:?}");
        }
    }

    #[test]
    fn answers_in_other_languages() {
        let check = |code: &str, cases: &[(&str, Option<Answer>)]| {
            let lang = crate::lang::find(code).unwrap();
            for (said, want) in cases {
                assert_eq!(parse_answer(said, lang), *want, "{code}: {said:?}");
            }
        };
        use Answer::*;
        check("de", &[
            ("Ja.", Some(Once)),
            ("Ja, mach das bitte.", Some(Once)),
            ("Klar, kein Problem.", Some(Once)),
            ("Ja, immer.", Some(Always)),
            ("Ja, und frag nicht mehr.", Some(Always)),
            ("Nein.", Some(Deny)),
            ("Nein, immer fragen.", Some(Deny)),
            ("Lieber nicht.", Some(Deny)),
            ("Ich weiß nicht.", None),
            ("Vielleicht.", None),
            ("Was macht das?", None),
            ("Was macht das", None),
            ("Immer fragen.", None),
            // English still works.
            ("Okay.", Some(Once)),
            ("No.", Some(Deny)),
        ]);
        check("fr", &[
            ("Oui.", Some(Once)),
            ("D'accord, vas-y.", Some(Once)),
            ("Oui, toujours.", Some(Always)),
            ("Non merci.", Some(Deny)),
            ("Pas de problème.", Some(Once)),
            ("Je ne sais pas.", None),
            ("Pourquoi ?", None),
        ]);
        check("es", &[
            ("Sí.", Some(Once)),
            ("Sí, adelante, por favor.", Some(Once)),
            ("Vale, siempre.", Some(Always)),
            ("No, gracias.", Some(Deny)),
            ("No hay problema.", Some(Once)),
            ("No sé.", None),
            ("¿Qué hace?", None),
        ]);
        check("ru", &[
            ("Да.", Some(Once)),
            ("Да, конечно.", Some(Once)),
            ("Да, всегда.", Some(Always)),
            ("Нет.", Some(Deny)),
            ("Без проблем.", Some(Once)),
            ("Не знаю.", None),
        ]);
        check("pl", &[("Tak.", Some(Once)), ("Nie.", Some(Deny)), ("Nie?", Some(Deny)), ("Tak?", None), ("Tak, zawsze.", Some(Always)), ("Nie wiem.", None)]);
        check("en", &[("No?", Some(Deny)), ("Yes?", None), ("Okay?", None)]);
        check("pl", &[("No dobra.", Some(Once)), ("No.", None), ("No nie.", Some(Deny))]);
        check("cs", &[("No jasně.", Some(Once)), ("Ne.", Some(Deny))]);
        check("nl", &[("Is goed.", Some(Once)), ("Nee.", Some(Deny))]);
        check("tr", &[("Evet.", Some(Once)), ("Hayır.", Some(Deny)), ("Sorun yok.", Some(Once)), ("Evet, her zaman.", Some(Always))]);
        check("hu", &[("Igen.", Some(Once)), ("Nem.", Some(Deny)), ("Nem vagyok biztos.", None), ("Igen, mindig.", Some(Always))]);
        // Every language: its yes means yes, its no means no.
        for l in &crate::lang::LANGUAGES {
            assert_eq!(parse_answer(l.answers.consent[0], l), Some(Once), "{}", l.code);
            assert_eq!(parse_answer(l.answers.deny[0], l), Some(Deny), "{}", l.code);
        }
    }

    #[test]
    fn phrases() {
        assert_eq!(phrase("Open Firefox."), "open Firefox");
        assert_eq!(phrase("List files"), "list files");
        assert_eq!(phrase("NASA site"), "NASA site");
    }
}
