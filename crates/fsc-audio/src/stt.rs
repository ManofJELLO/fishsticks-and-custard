//! Speech to text with whisper.cpp (Vulkan on AMD).

use anyhow::{Context, Result, anyhow};
use std::path::Path;
use std::sync::Mutex;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

/// Shared between the audio thread and wake-phrase transcriptions. One
/// whisper state is reused for every run, which also means runs take turns
/// (on the GPU they would anyway).
pub struct Stt {
    _ctx: WhisperContext,
    state: Mutex<WhisperState>,
    /// "en", "de", ... or "auto".
    language: Mutex<String>,
    wake_phrase: Mutex<String>,
}

/// The language the wake phrase is said in: English for the usual one,
/// else the language spoken (a phrase of your own is in your language).
/// Whisper set to one language leaves out speech in another, so the phrase
/// is listened for in its own.
pub fn wake_language(phrase: &str, language: &str) -> String {
    let usual = fsc_core::config::VoiceConfig::default().wake_phrase;
    if phrase.split_whitespace().collect::<Vec<_>>().join(" ").eq_ignore_ascii_case(&usual) {
        "en".into()
    } else {
        language.into()
    }
}

/// Biases whisper toward the wake phrase's spelling (and the desktop's
/// names). English only: in another language whisper treats a prompt as
/// already said, dropping the phrase from what it hears, and reads it into
/// short answers ("Ja." heard as "Arch Linux, Hyprland."). Tested with real
/// models; the wake matcher's fuzziness covers the spelling instead.
fn prompt_for(language: &str, phrase: &str) -> String {
    if language == "en" {
        format!("Talking to my desktop assistant, {}, on Arch Linux and Hyprland.", crate::wake::spelling(phrase))
    } else {
        String::new()
    }
}

impl Stt {
    pub fn load(path: &Path, language: &str, gpu: bool, wake_phrase: &str) -> Result<Self> {
        // Without a log backend feature this routes whisper.cpp/ggml logs to nowhere.
        whisper_rs::install_logging_hooks();
        let mut params = WhisperContextParameters::default();
        params.use_gpu(gpu).flash_attn(gpu);
        let ctx = WhisperContext::new_with_params(path, params)
            .map_err(|e| anyhow!("{e}"))
            .with_context(|| format!("loading {}", path.display()))?;
        let state = ctx.create_state().map_err(|e| anyhow!("{e}"))?;
        Ok(Self {
            _ctx: ctx,
            state: Mutex::new(state),
            language: Mutex::new(language.to_string()),
            wake_phrase: Mutex::new(wake_phrase.to_string()),
        })
    }

    pub fn set_wake_phrase(&self, phrase: &str) {
        *self.wake_phrase.lock().unwrap_or_else(|e| e.into_inner()) = phrase.to_string();
    }

    /// Recognise `language` ("de", or "auto") from now on.
    pub fn set_language(&self, language: &str) {
        *self.language.lock().unwrap_or_else(|e| e.into_inner()) = language.to_string();
    }

    fn languages(&self) -> (String, String) {
        let language = self.language.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let phrase = self.wake_phrase.lock().unwrap_or_else(|e| e.into_inner()).clone();
        (language, phrase)
    }

    /// The language recognised ("de", "auto", ...).
    pub fn language(&self) -> String {
        self.languages().0
    }

    /// The wake phrase is said in another language than the rest.
    pub fn wake_differs(&self) -> bool {
        let (language, phrase) = self.languages();
        wake_language(&phrase, &language) != language
    }

    /// What was said, in the language spoken. `pcm`: mono f32 at 16 kHz.
    pub fn transcribe(&self, pcm: &[f32]) -> Result<String> {
        let (language, phrase) = self.languages();
        self.run(pcm, &language, &prompt_for(&language, &phrase))
    }

    /// What was said, heard in the wake phrase's language: for spotting it.
    pub fn transcribe_wake(&self, pcm: &[f32]) -> Result<String> {
        let (language, phrase) = self.languages();
        let language = wake_language(&phrase, &language);
        self.run(pcm, &language, &prompt_for(&language, &phrase))
    }

    fn run(&self, pcm: &[f32], language: &str, prompt: &str) -> Result<String> {
        let mut p = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        p.set_language((language != "auto" && !language.is_empty()).then_some(language));
        p.set_n_threads(4);
        p.set_no_timestamps(true);
        p.set_no_context(true);
        p.set_suppress_blank(true);
        p.set_suppress_nst(true);
        p.set_print_progress(false);
        p.set_print_realtime(false);
        p.set_print_special(false);
        p.set_print_timestamps(false);
        // Bias spelling toward the wake phrase.
        if !prompt.is_empty() {
            p.set_initial_prompt(prompt);
        }

        // whisper wants at least ~1 s of audio; pad short clips with silence.
        let mut audio = pcm.to_vec();
        if audio.len() < 16_000 + 1_600 {
            audio.resize(16_000 + 1_600, 0.0);
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.full(p, &audio).map_err(|e| anyhow!("{e}"))?;

        let mut text = String::new();
        for seg in state.as_iter() {
            if let Ok(s) = seg.to_str_lossy() {
                text.push_str(&s);
            }
        }
        Ok(clean(&text))
    }
}

/// Drop whisper's non-speech annotations like "[BLANK_AUDIO]" or "(wind blowing)".
fn clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    #[test]
    fn wake_languages() {
        assert_eq!(super::wake_language("Fishsticks  and custard", "de"), "en");
        assert_eq!(super::wake_language("hallo computer", "de"), "de");
        assert_eq!(super::wake_language("hey computer", "en"), "en");
    }

    #[test]
    fn strips_annotations() {
        assert_eq!(super::clean(" [BLANK_AUDIO] hello (laughs) there "), "hello there");
    }
}
