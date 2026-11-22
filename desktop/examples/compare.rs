//! Compare two frozen workers on the same offline fixtures with ABBA ordering.
//! cargo run --release --example compare -- OLD_EXE NEW_EXE "tests/fixtures/*.png"
use clap::Parser;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    process::{Command, ExitCode},
};

#[derive(Parser)]
struct Args {
    baseline: PathBuf,
    candidate: PathBuf,
    images: String,
    #[arg(long, default_value = "simd")]
    backend: String,
    #[arg(long)]
    candidate_backend: Option<String>,
    #[arg(long)]
    baseline_config: Option<PathBuf>,
    #[arg(long)]
    candidate_config: Option<PathBuf>,
    #[arg(long, default_value_t = 7, value_parser = clap::value_parser!(u32).range(1..))]
    repeat: u32,
}

#[derive(Default)]
struct Sample {
    times: [Vec<f64>; 2],
    reference: Value,
}

fn quantile(mut values: Vec<f64>, fraction: f64) -> f64 {
    values.sort_unstable_by(f64::total_cmp);
    values[(values.len() as f64 * fraction).ceil() as usize - 1]
}

fn compare(o: Args) -> Result<bool, String> {
    let mut frames = BTreeMap::<String, Sample>::new();
    let mut failures = Vec::new();
    let mut expected = None;
    for side in [0, 1, 1, 0] {
        let path = [&o.baseline, &o.candidate][side]
            .canonicalize()
            .map_err(|e| e.to_string())?;
        let backend = if side == 1 {
            o.candidate_backend.as_ref().unwrap_or(&o.backend)
        } else {
            &o.backend
        };
        let mut cmd = Command::new(path);
        cmd.args([
            "--image",
            &o.images,
            "--backend",
            backend,
            "--bench-repeat",
            &o.repeat.to_string(),
        ]);
        if let Some(config) = [&o.baseline_config, &o.candidate_config][side] {
            cmd.arg("--config").arg(config);
        }
        let result = cmd
            .env("JUMPBOT_NO_INPUT", "1")
            .output()
            .map_err(|e| e.to_string())?;
        if !result.status.success() {
            return Err(format!(
                "worker {}: {}",
                result.status,
                String::from_utf8_lossy(&result.stderr)
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for line in String::from_utf8(result.stdout)
            .map_err(|e| e.to_string())?
            .lines()
        {
            let mut row: Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
            let object = row.as_object_mut().ok_or("invalid worker row")?;
            let key = object
                .remove("image")
                .and_then(|v| v.as_str().map(str::to_owned))
                .ok_or("missing image")?;
            if !seen.insert(key.clone()) {
                return Err(format!("duplicate {key}"));
            }
            let actual = object.remove("backend").ok_or("missing backend")?;
            let requested = if backend == "gpu" {
                "cuda"
            } else {
                backend.as_str()
            };
            if requested != "auto" && actual.as_str() != Some(requested) {
                return Err(format!("{key}: requested {requested}, worker ran {actual}"));
            }
            let frame = frames.entry(key.clone()).or_default();
            let timing = object.remove("detect_us").ok_or("missing timing")?;
            let samples = timing
                .as_array()
                .filter(|v| v.len() == o.repeat as usize)
                .ok_or("incorrect sample count")?;
            for sample in samples {
                frame.times[side].push(
                    sample
                        .as_f64()
                        .filter(|v| v.is_finite() && *v > 0.)
                        .ok_or("invalid time")?,
                );
            }
            if frame.reference.is_null() {
                frame.reference = row;
            } else if frame.reference != row {
                let keys: Vec<_> = row
                    .as_object()
                    .unwrap()
                    .keys()
                    .filter(|k| frame.reference[*k] != row[*k])
                    .cloned()
                    .collect();
                failures.push(
                    json!({"image":key,"changed": keys,"expected":frame.reference,"actual":row}),
                );
            }
        }
        if seen.is_empty() || expected.as_ref().is_some_and(|prior| prior != &seen) {
            return Err("workers returned different or empty fixture sets".into());
        }
        expected = Some(seen);
    }
    let mut totals = [Vec::new(), Vec::new()];
    let mut rows = Vec::new();
    for (key, frame) in &frames {
        rows.push(
            json!({"image":key,"baseline_median_ms":quantile(frame.times[0].clone(),0.5)/1000.,
            "candidate_median_ms":quantile(frame.times[1].clone(),0.5)/1000.}),
        );
        for (t, s) in totals.iter_mut().zip(&frame.times) {
            t.extend(s.iter().copied());
        }
    }
    let summary = totals.map(|v| {
        json!({"n":v.len(),"median_ms":quantile(v.clone(),0.5)/1000.,
        "p95_ms":quantile(v,0.95)/1000.})
    });
    println!("{}",serde_json::to_string_pretty(&json!({"frames":frames.len(),"baseline":summary[0],
        "candidate":summary[1],"decisions_match":failures.is_empty(),"mismatches":failures,"images":rows})).unwrap());
    Ok(failures.is_empty())
}
fn main() -> ExitCode {
    match compare(Args::parse()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
