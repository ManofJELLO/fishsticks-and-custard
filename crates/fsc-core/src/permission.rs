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

/// Places that hold keys, passwords and logins, as Claude Code permission
/// patterns. A web page the agent reads can carry instructions, so the agent
/// must never read these unasked: Claude's seeded settings deny
/// `Read(<pattern>)` for each (a test keeps them in step), and an ACP agent
/// reading one is asked about even when reads are otherwise allowed.
pub const SECRET_PATHS: &[&str] = &[
    "~/.ssh/**",
    "~/.gnupg/**",
    "~/.password-store/**",
    "~/.local/share/keyrings/**",
    "~/.mozilla/**",
    "~/.config/mozilla/**",
    "~/.librewolf/**",
    "~/.zen/**",
    "~/.config/google-chrome/**",
    "~/.config/chromium/**",
    "~/.config/BraveSoftware/**",
    "~/.config/vivaldi/**",
    "~/.config/microsoft-edge/**",
    "~/.aws/**",
    "~/.kube/**",
    "~/.docker/**",
    "~/.config/gh/**",
    "~/.config/rclone/**",
    "~/.netrc",
    "~/.git-credentials",
    "~/.pgpass",
    "~/.claude/.credentials.json",
    "~/.codex/auth.json",
    "~/.gemini/oauth_creds.json",
    "~/.gemini/antigravity-cli/antigravity-oauth-token",
    "~/.local/share/opencode/auth.json",
    // macOS: the keychains, browser profiles and Apple's own apps.
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

/// `path` is, or is inside, one of the [`SECRET_PATHS`] under `home`.
/// `..` is resolved first, and symlinks too when the path exists, so
/// `~/notes/../.ssh/id_ed25519` and links into `~/.ssh` are caught.
pub fn is_secret(path: &std::path::Path, home: &std::path::Path) -> bool {
    use std::path::{Component, PathBuf};
    let path = if path.is_absolute() { path.to_path_buf() } else { home.join(path) };
    let mut lexical = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                lexical.pop();
            }
            Component::CurDir => {}
            c => lexical.push(c),
        }
    }
    let real = std::fs::canonicalize(&lexical).ok();
    SECRET_PATHS.iter().any(|pattern| {
        let rest = pattern.trim_start_matches("~/").trim_end_matches("/**");
        let secret = home.join(rest);
        let real_secret = std::fs::canonicalize(&secret).ok();
        let inside = |p: &std::path::Path| under(p, &secret) || real_secret.as_deref().is_some_and(|s| under(p, s));
        inside(&lexical) || real.as_deref().is_some_and(inside)
    })
}

