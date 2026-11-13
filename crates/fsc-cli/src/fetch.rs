//! `fsc fetch-models`: download whatever the configured voice pipeline needs.

use anyhow::{Context, Result, bail};
use std::io::{Read, Write};
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

fn fetch_all() -> Result<()> {
    let cfg = Config::load()?;
    let missing = models::missing(&cfg.voice);
    if missing.is_empty() {
        println!("all models present in {}", models::models_dir().display());
        return Ok(());
    }
    std::fs::create_dir_all(models::models_dir())?;
    for m in &missing {
        fetch(m).with_context(|| format!("downloading {}", m.url))?;
    }
    println!("done. Restart fscd to load them.");
    Ok(())
}

fn fetch(m: &ModelFile) -> Result<()> {
    let name = m.path.file_name().unwrap_or_default().to_string_lossy();
    let resp = ureq::get(&m.url).call()?;
    if resp.status() != 200 {
        bail!("HTTP {}", resp.status());
    }
    let total: Option<u64> = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let mut body = resp.into_body().into_with_config().limit(u64::MAX).reader();

    let part = m.path.with_extension("part");
    let mut out = std::fs::File::create(&part)?;
    let mut buf = vec![0u8; 1 << 16];
    let mut done: u64 = 0;
    let mut last_pct = u64::MAX;
    loop {
        let n = body.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        done += n as u64;
        if let Some(t) = total {
            let pct = done * 100 / t.max(1);
            if pct != last_pct {
                last_pct = pct;
                eprint!("\r{name} ({}): {pct:>3}% of {:.0} MB", m.label, t as f64 / 1e6);
            }
        }
    }
    eprintln!("\r{name} ({}): done{:>20}", m.label, "");
    out.flush()?;
    std::fs::rename(&part, &m.path)?;
    Ok(())
}
