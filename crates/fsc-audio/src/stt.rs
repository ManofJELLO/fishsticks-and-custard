//! Speech to text with whisper.cpp (Vulkan on AMD).

use anyhow::{Context, Result, anyhow};
use std::path::Path;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

pub struct Stt {
    ctx: WhisperContext,
    language: String,
}

impl Stt {
    pub fn load(path: &Path, language: &str, gpu: bool) -> Result<Self> {
        // Without a log backend feature this routes whisper.cpp/ggml logs to nowhere.
        whisper_rs::install_logging_hooks();
        let mut params = WhisperContextParameters::default();
        params.use_gpu(gpu).flash_attn(gpu);
        let ctx = WhisperContext::new_with_params(path, params)
            .map_err(|e| anyhow!("{e}"))
            .with_context(|| format!("loading {}", path.display()))?;
        Ok(Self { ctx, language: language.to_string() })
    }

    /// `pcm`: mono f32 at 16 kHz.
    pub fn transcribe(&self, pcm: &[f32]) -> Result<String> {
        let mut state = self.ctx.create_state().map_err(|e| anyhow!("{e}"))?;
        let mut p = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        let lang = (self.language != "auto").then_some(self.language.as_str());
        p.set_language(lang);
        p.set_n_threads(4);
        p.set_no_timestamps(true);
        p.set_no_context(true);
        p.set_suppress_blank(true);
        p.set_suppress_nst(true);
        p.set_print_progress(false);
        p.set_print_realtime(false);
        p.set_print_special(false);
        p.set_print_timestamps(false);
        // Bias spelling toward the assistant's name (and the wake phrase).
        p.set_initial_prompt("Talking to my desktop assistant, Fishsticks and Custard, on Arch Linux and Hyprland.");

        // whisper wants at least ~1 s of audio; pad short clips with silence.
        let mut audio = pcm.to_vec();
        if audio.len() < 16_000 + 1_600 {
            audio.resize(16_000 + 1_600, 0.0);
        }
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
    fn strips_annotations() {
        assert_eq!(super::clean(" [BLANK_AUDIO] hello (laughs) there "), "hello there");
    }
}
