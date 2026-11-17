//! Where the speech models live and where they come from.
//! `fsc fetch-models` downloads anything missing into `models_dir()`.
//!
//! Downloads come from fixed upstream revisions (`model_hashes`), and each
//! file is checked against the SHA-256 recorded for it before it's used:
//! whisper.cpp and ONNX Runtime parse these files in C/C++, so a replaced
//! upload must never load.

use std::path::PathBuf;

use crate::config::VoiceConfig;
use crate::model_hashes::{PIPER_REV, SHA256, SILERO_REV, WHISPER_REV};

pub struct ModelFile {
    /// What it's for, shown while downloading.
    pub label: &'static str,
    /// `None` when the configured name can't be turned into a download.
    pub url: Option<String>,
    pub path: PathBuf,
}

impl ModelFile {
    /// The SHA-256 this version expects, for any file it offers. `None` for a
    /// voice or whisper model named in the config but not known here.
    pub fn sha256(&self) -> Option<&'static str> {
        let name = self.path.file_name()?.to_str()?;
        SHA256.iter().find(|(n, _)| *n == name).map(|(_, sha)| *sha)
    }
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

pub fn piper_path(voice: &str) -> PathBuf {
    models_dir().join(format!("{voice}.onnx"))
}

pub fn piper_config_path(voice: &str) -> PathBuf {
    models_dir().join(format!("{voice}.onnx.json"))
}

/// The two files of a Piper voice.
pub fn voice_files(voice: &str) -> [ModelFile; 2] {
    let base = piper_base_url(voice);
    [
        ModelFile { label: "voice (piper)", url: base.as_ref().map(|b| format!("{b}.onnx")), path: piper_path(voice) },
        ModelFile { label: "voice config (piper)", url: base.map(|b| format!("{b}.onnx.json")), path: piper_config_path(voice) },
    ]
}

/// The voice is downloaded.
pub fn has_voice(voice: &str) -> bool {
    voice_files(voice).iter().all(|m| m.path.exists())
}

/// "en_GB-alan-medium" -> ".../en/en_GB/alan/medium/en_GB-alan-medium"
fn piper_base_url(voice: &str) -> Option<String> {
    let mut parts = voice.splitn(3, '-');
    let (locale, name, quality) = (parts.next()?, parts.next()?, parts.next()?);
    let lang = locale.split('_').next()?;
    Some(format!(
        "https://huggingface.co/rhasspy/piper-voices/resolve/{PIPER_REV}/{lang}/{locale}/{name}/{quality}/{voice}"
    ))
}

pub fn required(v: &VoiceConfig) -> Vec<ModelFile> {
    let mut out = vec![
        ModelFile {
            label: "speech recognition (whisper)",
            url: Some(format!(
                "https://huggingface.co/ggerganov/whisper.cpp/resolve/{WHISPER_REV}/ggml-{}.bin",
                v.whisper_model
            )),
            path: whisper_path(v),
        },
        ModelFile {
            label: "voice activity detection (silero)",
            url: Some(format!(
                "https://raw.githubusercontent.com/snakers4/silero-vad/{SILERO_REV}/src/silero_vad/data/silero_vad.onnx"
            )),
            path: vad_path(),
        },
    ];
    // Always listed, so a malformed name shows up as missing (with a clear
    // error from fetch-models) instead of silently disabling speech.
    out.extend(voice_files(&v.voice()));
    out
}

