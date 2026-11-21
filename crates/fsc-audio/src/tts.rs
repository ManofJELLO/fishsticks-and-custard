//! Text to speech with a Piper (VITS) voice on ONNX Runtime.
//!
//! text --espeak-ng--> IPA phonemes --voice id map--> ids --onnx--> PCM
//!
//! The id sequence follows Piper: BOS, PAD, then each phoneme followed by
//! PAD, then EOS.

use anyhow::{Context, Result};
use ort::session::Session;
use ort::value::Tensor;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Deserialize)]
struct VoiceFile {
    audio: Audio,
    espeak: Espeak,
    inference: Inference,
    phoneme_id_map: HashMap<String, Vec<i64>>,
    #[serde(default)]
    num_speakers: u32,
}

#[derive(Deserialize)]
struct Audio {
    sample_rate: u32,
}

#[derive(Deserialize)]
struct Espeak {
    voice: String,
}

#[derive(Deserialize)]
struct Inference {
    noise_scale: f32,
    length_scale: f32,
    noise_w: f32,
}

pub struct Tts {
    session: Session,
    voice: VoiceFile,
    ids: HashMap<char, Vec<i64>>,
    rate: f32,
}

impl Tts {
    pub fn load(env: &ort::environment::Environment, model: &Path, config: &Path, rate: f32) -> Result<Self> {
        let voice: VoiceFile = serde_json::from_str(
            &std::fs::read_to_string(config).with_context(|| format!("reading {}", config.display()))?,
        )?;
        let ids = voice
            .phoneme_id_map
            .iter()
            .filter_map(|(k, v)| {
                let mut chars = k.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => Some((c, v.clone())),
                    _ => None,
                }
            })
            .collect();
        let session = Session::builder(env)?
            // On the CPU, everywhere. ONNX Runtime's own pick would be Core ML on
            // macOS, which crashes running these models (and they're small).
            .with_auto_device(ort::session::builder::AutoDevicePolicy::PreferCPU)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .with_intra_threads(4)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .commit_from_file(model)
            .with_context(|| format!("loading {}", model.display()))?;
        Ok(Self { session, voice, ids, rate: rate.clamp(0.5, 2.0) })
    }

    pub fn sample_rate(&self) -> u32 {
        self.voice.audio.sample_rate
    }

    /// Phonemize and synthesize `text`. Returns mono f32 at `sample_rate()`.
    pub fn synth(&mut self, text: &str) -> Result<Vec<f32>> {
        let sentences = espeak_rs::text_to_phonemes(text, &self.voice.espeak.voice, None)
            .map_err(|e| anyhow::anyhow!("espeak: {e:?}"))?;
        let mut audio = Vec::new();
        let gap = vec![0.0f32; self.sample_rate() as usize / 8];
        for phonemes in sentences {
            let ids = self.to_ids(&phonemes);
            if ids.len() <= 3 {
                continue;
            }
            audio.extend(self.infer(ids)?);
            audio.extend_from_slice(&gap);
        }
        Ok(audio)
    }

    fn to_ids(&self, phonemes: &str) -> Vec<i64> {
        let id = |c: char| self.ids.get(&c).cloned().unwrap_or_default();
        let pad = id('_');
        let mut out = id('^');
        out.extend(&pad);
        for c in phonemes.chars() {
            if let Some(v) = self.ids.get(&c) {
                out.extend(v);
                out.extend(&pad);
            }
        }
        out.extend(id('$'));
        out
    }

    fn infer(&mut self, ids: Vec<i64>) -> Result<Vec<f32>> {
        let n = ids.len();
        let inf = &self.voice.inference;
        let scales = vec![inf.noise_scale, inf.length_scale / self.rate, inf.noise_w];
        let input = Tensor::from_array(([1usize, n], ids))?;
        let lengths = Tensor::from_array(([1usize], vec![n as i64]))?;
        let scales = Tensor::from_array(([3usize], scales))?;
        // A voice with several speakers speaks as its first.
        let outputs = if self.voice.num_speakers > 1 {
            let sid = Tensor::from_array(([1usize], vec![0i64]))?;
            self.session.run(ort::inputs!["input" => input, "input_lengths" => lengths, "scales" => scales, "sid" => sid])?
        } else {
            self.session.run(ort::inputs!["input" => input, "input_lengths" => lengths, "scales" => scales])?
        };
        let (_, pcm) = outputs[0].try_extract_tensor::<f32>()?;
        Ok(pcm.iter().map(|s| s.clamp(-1.0, 1.0)).collect())
    }
}
