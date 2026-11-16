//! `fsc fetch-models`: download whatever the configured voice pipeline needs.

use anyhow::{Context, Result};
use std::process::ExitCode;

use fsc_core::config::Config;
use fsc_core::models::{self, ModelFile};

pub fn run() -> ExitCode {
    match fetch_all() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("fsc: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// `label<TAB>url` for each model the default settings use.
pub fn print_default_urls() -> ExitCode {
    for m in models::required(&Default::default()) {
        if let Some(url) = m.url {
            println!("{}\t{url}", m.label);
        }
    }
    ExitCode::SUCCESS
}

fn fetch_all() -> Result<()> {
    let cfg = Config::load()?;
    let missing = models::missing(&cfg.voice);
    if missing.is_empty() {
        println!("all models present in {}", models::models_dir().display());
        return Ok(());
    }
    std::fs::create_dir_all(models::models_dir())?;
    for m in &missing {
        fetch(m).with_context(|| format!("downloading {}", m.url.as_deref().unwrap_or(m.label)))?;
    }
    println!("done. Restart fscd to load them.");
    Ok(())
}

fn fetch(m: &ModelFile) -> Result<()> {
    let name = m.path.file_name().unwrap_or_default().to_string_lossy().to_string();
    let mut last_pct = u64::MAX;
    models::download(m, |done, total| {
        if let Some(t) = total {
            let pct = done * 100 / t.max(1);
            if pct != last_pct {
                last_pct = pct;
                eprint!("\r{name} ({}): {pct:>3}% of {:.0} MB", m.label, t as f64 / 1e6);
            }
        }
    })?;
    eprintln!("\r{name} ({}): done{:>20}", m.label, "");
    Ok(())
}
