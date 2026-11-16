//! The languages the assistant can talk in: for each, the Piper voice to
//! speak with, what it says itself (permission questions, prompts), and the
//! words that answer a permission question or come before the wake phrase.
//!
//! `voice.language` picks one by code ("de"); empty follows the desktop's
//! locale. Speech recognition (whisper) uses the same code.

/// What a permission question is about, so it can be asked in any language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AskKind {
    Command,
    Edit,
    Write,
    Delete,
    Move,
    Read,
    WebSearch,
    OpenPage,
    SearchFiles,
    Think,
    /// Some other tool, named on the card.
    #[default]
    Tool,
}

/// Words that answer a permission question. Read together with English's,
/// since many people say "okay" or "no" whatever they speak.
pub struct AnswerWords {
    /// Mean "allow and don't ask again" (removed before the rest is read).
    pub always: &'static [&'static str],
    /// Any of these anywhere means no.
    pub deny: &'static [&'static str],
    /// Unsure: ask again rather than guess.
    pub hedges: &'static [&'static str],
    /// A reply starting with one of these is a question.
    pub question_starts: &'static [&'static str],
    /// Words that say yes.
    pub consent: &'static [&'static str],
    /// Words that may go with a yes without changing it.
    pub filler: &'static [&'static str],
    /// Agreeing phrases that contain a negative word ("no problem").
    pub agree: &'static [&'static str],
}

/// What the assistant says itself.
pub struct Phrases {
    /// The question for each `AskKind`, in its order; `{}` is the tool.
    pub questions: [&'static str; 11],
    /// For a tool it can't name.
    pub use_a_tool: &'static str,
    pub yes_always_no: &'static str,
    pub yes_no: &'static str,
    /// Before asking again after an answer it couldn't read.
    pub sorry: &'static str,
    /// Said by a newly chosen voice.
    pub sample: &'static str,
    /// Said instead of a code block.
    pub code_on_screen: &'static str,
    /// Said in place of long inline code, mid-sentence.
    pub command_on_screen: &'static str,
}

pub struct Language {
    /// ISO 639-1, as whisper and `voice.language` take it.
    pub code: &'static str,
    /// In English, for the agent's instructions.
    pub name: &'static str,
    /// In itself, for the menu.
    pub native: &'static str,
    /// (region, Piper voice): the first whose region matches the locale's,
    /// else the one with region "".
    pub voices: &'static [(&'static str, &'static str)],
    pub answers: AnswerWords,
    /// Words people put before a name when they call it.
    pub wake_fillers: &'static [&'static str],
    pub phrases: Phrases,
}

impl Language {
    /// The voice for `region` ("DE", "BR"; "" for none).
    pub fn voice(&self, region: &str) -> &'static str {
        self.voices
            .iter()
            .find(|(r, _)| !r.is_empty() && r.eq_ignore_ascii_case(region))
            .or_else(|| self.voices.iter().find(|(r, _)| r.is_empty()))
            .map(|(_, v)| *v)
            .unwrap_or(self.voices[0].1)
    }

    /// The permission question for `kind`, about `tool` where it names one
    /// (an ACP agent's unnamed tool comes as "Tool").
    pub fn question(&self, kind: AskKind, tool: &str) -> String {
        let tool = tool.trim();
        if kind == AskKind::Tool && (tool.is_empty() || tool == "Tool") {
            return self.phrases.use_a_tool.to_string();
        }
        self.phrases.questions[kind as usize].replace("{}", tool)
    }

    pub fn is_english(&self) -> bool {
        self.code == "en"
    }

    /// For the agent's instructions; `None` for English, which needs none.
    pub fn instruction(&self) -> Option<String> {
        (!self.is_english()).then(|| {
            format!(
                "The user speaks {} ({}). Reply in {} unless they ask for another language.",
                self.name, self.native, self.name
            )
        })
    }
}

pub fn english() -> &'static Language {
    &LANGUAGES[0]
}

pub fn find(code: &str) -> Option<&'static Language> {
    let code = code.trim();
    let code = if matches!(code, "nb" | "nn") { "no" } else { code };
    LANGUAGES.iter().find(|l| l.code.eq_ignore_ascii_case(code))
}

/// What `voice.language` comes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spoken {
    /// For whisper: a language code, or "auto".
    pub whisper: String,
    /// The table used for what's said and heard (English for a language
    /// that isn't in the list).
    pub code: &'static str,
    /// From the locale, to pick a regional voice ("BR").
    pub region: String,
}

impl Spoken {
    pub fn language(&self) -> &'static Language {
        find(self.code).unwrap_or_else(english)
    }

    /// What the agent is told about the user's language: nothing for
    /// English; for "auto" or a language that isn't listed, to answer in
    /// whatever the user speaks.
    pub fn instruction(&self) -> Option<String> {
        if self.whisper != self.code {
            return Some("Reply in the language the user speaks to you in.".into());
        }
        self.language().instruction()
    }
}

/// `voice.language`: empty follows the desktop's locale; a code from the
/// list uses that; anything else ("ja", "auto") goes to whisper as it is,
/// with English for the rest.
pub fn resolve(setting: &str) -> Spoken {
    resolve_with(setting, &locale())
}

fn resolve_with(setting: &str, locale: &str) -> Spoken {
    let (loc_lang, region) = parse_locale(locale);
    let setting = setting.trim();
    if setting.is_empty() {
        let lang = find(&loc_lang).unwrap_or_else(english);
        return Spoken { whisper: lang.code.into(), code: lang.code, region };
    }
    let setting = setting.to_lowercase();
    match find(&setting) {
        // A region only counts when it's this language's ("pt" with pt_BR).
        Some(lang) => {
            let region = if find(&loc_lang).is_some_and(|l| l.code == lang.code) { region } else { String::new() };
            Spoken { whisper: lang.code.into(), code: lang.code, region }
        }
        None => Spoken { whisper: setting, code: "en", region: String::new() },
    }
}

/// The desktop's language: LC_ALL, else LC_MESSAGES, else LANG.
pub fn locale() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.trim().is_empty())
        .unwrap_or_default()
}

/// "de_DE.UTF-8@euro" -> ("de", "DE"). "C" and "POSIX" are no language.
fn parse_locale(locale: &str) -> (String, String) {
    let base = locale.split(['.', '@']).next().unwrap_or("").trim();
    if base.is_empty() || base == "C" || base == "POSIX" {
        return (String::new(), String::new());
    }
    let mut parts = base.splitn(2, ['_', '-']);
    let lang = parts.next().unwrap_or("").to_lowercase();
    let region = parts.next().unwrap_or("").to_uppercase();
    (lang, region)
}