/// Download `m` into place (through a `.part` file, so a broken download
/// never looks finished). `progress(done, total)` is called as it goes.
pub fn download(m: &ModelFile, mut progress: impl FnMut(u64, Option<u64>)) -> anyhow::Result<()> {
    use anyhow::{Context, bail};
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    let Some(url) = m.url.as_deref() else {
        bail!(
            "can't download the {}: `voice.piper_voice` must look like locale-name-quality, e.g. \"en_GB-alan-medium\"",
            m.label
        );
    };
    let resp = ureq::get(&encode_url(url)).call().with_context(|| format!("downloading {url}"))?;
    if resp.status() != 200 {
        bail!("HTTP {} for {url}", resp.status());
    }
    let total: Option<u64> =
        resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok());
    let mut body = resp.into_body().into_with_config().limit(u64::MAX).reader();
    if let Some(dir) = m.path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let part = m.path.with_extension("part");
    let mut out = std::fs::File::create(&part).with_context(|| format!("creating {}", part.display()))?;
    let mut buf = vec![0u8; 1 << 16];
    let mut done: u64 = 0;
    let mut hash = Sha256::new();
    loop {
        let n = body.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        hash.update(&buf[..n]);
        done += n as u64;
        progress(done, total);
    }
    out.flush()?;
    drop(out);
    // Short only: a compressed reply decodes to more than its length.
    if total.is_some_and(|t| done < t) {
        let _ = std::fs::remove_file(&part);
        bail!("{url} ended early ({done} of {} bytes)", total.unwrap_or(0));
    }
    let got: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    match m.sha256() {
        Some(want) if got != want => {
            let _ = std::fs::remove_file(&part);
            bail!(
                "{url} isn't the file this version was tested with (SHA-256 {got}, expected {want}). \
                 It wasn't saved. Try again later, and report it if it keeps happening."
            );
        }
        Some(_) => {}
        // A model chosen by name in the config: still from a fixed revision.
        None => eprintln!("fsc: {} has no recorded checksum (SHA-256 {got})", m.path.display()),
    }
    std::fs::rename(&part, &m.path)?;
    Ok(())
}

/// Percent-encode what a URL can't carry as it is (voice names such as
/// "pt_PT-tugão-medium").
fn encode_url(url: &str) -> String {
    let mut out = String::with_capacity(url.len());
    for b in url.bytes() {
        if b.is_ascii_graphic() {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn missing(v: &VoiceConfig) -> Vec<ModelFile> {
    required(v).into_iter().filter(|m| !m.path.exists()).collect()
}

#[cfg(test)]
mod tests {
    use crate::model_hashes::PIPER_REV;

    #[test]
    fn bad_voice_name_is_reported_not_skipped() {
        let v = crate::config::VoiceConfig { piper_voice: "alan".into(), language: "auto".into(), ..Default::default() };
        let piper: Vec<_> = super::required(&v).into_iter().filter(|m| m.label.contains("piper")).collect();
        assert_eq!(piper.len(), 2);
        assert!(piper.iter().all(|m| m.url.is_none()));
    }

    #[test]
    fn non_ascii_urls_are_encoded() {
        assert_eq!(super::encode_url("https://h/pt/pt_PT/tugão/x"), "https://h/pt/pt_PT/tug%C3%A3o/x");
    }

    #[test]
    fn piper_url() {
        assert_eq!(
            super::piper_base_url("en_GB-alan-medium").unwrap(),
            format!("https://huggingface.co/rhasspy/piper-voices/resolve/{PIPER_REV}/en/en_GB/alan/medium/en_GB-alan-medium")
        );
    }

    #[test]
    fn every_offered_model_has_a_checksum() {
        use crate::config::VoiceConfig;
        for v in crate::voices::VOICES {
            for m in super::voice_files(v.key) {
                assert!(m.sha256().is_some(), "{} has no checksum: run packaging/release/model_hashes.py", v.key);
            }
        }
        for m in super::required(&VoiceConfig { language: "en".into(), ..Default::default() }) {
            assert!(m.sha256().is_some(), "{} has no checksum", m.label);
        }
    }

    #[test]
    fn a_replaced_file_is_refused() {
        // A tiny "model" served locally whose bytes don't match the checksum.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut s, _) = listener.accept().unwrap();
            let mut req = [0u8; 1024];
            let _ = s.read(&mut req);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nevil");
        });
        let dir = std::env::temp_dir().join(format!("fsc-models-test-{}", std::process::id()));
        let path = dir.join("silero_vad.onnx");
        let m = super::ModelFile { label: "test", url: Some(format!("http://127.0.0.1:{port}/x")), path: path.clone() };
        let err = super::download(&m, |_, _| {}).unwrap_err().to_string();
        assert!(err.contains("isn't the file"), "{err}");
        assert!(!path.exists() && !path.with_extension("part").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
