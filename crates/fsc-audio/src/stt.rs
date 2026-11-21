//! Speech to text with whisper.cpp, on the GPU through Vulkan (AMD, Intel and
//! NVIDIA alike), else on the CPU.

use anyhow::{Context, Result, anyhow};
use std::path::Path;
use std::sync::Mutex;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

/// Shared between the audio thread and wake-phrase transcriptions. One
/// whisper state is reused for every run, which also means runs take turns
/// (on the GPU they would anyway). The wake phrase may have a small model of
/// its own (`voice.wake_model`), so it's heard fast.
pub struct Stt {
    _ctx: WhisperContext,
    state: Mutex<WhisperState>,
    wake: Option<(WhisperContext, Mutex<WhisperState>)>,
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

/// whisper.cpp found a GPU but couldn't start it, so it runs on the CPU.
static GPU_FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// whisper.cpp's and ggml's log: errors go to ours, the rest is dropped,
/// and a GPU that fails to start is noted for `device`.
unsafe extern "C" fn whisper_log(
    level: whisper_rs::whisper_rs_sys::ggml_log_level,
    text: *const std::os::raw::c_char,
    _: *mut std::os::raw::c_void,
) {
    use std::io::Write;
    if text.is_null() {
        return;
    }
    // SAFETY: ggml passes a NUL-terminated message, valid for this call.
    let text = unsafe { std::ffi::CStr::from_ptr(text) }.to_string_lossy();
    if text.contains("whisper_backend_init_gpu: failed to initialize") {
        GPU_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    if level == whisper_rs::whisper_rs_sys::ggml_log_level_GGML_LOG_LEVEL_ERROR {
        let _ = write!(std::io::stderr(), "whisper: {}", if text.ends_with('\n') { text.to_string() } else { format!("{text}\n") });
    }
}

/// Where whisper.cpp runs: the first GPU it lists (its default device), for
/// the log, since "it's slow" is usually "it's on the CPU".
fn device(gpu: bool) -> String {
    use std::ffi::CStr;
    use whisper_rs::whisper_rs_sys as sys;
    if !gpu {
        return "the CPU (voice.gpu is off)".into();
    }
    let mut gpus = Vec::new();
    // SAFETY: ggml's device registry is static once whisper.cpp is loaded;
    // these only read it, and the names live as long as the registry.
    unsafe {
        for i in 0..sys::ggml_backend_dev_count() {
            let dev = sys::ggml_backend_dev_get(i);
            let kind = sys::ggml_backend_dev_type(dev);
            if kind == sys::ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_GPU
                || kind == sys::ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_IGPU
            {
                let desc = sys::ggml_backend_dev_description(dev);
                let name = if desc.is_null() { sys::ggml_backend_dev_name(dev) } else { desc };
                gpus.push(if name.is_null() { "?".into() } else { CStr::from_ptr(name).to_string_lossy().into_owned() });
            }
        }
    }
    if GPU_FAILED.load(std::sync::atomic::Ordering::Relaxed) {
        let name = gpus.first().map_or("the GPU", String::as_str);
        return format!("the CPU: {name} didn't start (whisper's error is above)");
    }
    match gpus.split_first() {
        None => "the CPU: no Vulkan GPU found (is your graphics driver's Vulkan package installed?)".into(),
        Some((first, [])) => format!("the GPU: {first}"),
        Some((first, rest)) => format!("the GPU: {first} (also found: {})", rest.join(", ")),
    }
}

impl Stt {
    /// `wake`: the wake phrase's own model, if it has one. If that one
    /// can't be loaded, the main model listens for the phrase too.
    pub fn load(path: &Path, wake: Option<&Path>, language: &str, gpu: bool, wake_phrase: &str) -> Result<Self> {
        // SAFETY: `whisper_log` is a plain function that never unwinds.
        unsafe { whisper_rs::whisper_rs_sys::whisper_log_set(Some(whisper_log), std::ptr::null_mut()) };
        GPU_FAILED.store(false, std::sync::atomic::Ordering::Relaxed);
        let (ctx, state) = load_model(path, gpu)?;
        eprintln!("fsc: speech recognition runs on {}", device(gpu));
        let wake = wake.and_then(|p| match load_model(p, gpu) {
            Ok((c, s)) => Some((c, Mutex::new(s))),
            Err(e) => {
                eprintln!("fsc: wake phrase model unavailable ({e:#}); the main model listens for it");
                None
            }
        });
        Ok(Self {
            _ctx: ctx,
            state: Mutex::new(state),
            wake,
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
        self.run(&self.state, pcm, &language, &prompt_for(&language, &phrase))
    }

    /// What was said, heard in the wake phrase's language (by the wake
    /// phrase's model, if it has one): for spotting it.
    pub fn transcribe_wake(&self, pcm: &[f32]) -> Result<String> {
        let (language, phrase) = self.languages();
        let language = wake_language(&phrase, &language);
        let state = self.wake.as_ref().map_or(&self.state, |(_, s)| s);
        self.run(state, pcm, &language, &prompt_for(&language, &phrase))
    }

    /// The wake phrase has a (smaller) model of its own: what's said after
    /// it is heard again with the main one.
    pub fn separate_wake(&self) -> bool {
        self.wake.is_some()
    }

    fn run(&self, state: &Mutex<WhisperState>, pcm: &[f32], language: &str, prompt: &str) -> Result<String> {
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
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
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

fn load_model(path: &Path, gpu: bool) -> Result<(WhisperContext, WhisperState)> {
    let mut params = WhisperContextParameters::default();
    params.use_gpu(gpu).flash_attn(gpu);
    let ctx = WhisperContext::new_with_params(path, params)
        .map_err(|e| anyhow!("{e}"))
        .with_context(|| format!("loading {}", path.display()))?;
    let state = ctx.create_state().map_err(|e| anyhow!("{e}"))?;
    Ok((ctx, state))
}

/// Whole lines whisper invents on short or quiet clips, learned from video
/// subtitles: outros and subtitle credits. A sentence that is one of these
/// (mostly: see `invented`) is dropped, so it's never sent as a question.
const INVENTED: &[&str] = &[
    "thanks for watching",
    "thank you for watching",
    "thank you so much for watching",
    "please subscribe",
    "subtitles by the amara.org community",
    "untertitel im auftrag des zdf",
    "untertitel der amara.org-community",
    "sous-titres réalisés par la communauté d'amara.org",
    "hãy subscribe cho kênh ghiền mì gõ để không bỏ lỡ những video hấp dẫn",
    "cảm ơn các bạn đã theo dõi",
    "시청해주셔서 감사합니다",
    "구독과 좋아요 부탁드립니다",
    "请不吝点赞 订阅 转发 打赏支持明镜与点点栏目",
    "字幕由amara.org社区提供",
    "谢谢观看",
    "terima kasih telah menonton",
    "jangan lupa subscribe",
];

/// Drop whisper's non-speech annotations like "[BLANK_AUDIO]" or "(wind
/// blowing)", and the lines it invents.
fn clean(s: &str) -> String {
    let s = &drop_invented(s);
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

/// Letters and digits only, lower case, no spaces: how lines are compared.
fn squashed(s: &str) -> String {
    s.to_lowercase().chars().filter(|c| fsc_core::lang::is_word_char(*c)).collect()
}

/// `sentence` is an `INVENTED` line: it starts with one that makes up most
/// of it, or is the start of one, long enough not to be a few real words.
/// "Thank you for watching the kids, remind me at five" is kept.
fn invented(sentence: &str) -> bool {
    let s = squashed(sentence);
    let n = fsc_core::lang::weight(&s);
    INVENTED.iter().any(|line| {
        let line = squashed(line);
        let m = fsc_core::lang::weight(&line);
        // The start of a line only counts when it's most of it: "Thank you
        // so much." is someone thanking, not an outro.
        (s.starts_with(&line) && m * 10 >= n * 7) || (line.starts_with(&s) && n >= 16 && n * 4 >= m * 3)
    })
}

/// `s` without its invented sentences. A sentence ends at ".", "!", "?" or
/// "।" followed by a space or the end (so "Amara.org" stays whole), or at
/// Chinese "。！？".
fn drop_invented(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut kept = String::new();
    let mut sentence = String::new();
    for (i, &c) in chars.iter().enumerate() {
        sentence.push(c);
        let next_blank = chars.get(i + 1).is_none_or(|n| n.is_whitespace());
        if matches!(c, '。' | '！' | '？') || (matches!(c, '.' | '!' | '?' | '।') && next_blank) {
            if !invented(&sentence) {
                kept.push_str(&sentence);
            }
            sentence.clear();
        }
    }
    if !invented(&sentence) {
        kept.push_str(&sentence);
    }
    kept
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
    fn drops_invented_lines() {
        assert_eq!(super::clean("Thanks for watching!"), "");
        assert_eq!(super::clean("Turn the music down. Thank you for watching."), "Turn the music down.");
        assert_eq!(super::clean("Hãy subscribe cho kênh Ghiền Mì Gõ Để không bỏ lỡ những video hấp dẫn"), "");
        assert_eq!(super::clean("你好。请不吝点赞 订阅 转发 打赏支持明镜与点点栏目"), "你好。");
        assert_eq!(super::clean("Thank you so much."), "Thank you so much.");
        assert_eq!(super::clean("Cảm ơn các bạn đã giúp."), "Cảm ơn các bạn đã giúp.");
        assert_eq!(super::clean("Thanks, that's all."), "Thanks, that's all.");
        assert_eq!(super::clean("Subtitles by the Amara.org community"), "");
        assert_eq!(super::clean("Thank you for watching the kids, remind me at five."), "Thank you for watching the kids, remind me at five.");
        assert_eq!(super::clean("Zeig mir die Untertitel von diesem Film."), "Zeig mir die Untertitel von diesem Film.");
        assert_eq!(super::clean("MBC 뉴스 틀어줘"), "MBC 뉴스 틀어줘");
        assert_eq!(super::clean("Thanks for."), "Thanks for.");
    }

    #[test]
    fn strips_annotations() {
        assert_eq!(super::clean(" [BLANK_AUDIO] hello (laughs) there "), "hello there");
    }
}