pub static LANGUAGES: [Language; 18] = [
    Language {
        code: "en",
        name: "English",
        native: "English",
        voices: &[("", "en_GB-alan-medium")],
        answers: AnswerWords {
            always: &["don't ask again", "don't ask me again", "every time", "from now on", "always"],
            deny: &["no", "n", "nope", "nah", "not", "don't", "do not", "deny", "denied", "stop", "cancel", "never", "negative"],
            hedges: &[
                "not sure", "unsure", "maybe", "perhaps", "i think", "i guess", "dunno", "don't know", "idk", "hmm", "hm",
                "possibly", "probably", "depends", "wait", "hold on",
            ],
            question_starts: &[
                "what", "what's", "why", "how", "who", "where", "when", "which", "is", "isn't", "are", "can", "could",
                "would", "should", "does", "will", "shall",
            ],
            consent: &[
                "yes", "y", "yeah", "yea", "yep", "yup", "yah", "ya", "sure", "ok", "okay", "k", "allow", "allowed", "fine",
                "alright", "absolutely", "course", "proceed", "affirmative", "approve", "approved", "go", "ahead",
                "certainly", "definitely", "please", "granted", "permission",
            ],
            filler: &[
                "and", "then", "just", "it", "that", "that's", "this", "is", "you", "can", "may", "for", "of", "all", "right",
                "do", "oh", "um", "uh", "er", "well", "thanks", "thank", "once", "time", "now", "good", "great", "sounds",
                "totally", "i", "i'm", "a", "the", "one", "have",
            ],
            agree: &["no problem", "no worries", "why not"],
        },
        wake_fillers: &[
            "hey", "hi", "hello", "hiya", "oi", "yo", "ok", "okay", "oh", "um", "umm", "uh", "uhh", "er", "erm", "ah",
            "eh", "hm", "hmm", "mm", "so", "right", "alright", "well", "now", "there", "please",
        ],
        phrases: Phrases {
            questions: [
                "May I run a command?",
                "May I edit a file?",
                "May I write a file?",
                "May I delete a file?",
                "May I move a file?",
                "May I read a file?",
                "May I search the web?",
                "May I open a web page?",
                "May I search your files?",
                "May I think it through?",
                "May I use {}?",
            ],
            yes_always_no: "Yes, always, or no?",
            yes_no: "Yes or no?",
            use_a_tool: "May I use a tool?",
            sorry: "Sorry.",
            sample: "Hello. This is how I sound.",
            code_on_screen: "The code is on screen.",
            command_on_screen: "the command on screen",
        },
    },
    Language {
        code: "de",
        name: "German",
        native: "Deutsch",
        voices: &[("", "de_DE-thorsten-medium")],
        answers: AnswerWords {
            always: &[
                "nicht mehr fragen", "nicht wieder fragen", "frag nicht mehr", "frag nicht wieder", "frag mich nicht mehr",
                "ab jetzt", "von jetzt an", "jedes mal", "immer",
            ],
            deny: &["nein", "nee", "ne", "nö", "nicht", "kein", "keine", "nie", "niemals", "stopp", "abbrechen", "ablehnen", "verweigern", "verboten"],
            hedges: &[
                "weiß nicht", "weiss nicht", "keine ahnung", "vielleicht", "eventuell", "unsicher", "nicht sicher",
                "ich glaube", "glaube ich", "mal sehen", "moment", "warte", "augenblick", "kommt darauf an", "hmm",
            ],
            question_starts: &[
                "was", "warum", "wieso", "weshalb", "wie", "wer", "wo", "wann", "welche", "welcher", "welches", "ist",
                "sind", "kannst", "könntest", "würdest", "sollte", "wird", "hast",
            ],
            consent: &[
                "ja", "jo", "jawohl", "jep", "klar", "sicher", "gut", "einverstanden", "erlaubt", "erlauben", "genehmigt",
                "gerne", "gern", "natürlich", "selbstverständlich", "mach", "los", "ordnung", "passt", "absolut", "freilich",
                "logo", "weiter", "okay", "ok",
            ],
            filler: &[
                "und", "dann", "einfach", "nur", "es", "das", "dies", "du", "ich", "mal", "danke", "schön", "sehr", "bitte",
                "ruhig", "doch", "also", "äh", "ähm", "na", "diesmal", "jetzt", "in", "die", "der", "den", "ein", "eine",
                "einmal", "darfst", "machen", "ist",
            ],
            agree: &["kein problem", "keine sorge", "kein ding", "warum nicht", "nichts dagegen"],
        },
        wake_fillers: &["hey", "hallo", "he", "ok", "okay", "also", "äh", "ähm", "öhm", "hm", "na", "so", "ja", "gut", "bitte"],
        phrases: Phrases {
            questions: [
                "Darf ich einen Befehl ausführen?",
                "Darf ich eine Datei ändern?",
                "Darf ich eine Datei schreiben?",
                "Darf ich eine Datei löschen?",
                "Darf ich eine Datei verschieben?",
                "Darf ich eine Datei lesen?",
                "Darf ich im Internet suchen?",
                "Darf ich eine Webseite öffnen?",
                "Darf ich deine Dateien durchsuchen?",
                "Darf ich darüber nachdenken?",
                "Darf ich {} benutzen?",
            ],
            yes_always_no: "Ja, immer, oder nein?",
            yes_no: "Ja oder nein?",
            use_a_tool: "Darf ich ein Werkzeug benutzen?",
            sorry: "Entschuldigung.",
            sample: "Hallo. So klinge ich.",
            code_on_screen: "Der Code steht auf dem Bildschirm.",
            command_on_screen: "(siehe Bildschirm)",
        },
    },
    Language {
        code: "fr",
        name: "French",
        native: "Français",
        voices: &[("", "fr_FR-siwis-medium")],
        answers: AnswerWords {
            always: &[
                "ne demande plus", "ne me demande plus", "ne redemande pas", "à chaque fois", "chaque fois", "désormais",
                "dorénavant", "à partir de maintenant", "toujours",
            ],
            deny: &["non", "nan", "pas", "ne", "jamais", "arrête", "arrêter", "stop", "annule", "annuler", "refuse", "refusé", "interdit"],
            hedges: &[
                "je sais pas", "je ne sais pas", "sais pas", "peut être", "pas sûr", "pas sûre", "je pense", "je crois",
                "attends", "attendez", "on verra", "ça dépend", "bof", "hmm",
            ],
            question_starts: &[
                "quoi", "que", "qu'est", "pourquoi", "comment", "qui", "où", "quand", "quel", "quelle", "quels", "quelles",
                "est", "pourrais", "pourrait", "devrais",
            ],
            consent: &[
                "oui", "ouais", "ouaip", "d'accord", "daccord", "accord", "ok", "okay", "oké", "bien", "sûr", "certainement",
                "absolument", "vas", "allez", "autorisé", "autorise", "permis", "volontiers", "parfait", "entendu", "carrément",
                "évidemment",
            ],
            filler: &[
                "et", "alors", "juste", "le", "la", "les", "ça", "ca", "cela", "c'est", "tu", "je", "peux", "merci", "bon",
                "euh", "ben", "bah", "oh", "ah", "y", "cette", "fois", "maintenant", "un", "une", "très", "fais", "faire",
                "l'", "s'il", "te", "plaît", "vous", "plait",
            ],
            agree: &["pas de problème", "pas de souci", "pas de soucis", "pourquoi pas", "sans problème", "aucun problème"],
        },
        wake_fillers: &["hé", "hey", "salut", "bonjour", "ok", "okay", "euh", "ben", "bon", "alors", "oh", "ah", "dis", "allô", "eh"],
        phrases: Phrases {
            questions: [
                "Puis-je exécuter une commande ?",
                "Puis-je modifier un fichier ?",
                "Puis-je écrire un fichier ?",
                "Puis-je supprimer un fichier ?",
                "Puis-je déplacer un fichier ?",
                "Puis-je lire un fichier ?",
                "Puis-je chercher sur le web ?",
                "Puis-je ouvrir une page web ?",
                "Puis-je chercher dans tes fichiers ?",
                "Puis-je y réfléchir ?",
                "Puis-je utiliser {} ?",
            ],
            yes_always_no: "Oui, toujours, ou non ?",
            yes_no: "Oui ou non ?",
            use_a_tool: "Puis-je utiliser un outil ?",
            sorry: "Pardon.",
            sample: "Bonjour. Voici ma voix.",
            code_on_screen: "Le code est à l'écran.",
            command_on_screen: "(voir l'écran)",
        },
    },
    Language {
        code: "es",
        name: "Spanish",
        native: "Español",
        voices: &[
            ("ES", "es_ES-davefx-medium"),
            ("", "es_MX-ald-medium"),
        ],
        answers: AnswerWords {
            always: &[
                "no preguntes más", "no me preguntes más", "no vuelvas a preguntar", "cada vez", "de ahora en adelante",
                "a partir de ahora", "siempre",
            ],
            deny: &["no", "nunca", "jamás", "detente", "alto", "basta", "cancela", "cancelar", "deniega", "rechaza", "ni"],
            hedges: &[
                "no sé", "no se", "quizás", "quizá", "tal vez", "a lo mejor", "no estoy seguro", "no estoy segura", "creo",
                "supongo", "espera", "un momento", "depende", "mmm", "hmm",
            ],
            question_starts: &["qué", "cómo", "quién", "dónde", "cuándo", "cuál", "puedes", "podrías", "deberías"],
            consent: &[
                "sí", "si", "vale", "claro", "ok", "okay", "dale", "adelante", "venga", "bueno", "bien", "perfecto", "correcto",
                "seguro", "luego", "autorizo", "autorizado", "permitido", "permito", "hazlo", "procede", "órale", "sale",
                "exacto", "afirmativo", "genial", "acuerdo",
            ],
            filler: &[
                "y", "entonces", "solo", "sólo", "lo", "la", "el", "eso", "esto", "es", "por", "favor", "gracias", "muchas",
                "pues", "eh", "este", "esta", "vez", "ahora", "una", "un", "todo", "muy", "de", "desde", "ya", "va",
            ],
            agree: &["no hay problema", "sin problema", "no pasa nada", "por qué no", "cómo no", "no te preocupes"],
        },
        wake_fillers: &["oye", "hola", "ey", "eh", "ok", "vale", "bueno", "este", "pues", "mira", "em", "ehm"],
        phrases: Phrases {
            questions: [
                "¿Puedo ejecutar un comando?",
                "¿Puedo modificar un archivo?",
                "¿Puedo escribir un archivo?",
                "¿Puedo borrar un archivo?",
                "¿Puedo mover un archivo?",
                "¿Puedo leer un archivo?",
                "¿Puedo buscar en internet?",
                "¿Puedo abrir una página web?",
                "¿Puedo buscar en tus archivos?",
                "¿Puedo pensarlo?",
                "¿Puedo usar {}?",
            ],
            yes_always_no: "¿Sí, siempre, o no?",
            yes_no: "¿Sí o no?",
            use_a_tool: "¿Puedo usar una herramienta?",
            sorry: "Perdón.",
            sample: "Hola. Así suena mi voz.",
            code_on_screen: "El código está en la pantalla.",
            command_on_screen: "(ver la pantalla)",
        },
    },
    Language {
        code: "it",
        name: "Italian",
        native: "Italiano",
        voices: &[("", "it_IT-paola-medium")],
        answers: AnswerWords {
            always: &[
                "non chiedere più", "non chiedermelo più", "non chiedermi più", "ogni volta", "d'ora in poi", "da ora in poi",
                "sempre",
            ],
            deny: &["no", "non", "mai", "ferma", "fermati", "stop", "basta", "annulla", "nega", "rifiuta", "negato"],
            hedges: &[
                "non so", "boh", "forse", "magari", "non sono sicuro", "non sono sicura", "penso", "credo", "aspetta",
                "un attimo", "un momento", "dipende", "mah", "hmm",
            ],
            question_starts: &["cosa", "che", "perché", "come", "chi", "dove", "quando", "quale", "quali", "potresti", "dovresti"],
            consent: &[
                "sì", "si", "certo", "ok", "okay", "bene", "d'accordo", "daccordo", "accordo", "vai", "procedi", "fallo",
                "sicuro", "sicuramente", "assolutamente", "perfetto", "autorizzato", "permesso", "consentito", "ovvio",
                "ovviamente", "prego", "volentieri", "esatto", "giusto",
            ],
            filler: &[
                "e", "allora", "poi", "solo", "lo", "la", "il", "questo", "questa", "quello", "per", "favore", "grazie",
                "mille", "beh", "eh", "ehm", "ah", "oh", "pure", "anche", "volta", "adesso", "ora", "una", "un", "tutto",
                "molto", "fai", "puoi", "va",
            ],
            agree: &["nessun problema", "non c'è problema", "perché no", "figurati", "non preoccuparti"],
        },
        wake_fillers: &["ehi", "ciao", "ok", "okay", "allora", "ehm", "eh", "beh", "senti", "dai", "oh", "ah"],
        phrases: Phrases {
            questions: [
                "Posso eseguire un comando?",
                "Posso modificare un file?",
                "Posso scrivere un file?",
                "Posso eliminare un file?",
                "Posso spostare un file?",
                "Posso leggere un file?",
                "Posso cercare sul web?",
                "Posso aprire una pagina web?",
                "Posso cercare nei tuoi file?",
                "Posso pensarci?",
                "Posso usare {}?",
            ],
            yes_always_no: "Sì, sempre, o no?",
            yes_no: "Sì o no?",
            use_a_tool: "Posso usare uno strumento?",
            sorry: "Scusa.",
            sample: "Ciao. Ecco come suono.",
            code_on_screen: "Il codice è sullo schermo.",
            command_on_screen: "(vedi lo schermo)",
        },
    },
    Language {
        code: "pt",
        name: "Portuguese",
        native: "Português",
        voices: &[
            ("PT", "pt_PT-tugão-medium"),
            ("", "pt_BR-faber-medium"),
        ],
        answers: AnswerWords {
            always: &[
                "não pergunte mais", "não me pergunte mais", "não precisa perguntar", "não perguntes mais", "toda vez",
                "todas as vezes", "de agora em diante", "a partir de agora", "sempre",
            ],
            deny: &["não", "nao", "nunca", "jamais", "pare", "cancela", "cancelar", "nega", "negado", "recusa", "nem"],
            hedges: &[
                "não sei", "nao sei", "talvez", "pode ser", "não tenho certeza", "acho", "espera", "espere", "um momento",
                "depende", "hmm",
            ],
            question_starts: &["que", "quê", "porque", "como", "quem", "onde", "quando", "qual", "quais"],
            consent: &[
                "sim", "claro", "ok", "okay", "pode", "podes", "beleza", "certo", "tá", "ta", "bem", "bom", "combinado",
                "vai", "manda", "autorizo", "autorizado", "permitido", "certeza", "perfeito", "isso", "exato", "fechou",
            ],
            filler: &[
                "e", "então", "entao", "só", "o", "a", "isto", "por", "favor", "obrigado", "obrigada", "eh", "ah", "oh",
                "né", "essa", "esta", "vez", "agora", "um", "uma", "tudo", "muito", "faz", "fazer", "com", "pois",
            ],
            agree: &["sem problema", "sem problemas", "não tem problema", "não há problema", "por que não", "não se preocupe"],
        },
        wake_fillers: &["ei", "oi", "olá", "ok", "okay", "então", "é", "bem", "olha", "ah", "eh", "hum"],
        phrases: Phrases {
            questions: [
                "Posso executar um comando?",
                "Posso alterar um arquivo?",
                "Posso escrever um arquivo?",
                "Posso apagar um arquivo?",
                "Posso mover um arquivo?",
                "Posso ler um arquivo?",
                "Posso pesquisar na internet?",
                "Posso abrir uma página da web?",
                "Posso procurar nos seus arquivos?",
                "Posso pensar sobre isso?",
                "Posso usar {}?",
            ],
            yes_always_no: "Sim, sempre, ou não?",
            yes_no: "Sim ou não?",
            use_a_tool: "Posso usar uma ferramenta?",
            sorry: "Desculpe.",
            sample: "Olá. Esta é a minha voz.",
            code_on_screen: "O código está na tela.",
            command_on_screen: "(veja a tela)",
        },
    },
    Language {
        code: "nl",
        name: "Dutch",
        native: "Nederlands",
        voices: &[
            ("BE", "nl_BE-nathalie-medium"),
            ("", "nl_NL-pim-medium"),
        ],
        answers: AnswerWords {
            always: &[
                "niet meer vragen", "vraag niet meer", "vraag het niet meer", "niet opnieuw vragen", "elke keer", "voortaan",
                "vanaf nu", "altijd",
            ],
            deny: &["nee", "neen", "niet", "nooit", "stop", "stoppen", "annuleer", "annuleren", "weiger", "geen"],
            hedges: &[
                "weet niet", "weet het niet", "geen idee", "misschien", "wellicht", "niet zeker", "ik denk", "denk ik",
                "wacht", "momentje", "hangt ervan af", "hmm",
            ],
            question_starts: &["wat", "waarom", "hoe", "wie", "waar", "wanneer", "welke", "welk", "kun", "zou", "moet"],
            consent: &[
                "ja", "jawel", "jep", "oké", "ok", "okay", "prima", "goed", "akkoord", "zeker", "natuurlijk", "tuurlijk",
                "graag", "ga", "gang", "doe", "doen", "toegestaan", "best", "absoluut", "uiteraard", "top", "klopt",
            ],
            filler: &[
                "en", "dan", "gewoon", "maar", "het", "dat", "dit", "je", "jij", "ik", "u", "dank", "bedankt", "wel", "hoor",
                "hè", "eh", "uh", "nou", "oh", "deze", "keer", "nu", "een", "de", "even", "alsjeblieft", "alstublieft", "mag",
                "is",
            ],
            agree: &["geen probleem", "geen zorgen", "waarom niet", "geen bezwaar"],
        },
        wake_fillers: &["hé", "hey", "hoi", "hallo", "oké", "ok", "nou", "eh", "uh", "zeg", "oh", "ja"],
        phrases: Phrases {
            questions: [
                "Mag ik een opdracht uitvoeren?",
                "Mag ik een bestand wijzigen?",
                "Mag ik een bestand schrijven?",
                "Mag ik een bestand verwijderen?",
                "Mag ik een bestand verplaatsen?",
                "Mag ik een bestand lezen?",
                "Mag ik op internet zoeken?",
                "Mag ik een webpagina openen?",
                "Mag ik in je bestanden zoeken?",
                "Mag ik erover nadenken?",
                "Mag ik {} gebruiken?",
            ],
            yes_always_no: "Ja, altijd, of nee?",
            yes_no: "Ja of nee?",
            use_a_tool: "Mag ik een hulpmiddel gebruiken?",
            sorry: "Sorry.",
            sample: "Hallo. Zo klink ik.",
            code_on_screen: "De code staat op het scherm.",
            command_on_screen: "(zie het scherm)",
        },
    },
    Language {
        code: "pl",
        name: "Polish",
        native: "Polski",
        voices: &[("", "pl_PL-darkman-medium")],
        answers: AnswerWords {
            always: &[
                "nie pytaj więcej", "nie pytaj mnie więcej", "nie pytaj ponownie", "nie pytaj już", "za każdym razem",
                "od teraz", "zawsze",
            ],
            deny: &["nie", "nigdy", "stop", "przestań", "anuluj", "odmawiam", "zabraniam"],
            hedges: &[
                "nie wiem", "może", "chyba", "nie jestem pewien", "nie jestem pewna", "myślę", "zaraz", "czekaj", "poczekaj",
                "moment", "zależy", "hmm",
            ],
            question_starts: &["co", "dlaczego", "czemu", "jak", "kto", "gdzie", "kiedy", "który", "która", "które", "czy"],
            consent: &[
                "tak", "jasne", "okej", "ok", "okay", "dobrze", "dobra", "zgoda", "pewnie", "oczywiście", "możesz", "śmiało",
                "dawaj", "działaj", "zezwalam", "zgadzam", "pozwalam", "zatwierdzam",
            ],
            filler: &[
                "no",
                "i", "to", "tylko", "po", "prostu", "ten", "ta", "tym", "razem", "teraz", "się", "dzięki", "dziękuję", "a",
                "eee", "yyy", "mi", "mnie", "sobie", "raz", "proszę",
            ],
            agree: &["nie ma problemu", "bez problemu", "czemu nie", "nie ma sprawy", "nie szkodzi"],
        },
        wake_fillers: &["hej", "cześć", "halo", "ok", "okej", "no", "eee", "yyy", "słuchaj", "dobra", "to"],
        phrases: Phrases {
            questions: [
                "Czy mogę uruchomić polecenie?",
                "Czy mogę zmienić plik?",
                "Czy mogę zapisać plik?",
                "Czy mogę usunąć plik?",
                "Czy mogę przenieść plik?",
                "Czy mogę przeczytać plik?",
                "Czy mogę poszukać w internecie?",
                "Czy mogę otworzyć stronę internetową?",
                "Czy mogę przeszukać twoje pliki?",
                "Czy mogę to przemyśleć?",
                "Czy mogę użyć {}?",
            ],
            yes_always_no: "Tak, zawsze, czy nie?",
            yes_no: "Tak czy nie?",
            use_a_tool: "Czy mogę użyć narzędzia?",
            sorry: "Przepraszam.",
            sample: "Cześć. Tak brzmi mój głos.",
            code_on_screen: "Kod jest na ekranie.",
            command_on_screen: "(zobacz ekran)",
        },
    },
    Language {
        code: "sv",
        name: "Swedish",
        native: "Svenska",
        voices: &[("", "sv_SE-nst-medium")],
        answers: AnswerWords {
            always: &["fråga inte igen", "fråga inte mer", "varje gång", "från och med nu", "hädanefter", "alltid"],
            deny: &["nej", "nä", "inte", "aldrig", "stopp", "sluta", "avbryt", "neka"],
            hedges: &[
                "vet inte", "kanske", "inte säker", "jag tror", "tror jag", "vänta", "ett ögonblick", "beror på", "hmm",
            ],
            question_starts: &["vad", "varför", "hur", "vem", "var", "när", "vilken", "vilket", "vilka", "skulle", "borde"],
            consent: &[
                "ja", "japp", "jo", "okej", "ok", "okay", "visst", "absolut", "självklart", "gärna", "kör", "varsågod", "bra",
                "fint", "godkänt", "tillåt", "tillåtet", "säkert", "javisst", "perfekt",
            ],
            filler: &[
                "och", "sen", "bara", "det", "den", "detta", "du", "jag", "tack", "så", "mycket", "eh", "öh", "åh", "oh",
                "nu", "en", "ett", "gång", "gör", "får",
            ],
            agree: &["inga problem", "inget problem", "varför inte", "ingen fara"],
        },
        wake_fillers: &["hej", "hallå", "okej", "ok", "eh", "öh", "så", "du", "ja"],
        phrases: Phrases {
            questions: [
                "Får jag köra ett kommando?",
                "Får jag ändra en fil?",
                "Får jag skriva en fil?",
                "Får jag ta bort en fil?",
                "Får jag flytta en fil?",
                "Får jag läsa en fil?",
                "Får jag söka på webben?",
                "Får jag öppna en webbsida?",
                "Får jag söka i dina filer?",
                "Får jag tänka på det?",
                "Får jag använda {}?",
            ],
            yes_always_no: "Ja, alltid, eller nej?",
            yes_no: "Ja eller nej?",
            use_a_tool: "Får jag använda ett verktyg?",
            sorry: "Förlåt.",
            sample: "Hej. Så här låter jag.",
            code_on_screen: "Koden visas på skärmen.",
            command_on_screen: "(se skärmen)",
        },
    },
    Language {
        code: "da",
        name: "Danish",
        native: "Dansk",
        voices: &[("", "da_DK-talesyntese-medium")],
        answers: AnswerWords {
            always: &["spørg ikke igen", "spørg ikke mere", "hver gang", "fra nu af", "fremover", "altid"],
            deny: &["nej", "ikke", "aldrig", "stop", "stands", "annuller", "afvis"],
            hedges: &["ved ikke", "måske", "ikke sikker", "jeg tror", "tror jeg", "vent", "øjeblik", "kommer an på", "hmm"],
            question_starts: &["hvad", "hvorfor", "hvordan", "hvem", "hvor", "hvornår", "hvilken", "hvilket", "hvilke", "kunne", "burde"],
            consent: &[
                "ja", "jo", "jep", "okay", "ok", "fint", "godt", "selvfølgelig", "sikkert", "klart", "gerne", "kør", "værsgo",
                "tilladt", "tillad", "absolut", "perfekt", "javel",
            ],
            filler: &[
                "og", "så", "bare", "det", "den", "dette", "du", "jeg", "tak", "mange", "øh", "øhm", "åh", "oh", "nu", "en",
                "et", "gang", "gør", "må",
            ],
            agree: &["intet problem", "ingen problemer", "hvorfor ikke", "ingen fare"],
        },
        wake_fillers: &["hej", "hallo", "okay", "ok", "øh", "øhm", "så", "du", "ja"],
        phrases: Phrases {
            questions: [
                "Må jeg køre en kommando?",
                "Må jeg ændre en fil?",
                "Må jeg skrive en fil?",
                "Må jeg slette en fil?",
                "Må jeg flytte en fil?",
                "Må jeg læse en fil?",
                "Må jeg søge på nettet?",
                "Må jeg åbne en webside?",
                "Må jeg søge i dine filer?",
                "Må jeg tænke over det?",
                "Må jeg bruge {}?",
            ],
            yes_always_no: "Ja, altid, eller nej?",
            yes_no: "Ja eller nej?",
            use_a_tool: "Må jeg bruge et værktøj?",
            sorry: "Undskyld.",
            sample: "Hej. Sådan lyder jeg.",
            code_on_screen: "Koden står på skærmen.",
            command_on_screen: "(se skærmen)",
        },
    },
    Language {
        code: "no",
        name: "Norwegian",
        native: "Norsk",
        voices: &[("", "no_NO-talesyntese-medium")],
        answers: AnswerWords {
            always: &["ikke spør igjen", "ikke spør mer", "spør ikke igjen", "hver gang", "fra nå av", "heretter", "alltid"],
            deny: &["nei", "ikke", "aldri", "stopp", "slutt", "avbryt", "avslå"],
            hedges: &["vet ikke", "kanskje", "ikke sikker", "jeg tror", "tror jeg", "vent", "øyeblikk", "kommer an på", "hmm"],
            question_starts: &["hva", "hvorfor", "hvordan", "hvem", "hvor", "når", "hvilken", "hvilket", "hvilke", "kunne", "burde"],
            consent: &[
                "ja", "jo", "jepp", "okei", "ok", "okay", "greit", "fint", "bra", "selvfølgelig", "sikkert", "klart", "gjerne",
                "kjør", "værsågod", "tillatt", "tillat", "absolutt", "perfekt", "javisst",
            ],
            filler: &[
                "og", "så", "bare", "det", "den", "dette", "du", "jeg", "takk", "mange", "eh", "øh", "å", "oh", "nå", "en",
                "et", "ei", "gang", "gjør",
            ],
            agree: &["ikke noe problem", "null problem", "hvorfor ikke", "ingen fare"],
        },
        wake_fillers: &["hei", "hallo", "okei", "ok", "eh", "øh", "så", "du", "ja"],
        phrases: Phrases {
            questions: [
                "Kan jeg kjøre en kommando?",
                "Kan jeg endre en fil?",
                "Kan jeg skrive en fil?",
                "Kan jeg slette en fil?",
                "Kan jeg flytte en fil?",
                "Kan jeg lese en fil?",
                "Kan jeg søke på nettet?",
                "Kan jeg åpne en nettside?",
                "Kan jeg søke i filene dine?",
                "Kan jeg tenke over det?",
                "Kan jeg bruke {}?",
            ],
            yes_always_no: "Ja, alltid, eller nei?",
            yes_no: "Ja eller nei?",
            use_a_tool: "Kan jeg bruke et verktøy?",
            sorry: "Unnskyld.",
            sample: "Hei. Slik høres jeg ut.",
            code_on_screen: "Koden står på skjermen.",
            command_on_screen: "(se skjermen)",
        },
    },
    Language {
        code: "fi",
        name: "Finnish",
        native: "Suomi",
        voices: &[("", "fi_FI-harri-medium")],
        answers: AnswerWords {
            always: &["älä kysy enää", "älä kysy uudestaan", "joka kerta", "tästä lähtien", "jatkossa", "aina"],
            deny: &["ei", "älä", "en", "koskaan", "stop", "seis", "lopeta", "peruuta", "kiellä"],
            hedges: &[
                "en tiedä", "ehkä", "kenties", "en ole varma", "luulen", "luulisin", "odota", "hetki", "riippuu", "hmm",
            ],
            question_starts: &["mitä", "mikä", "miksi", "miten", "kuka", "missä", "milloin", "kumpi", "voitko", "voisitko", "onko"],
            consent: &[
                "kyllä", "joo", "juu", "jep", "okei", "ok", "okay", "selvä", "hyvä", "toki", "tietysti", "saa", "saat", "anna",
                "mene", "mennään", "sopii", "sallittu", "salli", "ehdottomasti", "varmasti", "käy",
            ],
            filler: &[
                "no",
                "ja", "sitten", "vain", "vaan", "se", "sen", "tämä", "sinä", "minä", "kiitos", "öö", "äh", "oh", "nyt",
                "kerran", "tee", "tehdä", "vaan",
            ],
            agree: &["ei hätää", "ei ongelmaa", "ei se mitään", "mikä ettei", "miksi ei"],
        },
        wake_fillers: &["hei", "moi", "terve", "okei", "ok", "öö", "no", "kuule"],
        phrases: Phrases {
            questions: [
                "Saanko suorittaa komennon?",
                "Saanko muokata tiedostoa?",
                "Saanko kirjoittaa tiedoston?",
                "Saanko poistaa tiedoston?",
                "Saanko siirtää tiedoston?",
                "Saanko lukea tiedoston?",
                "Saanko hakea verkosta?",
                "Saanko avata verkkosivun?",
                "Saanko hakea tiedostoistasi?",
                "Saanko miettiä sitä?",
                "Saanko käyttää työkalua {}?",
            ],
            yes_always_no: "Kyllä, aina, vai ei?",
            yes_no: "Kyllä vai ei?",
            use_a_tool: "Saanko käyttää työkalua?",
            sorry: "Anteeksi.",
            sample: "Hei. Tältä minä kuulostan.",
            code_on_screen: "Koodi näkyy näytöllä.",
            command_on_screen: "(katso näyttöä)",
        },
    },
    Language {
        code: "cs",
        name: "Czech",
        native: "Čeština",
        voices: &[("", "cs_CZ-jirka-medium")],
        answers: AnswerWords {
            always: &["už se neptej", "neptej se znovu", "neptej se už", "pokaždé", "od teď", "odteď", "vždy", "vždycky"],
            deny: &["ne", "nikdy", "nedělej", "stop", "zastav", "zruš", "zrušit", "odmítám", "zakazuji", "nesmíš"],
            hedges: &[
                "nevím", "možná", "asi", "nejsem si jistý", "nejsem si jistá", "myslím", "počkej", "moment", "záleží", "hmm",
            ],
            question_starts: &["co", "proč", "jak", "kdo", "kde", "kdy", "který", "která", "které", "mohl", "mohla"],
            consent: &[
                "ano", "jo", "jasně", "jasne", "ok", "okej", "okay", "dobře", "dobre", "souhlas", "souhlasím", "klidně",
                "můžeš", "určitě", "samozřejmě", "povoluji", "povoleno", "pokračuj", "pořádku",
            ],
            filler: &[
                "no",
                "a", "tak", "jen", "jenom", "to", "tohle", "ten", "ty", "já", "díky", "děkuju", "děkuji", "ehm", "eh", "teď",
                "jednou", "si", "mi", "toho", "do", "v", "prosím",
            ],
            agree: &["žádný problém", "bez problému", "proč ne", "nevadí", "v pohodě"],
        },
        wake_fillers: &["hej", "ahoj", "ok", "okej", "no", "ehm", "tak", "hele", "poslyš"],
        phrases: Phrases {
            questions: [
                "Můžu spustit příkaz?",
                "Můžu upravit soubor?",
                "Můžu zapsat soubor?",
                "Můžu smazat soubor?",
                "Můžu přesunout soubor?",
                "Můžu přečíst soubor?",
                "Můžu hledat na internetu?",
                "Můžu otevřít webovou stránku?",
                "Můžu prohledat tvoje soubory?",
                "Můžu si to promyslet?",
                "Můžu použít {}?",
            ],
            yes_always_no: "Ano, vždy, nebo ne?",
            yes_no: "Ano, nebo ne?",
            use_a_tool: "Můžu použít nástroj?",
            sorry: "Promiň.",
            sample: "Ahoj. Takhle zním.",
            code_on_screen: "Kód je na obrazovce.",
            command_on_screen: "(viz obrazovka)",
        },
    },
    Language {
        code: "ru",
        name: "Russian",
        native: "Русский",
        voices: &[("", "ru_RU-dmitri-medium")],
        answers: AnswerWords {
            always: &[
                "больше не спрашивай", "не спрашивай больше", "не спрашивай снова", "каждый раз", "с этого момента",
                "отныне", "впредь", "всегда",
            ],
            deny: &["нет", "не", "никогда", "стоп", "хватит", "отмена", "отмени", "запрещаю", "нельзя", "отказ"],
            hedges: &[
                "не знаю", "может быть", "возможно", "наверное", "не уверен", "не уверена", "думаю", "кажется", "подожди",
                "секунду", "минутку", "зависит", "хм",
            ],
            question_starts: &["что", "почему", "зачем", "как", "кто", "где", "когда", "какой", "какая", "какое", "какие", "можешь"],
            consent: &[
                "да", "ага", "угу", "конечно", "ладно", "хорошо", "окей", "ок", "ok", "давай", "давайте", "можно", "разрешаю",
                "разрешено", "валяй", "согласен", "согласна", "верно", "точно", "естественно", "безусловно", "вперёд",
                "вперед", "норм",
            ],
            filler: &[
                "и", "тогда", "просто", "только", "это", "этот", "эту", "ты", "я", "спасибо", "ну", "э", "эм", "ой", "ах",
                "сейчас", "теперь", "раз", "один", "делай", "сделай", "вот", "пожалуйста",
            ],
            agree: &["без проблем", "не проблема", "почему бы и нет", "почему нет", "не вопрос"],
        },
        wake_fillers: &["эй", "привет", "слушай", "ну", "ок", "окей", "э", "эм", "так", "алло"],
        phrases: Phrases {
            questions: [
                "Можно выполнить команду?",
                "Можно изменить файл?",
                "Можно записать файл?",
                "Можно удалить файл?",
                "Можно переместить файл?",
                "Можно прочитать файл?",
                "Можно поискать в интернете?",
                "Можно открыть веб-страницу?",
                "Можно поискать в твоих файлах?",
                "Можно подумать над этим?",
                "Можно использовать {}?",
            ],
            yes_always_no: "Да, всегда, или нет?",
            yes_no: "Да или нет?",
            use_a_tool: "Можно использовать инструмент?",
            sorry: "Извини.",
            sample: "Привет. Вот так звучит мой голос.",
            code_on_screen: "Код на экране.",
            command_on_screen: "(смотри экран)",
        },
    },
    Language {
        code: "uk",
        name: "Ukrainian",
        native: "Українська",
        voices: &[("", "uk_UA-tetiana-high")],
        answers: AnswerWords {
            always: &[
                "більше не питай", "не питай більше", "не питай знову", "кожного разу", "щоразу", "відтепер", "надалі",
                "завжди",
            ],
            deny: &["ні", "не", "ніколи", "стоп", "досить", "скасуй", "скасувати", "забороняю", "відмова"],
            hedges: &[
                "не знаю", "може", "можливо", "мабуть", "напевно", "не впевнений", "не впевнена", "думаю", "здається",
                "зачекай", "секунду", "хвилинку", "залежить", "хм",
            ],
            question_starts: &["що", "чому", "навіщо", "як", "хто", "де", "коли", "який", "яка", "яке", "які", "можеш", "чи"],
            consent: &[
                "так", "ага", "угу", "звісно", "звичайно", "гаразд", "добре", "окей", "ок", "давай", "можна", "дозволяю",
                "дозволено", "згоден", "згодна", "правильно", "точно", "вперед", "авжеж",
            ],
            filler: &[
                "і", "й", "тоді", "просто", "тільки", "це", "цей", "цю", "ти", "я", "дякую", "ну", "е", "ем", "ой", "ах",
                "зараз", "тепер", "раз", "один", "роби", "зроби", "от", "ось", "будь", "ласка",
            ],
            agree: &["без проблем", "не проблема", "чому б ні", "чому ні", "не питання"],
        },
        wake_fillers: &["гей", "привіт", "слухай", "ну", "ок", "окей", "е", "ем", "так", "алло"],
        phrases: Phrases {
            questions: [
                "Можна виконати команду?",
                "Можна змінити файл?",
                "Можна записати файл?",
                "Можна видалити файл?",
                "Можна перемістити файл?",
                "Можна прочитати файл?",
                "Можна пошукати в інтернеті?",
                "Можна відкрити вебсторінку?",
                "Можна пошукати у твоїх файлах?",
                "Можна подумати над цим?",
                "Можна використати {}?",
            ],
            yes_always_no: "Так, завжди, чи ні?",
            yes_no: "Так чи ні?",
            use_a_tool: "Можна використати інструмент?",
            sorry: "Вибач.",
            sample: "Привіт. Ось так звучить мій голос.",
            code_on_screen: "Код на екрані.",
            command_on_screen: "(дивись екран)",
        },
    },
    Language {
        code: "tr",
        name: "Turkish",
        native: "Türkçe",
        voices: &[("", "tr_TR-dfki-medium")],
        answers: AnswerWords {
            always: &["bir daha sorma", "artık sorma", "tekrar sorma", "her seferinde", "bundan sonra", "her zaman", "hep"],
            deny: &["hayır", "yok", "olmaz", "asla", "dur", "durdur", "iptal", "istemiyorum", "yapma", "reddet"],
            hedges: &[
                "bilmiyorum", "belki", "emin değilim", "sanırım", "galiba", "bekle", "bir saniye", "bir dakika",
                "duruma bağlı", "hmm",
            ],
            question_starts: &["ne", "neden", "niye", "nasıl", "kim", "nerede", "hangi"],
            consent: &[
                "evet", "tamam", "olur", "tabii", "tabi", "elbette", "peki", "ok", "okey", "okay", "kesinlikle", "yap", "devam",
                "izin", "veriyorum", "onaylıyorum", "onay", "aynen", "oldu",
            ],
            filler: &[
                "ve", "sonra", "sadece", "bu", "şu", "o", "sen", "ben", "teşekkürler", "teşekkür", "ederim", "sağ", "ol",
                "eee", "ıı", "şey", "hı", "şimdi", "bir", "kere", "kez", "lütfen", "bunu",
            ],
            agree: &["sorun değil", "sorun yok", "problem yok", "neden olmasın", "dert değil"],
        },
        wake_fillers: &["hey", "merhaba", "selam", "tamam", "eee", "ıı", "şey", "bak", "baksana", "hı"],
        phrases: Phrases {
            questions: [
                "Bir komut çalıştırabilir miyim?",
                "Bir dosyayı değiştirebilir miyim?",
                "Bir dosya yazabilir miyim?",
                "Bir dosyayı silebilir miyim?",
                "Bir dosyayı taşıyabilir miyim?",
                "Bir dosyayı okuyabilir miyim?",
                "İnternette arama yapabilir miyim?",
                "Bir web sayfası açabilir miyim?",
                "Dosyalarında arama yapabilir miyim?",
                "Bunu düşünebilir miyim?",
                "{} kullanabilir miyim?",
            ],
            yes_always_no: "Evet, her zaman, yoksa hayır mı?",
            yes_no: "Evet mi, hayır mı?",
            use_a_tool: "Bir araç kullanabilir miyim?",
            sorry: "Özür dilerim.",
            sample: "Merhaba. Sesim böyle.",
            code_on_screen: "Kod ekranda.",
            command_on_screen: "(ekrana bak)",
        },
    },
    Language {
        code: "hu",
        name: "Hungarian",
        native: "Magyar",
        voices: &[("", "hu_HU-anna-medium")],
        answers: AnswerWords {
            always: &["ne kérdezd többet", "ne kérdezz többet", "ne kérdezd újra", "minden alkalommal", "mostantól", "ezentúl", "mindig"],
            deny: &["nem", "ne", "soha", "sose", "állj", "stop", "mégse", "tilos"],
            hedges: &[
                "nem tudom", "talán", "lehet", "nem vagyok biztos", "szerintem", "azt hiszem", "várj", "egy pillanat",
                "attól függ", "hmm",
            ],
            question_starts: &["mi", "mit", "miért", "hogyan", "ki", "hol", "mikor", "melyik", "tudnád"],
            consent: &[
                "igen", "jó", "oké", "ok", "okay", "persze", "rendben", "mehet", "nyugodtan", "csináld", "engedélyezem",
                "szabad", "természetesen", "hogyne", "abszolút", "biztos", "biztosan", "naná", "aha",
            ],
            filler: &[
                "és", "aztán", "csak", "ez", "azt", "ezt", "te", "én", "köszönöm", "köszi", "hát", "öö", "ö", "ó", "most",
                "egyszer", "kérlek", "légyszi", "meg",
            ],
            agree: &["semmi gond", "nem probléma", "nincs probléma", "miért ne", "semmi baj"],
        },
        wake_fillers: &["hé", "szia", "helló", "halló", "oké", "ok", "öö", "hát", "figyelj", "na"],
        phrases: Phrases {
            questions: [
                "Futtathatok egy parancsot?",
                "Módosíthatok egy fájlt?",
                "Írhatok egy fájlt?",
                "Törölhetek egy fájlt?",
                "Áthelyezhetek egy fájlt?",
                "Elolvashatok egy fájlt?",
                "Kereshetek az interneten?",
                "Megnyithatok egy weboldalt?",
                "Kereshetek a fájljaid között?",
                "Átgondolhatom?",
                "Használhatom ezt: {}?",
            ],
            yes_always_no: "Igen, mindig, vagy nem?",
            yes_no: "Igen vagy nem?",
            use_a_tool: "Használhatok egy eszközt?",
            sorry: "Bocsánat.",
            sample: "Szia. Így szól a hangom.",
            code_on_screen: "A kód a képernyőn van.",
            command_on_screen: "(lásd a képernyőt)",
        },
    },
    Language {
        code: "ro",
        name: "Romanian",
        native: "Română",
        voices: &[("", "ro_RO-mihai-medium")],
        answers: AnswerWords {
            always: &[
                "nu mai întreba", "nu mă mai întreba", "de fiecare dată", "de acum înainte", "de acum", "întotdeauna", "mereu",
            ],
            deny: &["nu", "niciodată", "stop", "oprește", "anulează", "refuz", "interzis", "deloc"],
            hedges: &[
                "nu știu", "poate", "probabil", "nu sunt sigur", "nu sunt sigură", "cred", "așteaptă", "stai", "un moment",
                "depinde", "hmm",
            ],
            question_starts: &["ce", "cum", "cine", "unde", "când", "care", "ești"],
            consent: &[
                "da", "ok", "okay", "bine", "sigur", "desigur", "firește", "normal", "poți", "continuă", "fă", "aprob",
                "permis", "permit", "perfect", "exact", "hai", "absolut", "bineînțeles", "corect",
            ],
            filler: &[
                "și", "atunci", "doar", "asta", "acesta", "aceasta", "tu", "eu", "mersi", "mulțumesc", "păi", "ă", "ăă", "eh",
                "acum", "o", "dată", "te", "rog", "să", "faci",
            ],
            agree: &["nicio problemă", "nici o problemă", "fără probleme", "de ce nu", "nu i problemă"],
        },
        wake_fillers: &["hei", "salut", "bună", "ok", "păi", "ăă", "ă", "uite", "ascultă", "alo"],
        phrases: Phrases {
            questions: [
                "Pot să rulez o comandă?",
                "Pot să modific un fișier?",
                "Pot să scriu un fișier?",
                "Pot să șterg un fișier?",
                "Pot să mut un fișier?",
                "Pot să citesc un fișier?",
                "Pot să caut pe internet?",
                "Pot să deschid o pagină web?",
                "Pot să caut în fișierele tale?",
                "Pot să mă gândesc la asta?",
                "Pot să folosesc {}?",
            ],
            yes_always_no: "Da, întotdeauna, sau nu?",
            yes_no: "Da sau nu?",
            use_a_tool: "Pot să folosesc un instrument?",
            sorry: "Scuze.",
            sample: "Bună. Așa sună vocea mea.",
            code_on_screen: "Codul este pe ecran.",
            command_on_screen: "(vezi ecranul)",
        },
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locales() {
        assert_eq!(parse_locale("de_DE.UTF-8@euro"), ("de".into(), "DE".into()));
        assert_eq!(parse_locale("pt-BR"), ("pt".into(), "BR".into()));
        assert_eq!(parse_locale("C.UTF-8"), (String::new(), String::new()));
        let r = resolve_with("", "de_DE.UTF-8");
        assert_eq!((r.whisper.as_str(), r.code, r.region.as_str()), ("de", "de", "DE"));
        // No locale, or one not in the list: English.
        assert_eq!(resolve_with("", "").code, "en");
        assert_eq!(resolve_with("", "ja_JP.UTF-8").code, "en");
        // Set explicitly: that, with the locale's region only if it's the same language.
        let r = resolve_with("pt", "pt_PT.UTF-8");
        assert_eq!(r.language().voice(&r.region), "pt_PT-tugão-medium");
        let r = resolve_with("pt", "en_GB.UTF-8");
        assert_eq!(r.language().voice(&r.region), "pt_BR-faber-medium");
        assert_eq!(resolve_with("", "nb_NO.UTF-8").code, "no");
        // Something whisper knows but we don't: whisper gets it, the rest is English.
        let r = resolve_with("ja", "de_DE.UTF-8");
        assert_eq!((r.whisper.as_str(), r.code), ("ja", "en"));
        assert_eq!(resolve_with("auto", "").whisper, "auto");
        assert_eq!(resolve_with(" DE ", "").code, "de");
    }

    #[test]
    fn regional_voices() {
        let es = find("es").unwrap();
        assert_eq!(es.voice("ES"), "es_ES-davefx-medium");
        assert_eq!(es.voice("MX"), "es_MX-ald-medium");
        assert_eq!(es.voice(""), "es_MX-ald-medium");
        assert_eq!(english().voice("US"), "en_GB-alan-medium");
    }

    #[test]
    fn tables_are_complete() {
        let mut codes = std::collections::HashSet::new();
        for l in &LANGUAGES {
            assert!(codes.insert(l.code), "{} twice", l.code);
            assert!(l.voices.iter().any(|(r, _)| r.is_empty()), "{} has no default voice", l.code);
            for (_, v) in l.voices {
                let info = crate::voices::find(v).unwrap_or_else(|| panic!("{v} isn't in voices::VOICES"));
                assert_eq!(info.lang, l.code, "{v}");
            }
            assert!(crate::voices::for_language(l.code).count() >= 1);
            assert_eq!(l.question(AskKind::Tool, "Gmail").matches("Gmail").count(), 1, "{}", l.code);
            assert_eq!(l.question(AskKind::Tool, "Tool"), l.phrases.use_a_tool);
            for list in [l.answers.consent, l.answers.deny, l.answers.always, l.answers.filler, l.wake_fillers] {
                assert!(list.iter().all(|w| *w == w.to_lowercase() && !w.trim().is_empty()), "{}: {list:?}", l.code);
            }
            // A yes word that's also a no word would make "yes" deny.
            for w in l.answers.consent {
                assert!(!l.answers.deny.contains(w), "{}: {w} is both yes and no", l.code);
            }
            assert!(l.instruction().is_some() != l.is_english());
        }
    }
}
