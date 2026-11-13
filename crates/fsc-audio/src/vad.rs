//! Silero VAD v5 (ONNX), 16 kHz, 512-sample chunks.
//!
//! The model is stateful: it carries a [2, 1, 128] recurrent state and
//! expects each chunk prefixed with the last 64 samples of the previous one,
//! the same as the reference Python wrapper does.

use anyhow::{Context, Result};
use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;

pub const CHUNK: usize = 512;
const CONTEXT: usize = 64;
const STATE: usize = 2 * 128;

pub struct Vad {
    session: Session,
    state: Vec<f32>,
    context: Vec<f32>,
}

impl Vad {
    pub fn load(env: &ort::environment::Environment, path: &Path) -> Result<Self> {
        let session = Session::builder(env)?
            .with_intra_threads(1)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .commit_from_file(path)
            .with_context(|| format!("loading {}", path.display()))?;
        Ok(Self { session, state: vec![0.0; STATE], context: vec![0.0; CONTEXT] })
    }

    pub fn reset(&mut self) {
        self.state.iter_mut().for_each(|s| *s = 0.0);
        self.context.iter_mut().for_each(|s| *s = 0.0);
    }

    /// Speech probability for one 512-sample chunk at 16 kHz.
    pub fn prob(&mut self, chunk: &[f32]) -> Result<f32> {
        debug_assert_eq!(chunk.len(), CHUNK);
        let mut input = Vec::with_capacity(CONTEXT + CHUNK);
        input.extend_from_slice(&self.context);
        input.extend_from_slice(chunk);
        self.context.copy_from_slice(&chunk[CHUNK - CONTEXT..]);

        let outputs = self.session.run(ort::inputs![
            "input" => Tensor::from_array(([1usize, CONTEXT + CHUNK], input))?,
            "state" => Tensor::from_array(([2usize, 1, 128], self.state.clone()))?,
            "sr" => Tensor::from_array(((), vec![16000i64]))?,
        ])?;
        let (_, p) = outputs["output"].try_extract_tensor::<f32>()?;
        let p = p.first().copied().unwrap_or(0.0);
        let (_, st) = outputs["stateN"].try_extract_tensor::<f32>()?;
        self.state.copy_from_slice(&st[..STATE]);
        Ok(p)
    }
}
