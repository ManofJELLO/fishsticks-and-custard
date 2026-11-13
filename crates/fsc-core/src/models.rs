//! Where the speech models live and where they come from.
//! `fsc fetch-models` downloads anything missing into `models_dir()`.

use std::path::PathBuf;

use crate::config::VoiceConfig;

pub struct ModelFile {
    /// What it's for, shown while downloading.
    pub label: &'static str,
    pub url: String,
    pub path: PathBuf,
}

pub fn models_dir() -> PathBuf {
    directories::BaseDirs::new()
        .map(|d| d.data_dir().join("fsc").join("models"))
        .unwrap_or_else(|| PathBuf::from(".fsc-models"))
}

pub fn whisper_path(v: &VoiceConfig) -> PathBuf {
    models_dir().join(format!("ggml-{}.bin", v.whisper_model))
}

pub fn vad_path() -> PathBuf {
    models_dir().join("silero_vad.onnx")
}

pub fn piper_path(v: &VoiceConfig) -> PathBuf {
    models_dir().join(format!("{}.onnx", v.piper_voice))
}

pub fn piper_config_path(v: &VoiceConfig) -> PathBuf {
    models_dir().join(format!("{}.onnx.json", v.piper_voice))
}

/// "en_GB-alan-medium" -> ".../en/en_GB/alan/medium/en_GB-alan-medium"
fn piper_base_url(voice: &str) -> Option<String> {
    let mut parts = voice.splitn(3, '-');
    let (locale, name, quality) = (parts.next()?, parts.next()?, parts.next()?);
    let lang = locale.split('_').next()?;
    Some(format!(
        "https://huggingface.co/rhasspy/piper-voices/resolve/main/{lang}/{locale}/{name}/{quality}/{voice}"
    ))
}

pub fn required(v: &VoiceConfig) -> Vec<ModelFile> {
    let mut out = vec![
        ModelFile {
            label: "speech recognition (whisper)",
            url: format!(
                "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{}.bin",
                v.whisper_model
            ),
            path: whisper_path(v),
        },
        ModelFile {
            label: "voice activity detection (silero)",
            url: "https://raw.githubusercontent.com/snakers4/silero-vad/master/src/silero_vad/data/silero_vad.onnx"
                .into(),
            path: vad_path(),
        },
    ];
    if let Some(base) = piper_base_url(&v.piper_voice) {
        out.push(ModelFile { label: "voice (piper)", url: format!("{base}.onnx"), path: piper_path(v) });
        out.push(ModelFile {
            label: "voice config (piper)",
            url: format!("{base}.onnx.json"),
            path: piper_config_path(v),
        });
    }
    out
}

pub fn missing(v: &VoiceConfig) -> Vec<ModelFile> {
    required(v).into_iter().filter(|m| !m.path.exists()).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn piper_url() {
        assert_eq!(
            super::piper_base_url("en_GB-alan-medium").unwrap(),
            "https://huggingface.co/rhasspy/piper-voices/resolve/main/en/en_GB/alan/medium/en_GB-alan-medium"
        );
    }
}