/// `p` is `dir` or inside it. macOS and Windows file systems ignore case
/// (by default), so there `~/Library/keychains` is `~/Library/Keychains`.
fn under(p: &std::path::Path, dir: &std::path::Path) -> bool {
    if cfg!(any(target_os = "macos", windows)) {
        let low = |x: &std::path::Path| std::path::PathBuf::from(x.to_string_lossy().to_lowercase());
        low(p).starts_with(low(dir))
    } else {
        p.starts_with(dir)
    }
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
    // Phrases are read the way the reply is: "tidak apa-apa" as "tidak apa
    // apa", Chinese a character at a time.
    let any = |f: fn(&AnswerWords) -> &'static [&'static str]| -> Vec<String> {
        lists
            .iter()
            .flat_map(|l| {
                let own = std::ptr::eq(*l, &lang.answers);
                f(l).iter().copied().filter(move |w| own || !ordinary(w))
            })
            .map(|w| words_of(w).join(" "))
            .filter(|w| !w.is_empty())
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
    let words = words_of(text);
    // Chinese counts each character, so it may run longer.
    let max_words = if text.chars().any(crate::lang::is_han) { 16 } else { 8 };
    if words.is_empty() || words.len() > max_words {
        return None;
    }
    let mut s = format!(" {} ", words.join(" "));
    let has = |s: &str, list: &[String]| list.iter().any(|w| s.contains(&format!(" {w} ")));
    if question_starts.iter().any(|q| s.starts_with(&format!(" {q} "))) {
        return None;
    }
    if has(&s, &hedges) {
        return None;
    }
    // Agreeing phrases that contain a negative word.
    let mut agreed = false;
    for p in &agree {
        while let Some(i) = s.find(&format!(" {p} ")) {
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
    // What's left must be all yes and filler: take those out, longest
    // first (so "好的" goes before "好"), and nothing may remain.
    let mut rest: Vec<(&String, bool)> = consent.iter().map(|w| (w, true)).chain(filler.iter().map(|w| (w, false))).collect();
    rest.sort_by_key(|(w, _)| std::cmp::Reverse(w.len()));
    for (p, yes) in rest {
        while let Some(i) = s.find(&format!(" {p} ")) {
            s.replace_range(i..i + p.len() + 1, "");
            agreed |= yes;
        }
    }
    if !s.trim().is_empty() {
        return None;
    }
    match (always, agreed) {
        (true, _) => Some(Answer::Always),
        (false, true) => Some(Answer::Once),
        (false, false) => None,
    }
}

/// A reply, or a listed phrase, as lower-case words without punctuation.
fn words_of(text: &str) -> Vec<String> {
    let t: String = text
        .to_lowercase()
        .replace(['’', '‘'], "'")
        .chars()
        .map(|c| if crate::lang::is_word_char(c) || c == '\'' { c } else { ' ' })
        .collect();
    crate::lang::spaced(&crate::lang::simplified(&t))
        .split_whitespace()
        .map(|w| match w {
            "dont" => "don't".to_string(),
            "im" => "i'm".to_string(),
            w => w.to_string(),
        })
        .collect()
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
    fn asian_answers() {
        let l = |c| crate::lang::find(c).unwrap();
        let cases: &[(&str, &str, Option<Answer>)] = &[
            ("zh", "好的。", Some(Answer::Once)),
            ("zh", "可以，没问题。", Some(Answer::Once)),
            ("zh", "好，以后不用问。", Some(Answer::Always)),
            ("zh", "不行。", Some(Answer::Deny)),
            ("zh", "不要。", Some(Answer::Deny)),
            ("zh", "沒問題", Some(Answer::Once)),
            ("zh", "我不知道。", None),
            ("zh", "可以吗？", None),
            ("zh", "这是什么？", None),
            ("ko", "네.", Some(Answer::Once)),
            ("ko", "네, 해도 돼요.", Some(Answer::Once)),
            ("ko", "네, 항상.", Some(Answer::Always)),
            ("ko", "아니요.", Some(Answer::Deny)),
            ("ko", "안 돼요.", Some(Answer::Deny)),
            ("ko", "잘 모르겠어요.", None),
            ("ko", "괜찮아요.", None),
            ("hi", "बस करो।", Some(Answer::Deny)),
            ("hi", "हाँ।", Some(Answer::Once)),
            ("hi", "हां, ठीक है।", Some(Answer::Once)),
            ("hi", "हाँ, हमेशा।", Some(Answer::Always)),
            ("hi", "नहीं।", Some(Answer::Deny)),
            ("hi", "कोई बात नहीं।", Some(Answer::Once)),
            ("hi", "पता नहीं।", None),
            ("vi", "Có.", Some(Answer::Once)),
            ("vi", "Vâng, được.", Some(Answer::Once)),
            ("vi", "Có, từ giờ.", Some(Answer::Always)),
            ("vi", "Không.", Some(Answer::Deny)),
            ("vi", "Không sao.", Some(Answer::Once)),
            ("vi", "Không biết.", None),
            ("id", "Ya.", Some(Answer::Once)),
            ("id", "Iya, boleh.", Some(Answer::Once)),
            ("id", "Ya, selalu.", Some(Answer::Always)),
            ("id", "Tidak.", Some(Answer::Deny)),
            ("id", "Tidak apa-apa.", Some(Answer::Once)),
            ("id", "Mungkin.", None),
        ];
        for (code, text, want) in cases {
            assert_eq!(parse_answer(text, l(code)), *want, "{code}: {text}");
        }
    }

    #[test]
    fn secret_paths_are_recognised() {
        let home = std::path::Path::new("/home/u");
        assert!(is_secret(std::path::Path::new("/home/u/.ssh/id_ed25519"), home));
        assert!(is_secret(std::path::Path::new("/home/u/.ssh"), home));
        assert!(is_secret(std::path::Path::new("/home/u/notes/../.ssh/config"), home));
        assert!(is_secret(std::path::Path::new(".netrc"), home));
        assert!(is_secret(std::path::Path::new("/home/u/.config/BraveSoftware/Brave-Browser/Default/Login Data"), home));
        assert!(!is_secret(std::path::Path::new("/home/u/.sshrc"), home));
        assert!(!is_secret(std::path::Path::new("/home/u/notes/ssh.txt"), home));
        assert!(!is_secret(std::path::Path::new("/home/u/.claude/settings.json"), home));
        let mac = std::path::Path::new("/Users/u");
        assert!(is_secret(std::path::Path::new("/Users/u/Library/Keychains/login.keychain-db"), mac));
        assert!(is_secret(std::path::Path::new("/Users/u/Library/Application Support/Google/Chrome/Default/Cookies"), mac));
        assert!(!is_secret(std::path::Path::new("/Users/u/Library/Application Support/fsc/config.toml"), mac));
    }

    /// macOS and Windows ignore case in paths, so the check must too.
    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn secret_paths_ignore_case_where_files_do() {
        let home = std::path::Path::new("/Users/u");
        assert!(is_secret(std::path::Path::new("/Users/u/library/KEYCHAINS/login.keychain-db"), home));
        assert!(is_secret(std::path::Path::new("/users/U/.SSH/id_ed25519"), home));
    }

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
