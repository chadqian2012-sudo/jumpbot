use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};
mod analysis;
mod regression;
mod report;
mod round;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn sha(path: &Path) -> Result<String> {
    let mut hash = Sha256::new();
    std::io::copy(&mut fs::File::open(path)?, &mut hash)?;
    Ok(format!("{:x}", hash.finalize()))
}
fn read_json(path: &Path) -> Result<Value> {
    Ok(serde_json::from_str(
        fs::read_to_string(path)?.trim_start_matches('\u{feff}'),
    )?)
}
fn save(path: &Path, v: &Value) -> Result<()> {
    let mut tmp = tempfile::NamedTempFile::new_in(path.parent().unwrap_or(Path::new(".")))?;
    serde_json::to_writer_pretty(&mut tmp, v)?;
    tmp.flush()?;
    tmp.persist(path)?;
    Ok(())
}
#[derive(Parser)]
#[command(about = "JumpBot offline regression, HUD reports and opt-in bounded acceptance")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}
#[derive(Subcommand)]
enum Cmd {
    Analyze(analysis::Options),
    #[command(alias = "run_frozen_acceptance")]
    RunFrozenAcceptance(round::Round),
    #[command(alias = "run_skin_acceptance")]
    RunSkinAcceptance(round::Suite),
    #[command(alias = "summarize_acceptance")]
    SummarizeAcceptance {
        directory: PathBuf,
        #[arg(long, default_value = ".")]
        source_root: PathBuf,
    },
    Report {
        csv: PathBuf,
    },
    #[command(alias = "skin_regress")]
    SkinRegress(regression::Skin),
    #[command(alias = "bench_backends")]
    BackendRegress(regression::Backends),
}
fn run() -> Result<bool> {
    match Cli::parse().cmd {
        Cmd::Analyze(o) => analysis::run(o),
        Cmd::RunFrozenAcceptance(r) => round::single(r),
        Cmd::RunSkinAcceptance(s) => round::suite(s),
        Cmd::Report { csv } => {
            let rows = report::Rows::read(&csv)?;
            println!("{}", serde_json::to_string_pretty(&rows.report())?);
            Ok(true)
        }
        Cmd::SummarizeAcceptance {
            directory,
            source_root,
        } => {
            let s = report::summarize(&directory, &source_root)?;
            save(&directory.join("summary.json"), &s)?;
            println!("{}", serde_json::to_string_pretty(&s)?);
            Ok(s["round_pass"] == true)
        }
        Cmd::SkinRegress(s) => regression::skin(s),
        Cmd::BackendRegress(b) => regression::backends(b),
    }
}
fn main() {
    match run() {
        Ok(true) => (),
        Ok(false) => std::process::exit(1),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
