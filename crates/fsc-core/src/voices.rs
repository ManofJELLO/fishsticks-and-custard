//! The Piper voices offered for each language: single-speaker voices of
//! medium or high quality from huggingface.co/rhasspy/piper-voices
//! (voices.json). Generated; `packaging/release/check.py` checks that each
//! is still there. Any other voice works too, named in `voice.piper_voice`.

pub struct VoiceInfo {
    /// Language code, as in `lang::Language::code`.
    pub lang: &'static str,
    /// The file name without extension: "en_GB-alan-medium".
    pub key: &'static str,
    /// For the menu: "Alan (GB)".
    pub label: &'static str,
    /// Download size of the model, MB.
    pub mb: u32,
}

pub fn for_language(code: &str) -> impl Iterator<Item = &'static VoiceInfo> + '_ {
    VOICES.iter().filter(move |v| v.lang == code)
}

pub fn find(key: &str) -> Option<&'static VoiceInfo> {
    VOICES.iter().find(|v| v.key == key)
}

pub static VOICES: &[VoiceInfo] = &[
    VoiceInfo { lang: "cs", key: "cs_CZ-jirka-medium", label: "Jirka", mb: 63 },
    VoiceInfo { lang: "cs", key: "cs_CZ-kasandra-medium", label: "Kasandra", mb: 64 },
    VoiceInfo { lang: "da", key: "da_DK-talesyntese-medium", label: "Talesyntese", mb: 63 },
    VoiceInfo { lang: "de", key: "de_DE-kerstin-medium", label: "Kerstin", mb: 63 },
    VoiceInfo { lang: "de", key: "de_DE-thorsten-medium", label: "Thorsten", mb: 63 },
    VoiceInfo { lang: "de", key: "de_DE-thorsten-high", label: "Thorsten (high quality)", mb: 114 },
    VoiceInfo { lang: "en", key: "en_GB-alan-medium", label: "Alan (GB)", mb: 63 },
    VoiceInfo { lang: "en", key: "en_GB-alba-medium", label: "Alba (GB)", mb: 63 },
    VoiceInfo { lang: "en", key: "en_US-amy-medium", label: "Amy (US)", mb: 63 },
    VoiceInfo { lang: "en", key: "en_US-bryce-medium", label: "Bryce (US)", mb: 64 },
    VoiceInfo { lang: "en", key: "en_GB-cori-medium", label: "Cori (GB)", mb: 63 },
    VoiceInfo { lang: "en", key: "en_GB-cori-high", label: "Cori (GB, high quality)", mb: 114 },
    VoiceInfo { lang: "en", key: "en_US-hfc_female-medium", label: "HFC female (US)", mb: 63 },
    VoiceInfo { lang: "en", key: "en_US-hfc_male-medium", label: "HFC male (US)", mb: 63 },
    VoiceInfo { lang: "en", key: "en_US-hifi_tts_92-medium", label: "HiFi TTS 92 (US)", mb: 63 },
    VoiceInfo { lang: "en", key: "en_GB-jenny_dioco-medium", label: "Jenny Dioco (GB)", mb: 63 },
    VoiceInfo { lang: "en", key: "en_US-joe-medium", label: "Joe (US)", mb: 63 },
    VoiceInfo { lang: "en", key: "en_US-john-medium", label: "John (US)", mb: 64 },
    VoiceInfo { lang: "en", key: "en_US-kathleen-medium", label: "Kathleen (US)", mb: 63 },
    VoiceInfo { lang: "en", key: "en_US-kristin-medium", label: "Kristin (US)", mb: 64 },
    VoiceInfo { lang: "en", key: "en_US-kusal-medium", label: "Kusal (US)", mb: 63 },
    VoiceInfo { lang: "en", key: "en_US-lessac-medium", label: "Lessac (US)", mb: 63 },
    VoiceInfo { lang: "en", key: "en_US-lessac-high", label: "Lessac (US, high quality)", mb: 114 },
    VoiceInfo { lang: "en", key: "en_US-ljspeech-medium", label: "LJ Speech (US)", mb: 64 },
    VoiceInfo { lang: "en", key: "en_US-ljspeech-high", label: "LJ Speech (US, high quality)", mb: 114 },
    VoiceInfo { lang: "en", key: "en_US-mike-medium", label: "Mike (US)", mb: 63 },
    VoiceInfo { lang: "en", key: "en_US-norman-medium", label: "Norman (US)", mb: 64 },
    VoiceInfo { lang: "en", key: "en_GB-northern_english_male-medium", label: "Northern English Male (GB)", mb: 63 },
    VoiceInfo { lang: "en", key: "en_US-reza_ibrahim-medium", label: "Reza Ibrahim (US)", mb: 64 },
    VoiceInfo { lang: "en", key: "en_US-ryan-medium", label: "Ryan (US)", mb: 63 },
    VoiceInfo { lang: "en", key: "en_US-ryan-high", label: "Ryan (US, high quality)", mb: 121 },
    VoiceInfo { lang: "en", key: "en_US-sam-medium", label: "Sam (US)", mb: 63 },
    VoiceInfo { lang: "es", key: "es_MX-ald-medium", label: "Ald (MX)", mb: 63 },
    VoiceInfo { lang: "es", key: "es_MX-claude-high", label: "Claude (MX, high quality)", mb: 63 },
    VoiceInfo { lang: "es", key: "es_AR-daniela-high", label: "Daniela (AR, high quality)", mb: 114 },
    VoiceInfo { lang: "es", key: "es_ES-davefx-medium", label: "Davefx (ES)", mb: 63 },
    VoiceInfo { lang: "fi", key: "fi_FI-harri-medium", label: "Harri", mb: 63 },
    VoiceInfo { lang: "fr", key: "fr_FR-siwis-medium", label: "Siwis", mb: 63 },
    VoiceInfo { lang: "fr", key: "fr_FR-tom-medium", label: "Tom", mb: 64 },
    VoiceInfo { lang: "hi", key: "hi_IN-pratham-medium", label: "Pratham", mb: 64 },
    VoiceInfo { lang: "hi", key: "hi_IN-priyamvada-medium", label: "Priyamvada", mb: 64 },
    VoiceInfo { lang: "hi", key: "hi_IN-rohan-medium", label: "Rohan", mb: 63 },
    VoiceInfo { lang: "hu", key: "hu_HU-anna-medium", label: "Anna", mb: 63 },
    VoiceInfo { lang: "hu", key: "hu_HU-berta-medium", label: "Berta", mb: 63 },
    VoiceInfo { lang: "hu", key: "hu_HU-imre-medium", label: "Imre", mb: 63 },
    VoiceInfo { lang: "id", key: "id_ID-news_tts-medium", label: "News TTS", mb: 63 },
    VoiceInfo { lang: "it", key: "it_IT-paola-medium", label: "Paola", mb: 63 },
    VoiceInfo { lang: "it", key: "it_IT-serena-medium", label: "Serena", mb: 64 },
    VoiceInfo { lang: "it", key: "it_IT-serena-high", label: "Serena (high quality)", mb: 114 },
    VoiceInfo { lang: "ko", key: "ko_KR-kss-medium", label: "KSS", mb: 63 },
    VoiceInfo { lang: "nl", key: "nl_NL-alex-medium", label: "Alex (NL)", mb: 63 },
    VoiceInfo { lang: "nl", key: "nl_BE-flemishguy-medium", label: "Flemishguy (BE)", mb: 63 },
    VoiceInfo { lang: "nl", key: "nl_BE-nathalie-medium", label: "Nathalie (BE)", mb: 63 },
    VoiceInfo { lang: "nl", key: "nl_NL-pim-medium", label: "Pim (NL)", mb: 63 },
    VoiceInfo { lang: "nl", key: "nl_BE-rdh-medium", label: "Rdh (BE)", mb: 63 },
    VoiceInfo { lang: "nl", key: "nl_NL-ronnie-medium", label: "Ronnie (NL)", mb: 63 },
    VoiceInfo { lang: "no", key: "no_NO-talesyntese-medium", label: "Talesyntese", mb: 63 },
    VoiceInfo { lang: "pl", key: "pl_PL-bass-high", label: "Bass (high quality)", mb: 114 },
    VoiceInfo { lang: "pl", key: "pl_PL-darkman-medium", label: "Darkman", mb: 63 },
    VoiceInfo { lang: "pl", key: "pl_PL-gosia-medium", label: "Gosia", mb: 63 },
    VoiceInfo { lang: "pl", key: "pl_PL-mc_speech-medium", label: "MC Speech", mb: 63 },
    VoiceInfo { lang: "pt", key: "pt_BR-cadu-medium", label: "Cadu (BR)", mb: 63 },
    VoiceInfo { lang: "pt", key: "pt_BR-faber-medium", label: "Faber (BR)", mb: 63 },
    VoiceInfo { lang: "pt", key: "pt_BR-jeff-medium", label: "Jeff (BR)", mb: 63 },
    VoiceInfo { lang: "pt", key: "pt_PT-tugão-medium", label: "Tugão (PT)", mb: 63 },
    VoiceInfo { lang: "ro", key: "ro_RO-mihai-medium", label: "Mihai", mb: 63 },
    VoiceInfo { lang: "ru", key: "ru_RU-denis-medium", label: "Denis", mb: 63 },
    VoiceInfo { lang: "ru", key: "ru_RU-dmitri-medium", label: "Dmitri", mb: 63 },
    VoiceInfo { lang: "ru", key: "ru_RU-irina-medium", label: "Irina", mb: 63 },
    VoiceInfo { lang: "ru", key: "ru_RU-ruslan-medium", label: "Ruslan", mb: 63 },
    VoiceInfo { lang: "sv", key: "sv_SE-alma-medium", label: "Alma", mb: 63 },
    VoiceInfo { lang: "sv", key: "sv_SE-lisa-medium", label: "Lisa", mb: 64 },
    VoiceInfo { lang: "sv", key: "sv_SE-nst-medium", label: "NST", mb: 63 },
    VoiceInfo { lang: "tr", key: "tr_TR-dfki-medium", label: "DFKI", mb: 63 },
    VoiceInfo { lang: "uk", key: "uk_UA-mykyta-high", label: "Mykyta (high quality)", mb: 114 },
    VoiceInfo { lang: "uk", key: "uk_UA-oleksa-high", label: "Oleksa (high quality)", mb: 114 },
    VoiceInfo { lang: "uk", key: "uk_UA-tetiana-high", label: "Tetiana (high quality)", mb: 114 },
    VoiceInfo { lang: "vi", key: "vi_VN-vais1000-medium", label: "VAIS1000", mb: 63 },
    VoiceInfo { lang: "zh", key: "zh_CN-huayan-medium", label: "Huayan", mb: 63 },
];
