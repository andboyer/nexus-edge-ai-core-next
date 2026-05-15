//! `nexus-probe` CLI — one-shot host enumeration.
//!
//! Writes a JSON `device-manifest.json` describing the box. Used by deploy
//! tooling to pick the right Dockerfile target / EP feature flags.
//!
//! All real probing logic lives in [`nexus_probe`] (the library); this
//! binary is a thin wrapper so the engine can call the same code via
//! `--tier auto` (M-Install Checkpoint 1) without shelling out.

use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "nexus-probe",
    version,
    about = "Enumerate this host's hardware + runtimes"
)]
struct Cli {
    /// Where to write the manifest. `-` writes to stdout.
    #[arg(long, default_value = "data/device-manifest.json")]
    out: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let m = nexus_probe::build_manifest();
    let json = serde_json::to_string_pretty(&m)?;
    if cli.out == "-" {
        println!("{json}");
    } else {
        let path = PathBuf::from(&cli.out);
        if let Some(p) = path.parent() {
            fs::create_dir_all(p)?;
        }
        fs::write(&path, json)?;
        eprintln!("wrote {}", path.display());
    }
    Ok(())
}
