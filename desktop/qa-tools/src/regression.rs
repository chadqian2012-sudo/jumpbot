//! Offline child processes keep the candidate and the oracle in independent address spaces.
use super::*;
use clap::Args;
use std::{
    collections::{BTreeMap, BTreeSet},
    process::Command,
};
use walkdir::WalkDir;
#[derive(Args)]
pub struct Images {
    #[arg(long)]
    exe: PathBuf,
    #[arg(long, required = true)]
    dataset: Vec<PathBuf>,
    #[arg(long)]
    out: PathBuf,
    #[arg(long,default_value_t=1,value_parser=clap::value_parser!(u32).range(1..))]
    repeat: u32,
}
#[derive(Args)]
pub struct Skin {
    #[command(flatten)]
    images: Images,
    /// Independent baseline package, worker executable or frozen decisions JSONL.
    #[arg(long)]
    baseline: PathBuf,
}
#[derive(Args)]
pub struct Backends {
    #[command(flatten)]
    images: Images,
    #[arg(long,value_delimiter=',',default_value="cpu,simd",value_parser=["cpu","simd","auto","cuda","gpu","opencl"])]
    backend: Vec<String>,
}
fn clean(p: &Path) -> bool {
    let s = p.to_string_lossy().to_lowercase();
    let name = p
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    if !name.ends_with(".png")
        || [
            "annotated",
            "mask",
            "digit",
            "zoom",
            "hud",
            "score",
            "center_check",
            "chk_",
            "verify_",
            "piece_",
            "edge_",
            "block_",
            "green_",
            "white_",
            "dbg26",
        ]
        .iter()
        .any(|pat| s.contains(pat))
    {
        return false;
    }
    name.starts_with("purple-")
        || name.starts_with("skin-")
        || matches!(name.as_str(), "probe.png" | "before.png")
        || ["jump_", "dry_", "sample_", "fail_", "after_"]
            .iter()
            .any(|prefix| {
                name.starts_with(prefix) && (!name.starts_with("jump_") || name.contains("_raw"))
            })
}
impl Images {
    fn files(&self) -> Result<Vec<PathBuf>> {
        let mut result = BTreeSet::new();
        for path in &self.dataset {
            if path.is_file() {
                if !clean(path) {
                    return Err(format!("Expected a clean game frame: {}", path.display()).into());
                }
                result.insert(path.canonicalize()?);
            } else {
                for item in WalkDir::new(path) {
                    let e = item?;
                    if e.file_type().is_file() && clean(e.path()) {
                        result.insert(e.path().canonicalize()?);
                    }
                }
            }
        }
        if result.is_empty() {
            return Err("No clean game frames".into());
        }
        Ok(result.into_iter().collect())
    }
    fn write(&self, s: &Value) -> Result<()> {
        if let Some(p) = self.out.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(p)?;
        }
        save(&self.out, s)?;
        println!("{}", self.out.display());
        Ok(())
    }
}
fn exe(path: &Path) -> Result<PathBuf> {
    Ok(if path.is_dir() {
        path.join("jumpbot.exe")
    } else {
        path.to_path_buf()
    }
    .canonicalize()?)
}
fn rows(exe: &Path, files: &[PathBuf], backend: &str, repeat: u32) -> Result<Vec<Value>> {
    let mut all = vec![];
    for batch in files.chunks(10) {
        let mut cmd = Command::new(exe);
        cmd.env("JUMPBOT_NO_INPUT", "1").args([
            "--backend",
            backend,
            "--bench-repeat",
            &repeat.to_string(),
        ]);
        if matches!(backend, "simd" | "cpu") {
            cmd.args(["--no-cuda", "--no-opencl"]);
        }
        for p in batch {
            cmd.arg("--image").arg(p);
        }
        let o = cmd.output()?;
        if !o.status.success() {
            return Err(format!(
                "{} {backend} exited {}: {}",
                exe.display(),
                o.status,
                String::from_utf8_lossy(&o.stderr)
            )
            .into());
        }
        let records = String::from_utf8(o.stdout)?
            .lines()
            .map(serde_json::from_str)
            .collect::<std::result::Result<Vec<Value>, _>>()?;
        if records.len() != batch.len() {
            return Err("Incomplete offline CLI output".into());
        }
        for (p, row) in batch.iter().zip(&records) {
            if Path::new(row["image"].as_str().ok_or("Missing image")?).canonicalize()? != *p {
                return Err("Wrong image in CLI output".into());
            }
            for k in [
                "piece",
                "target",
                "hold_ms",
                "score",
                "piece_mask_crc32",
                "block_mask_crc32",
                "backend",
                "detect_us",
            ] {
                if row.get(k).is_none() {
                    return Err(format!("Missing {k} in CLI output").into());
                }
            }
        }
        all.extend(records);
    }
    Ok(all)
}
fn equals(a: &Value, b: &Value) -> bool {
    match a {
        Value::Number(n) => n
            .as_f64()
            .zip(b.as_f64())
            .is_some_and(|(x, y)| (x - y).abs() < 1.0e-6),
        Value::Object(map) => {
            b.is_object()
                && map
                    .iter()
                    .all(|(k, x)| b.get(k).is_some_and(|y| equals(x, y)))
        }
        Value::Array(arr) => b
            .as_array()
            .is_some_and(|v| v.len() == arr.len() && arr.iter().zip(v).all(|(x, y)| equals(x, y))),
        _ => a == b,
    }
}
fn timings(rows: &[Value]) -> Value {
    let mut t: Vec<_> = rows
        .iter()
        .filter_map(|r| r["detect_us"].as_array())
        .flatten()
        .filter_map(Value::as_f64)
        .collect();
    t.sort_by(f64::total_cmp);
    if t.is_empty() {
        return Value::Null;
    }
    let middle = t.len() / 2;
    json!({"samples":t.len(),"median_us":(t[middle]+t[(t.len()-1)/2])/2.,"p95_us":t[(t.len()*95).div_ceil(100)-1]})
}
fn compare(paths: &[PathBuf], old: &[Value], new: &[Value]) -> Result<Value> {
    let (mut changed, mut lost, mut added, mut masks, mut score, mut holds) =
        (vec![], vec![], vec![], vec![], vec![], vec![]);
    let (mut a, mut b) = (0, 0);
    for ((path, before), after) in paths.iter().zip(old).zip(new) {
        let old_target = !before["target"].is_null();
        let new_target = !after["target"].is_null();
        a += usize::from(old_target);
        b += usize::from(new_target);
        for k in ["piece", "target", "score"] {
            if before.get(k).is_none() {
                return Err(format!("Missing baseline {k} for {}", path.display()).into());
            }
        }
        let entry = json!({"path":path,"before":{"piece":before["piece"],"target":before["target"]},"after":{"piece":after["piece"],"target":after["target"]}});
        if !equals(&before["piece"], &after["piece"])
            || !equals(&before["target"], &after["target"])
        {
            changed.push(entry.clone());
        }
        if old_target && !new_target {
            lost.push(entry.clone());
        }
        if !old_target && new_target {
            added.push(entry);
        }
        let mut different = false;
        for (k, legacy) in [
            ("piece_mask_crc32", "piece_mask"),
            ("block_mask_crc32", "block_mask"),
        ] {
            let expected = before
                .get(k)
                .or_else(|| before.get(legacy))
                .and_then(Value::as_u64)
                .ok_or("Missing baseline mask CRC")?;
            different |= after[k].as_u64() != Some(expected);
        }
        if different {
            masks.push(path);
        }
        if !equals(&before["score"], &after["score"]) {
            score.push(path);
        }
        if let Some(old) = before.get("hold_ms") {
            if !equals(old, &after["hold_ms"]) {
                holds.push(json!({"path":path,"before":old,"after":after["hold_ms"]}));
            }
        }
    }
    Ok(
        json!({"frames":paths.len(),"baseline_positive":a,"candidate_positive":b,"changed":changed,"lost":lost,"added":added,"raw_mask_changes":masks,"score_changes":score,"hold_changes":holds,"baseline_timing":timings(old),"candidate_timing":timings(new)}),
    )
}
pub fn skin(s: Skin) -> Result<bool> {
    let paths = s.images.files()?;
    let candidate = exe(&s.images.exe)?;
    let baseline = s.baseline.canonicalize()?;
    let file = if baseline.is_dir() && baseline.join("v1.3-decisions.jsonl").is_file() {
        baseline.join("v1.3-decisions.jsonl")
    } else {
        baseline.clone()
    };
    let reference = if file.extension().is_some_and(|e| e == "jsonl") {
        let mut map = BTreeMap::new();
        for line in fs::read_to_string(&file)?.lines() {
            let v: Value = serde_json::from_str(line)?;
            let name = v
                .get("file")
                .and_then(Value::as_str)
                .ok_or("Missing frozen frame name")?
                .to_owned();
            if map.insert(name, v).is_some() {
                return Err("Duplicate frozen reference".into());
            }
        }
        paths
            .iter()
            .map(|p| {
                map.remove(&p.file_name().unwrap().to_string_lossy().into_owned())
                    .ok_or_else(|| {
                        format!("Missing or duplicate baseline frame {}", p.display()).into()
                    })
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        let old = exe(&file)?;
        if old == candidate {
            return Err("Independent baseline required".into());
        }
        rows(&old, &paths, "simd", s.images.repeat)?
    };
    let new = rows(&candidate, &paths, "simd", s.images.repeat)?;
    let mut summary = compare(&paths, &reference, &new)?;
    let annotated = file.extension().is_some_and(|ext| ext == "jsonl");
    let ok = (!annotated || summary["changed"] == json!([]))
        && ["lost", "raw_mask_changes", "score_changes"]
            .iter()
            .all(|k| summary[*k] == json!([]));
    summary["baseline_sha256"] = sha(&if file.is_dir() { exe(&file)? } else { file })?.into();
    summary["candidate_sha256"] = sha(&candidate)?.into();
    summary["passed"] = ok.into();
    s.images.write(&summary)?;
    Ok(ok)
}
pub fn backends(s: Backends) -> Result<bool> {
    let paths = s.images.files()?;
    let exe = exe(&s.images.exe)?;
    let reference = rows(&exe, &paths, "simd", s.images.repeat)?;
    let mut report =
        json!({"source_sha256":sha(&exe)?,"verified_frames":paths.len(),"backends":{}});
    let mut ok = reference.iter().all(|v| v["backend"] == "simd");
    for backend in &s.backend {
        let values = if backend == "simd" {
            reference.clone()
        } else {
            rows(&exe, &paths, backend, s.images.repeat)?
        };
        let mut diff = compare(&paths, &reference, &values)?;
        let actual: BTreeSet<_> = values
            .iter()
            .filter_map(|v| v["backend"].as_str())
            .collect();
        let selected = !actual.is_empty()
            && actual.iter().all(|&v| {
                backend == "auto"
                    || backend == v
                    || (matches!(v, "gpu" | "cuda") && matches!(backend.as_str(), "gpu" | "cuda"))
            });
        let equal = selected
            && [
                "changed",
                "raw_mask_changes",
                "score_changes",
                "hold_changes",
            ]
            .iter()
            .all(|k| diff[*k] == json!([]));
        ok &= equal;
        diff["actual"] = json!(actual);
        diff["passed"] = equal.into();
        report["backends"][backend] = diff;
    }
    report["passed"] = ok.into();
    s.images.write(&report)?;
    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn added_target_and_raw_mask_change_stay_visible() {
        let old =
            json!({"piece":null,"target":null,"score":null,"piece_mask":123,"block_mask":456});
        let new = json!({"piece":null,"target":{"x":1},"score":null,"piece_mask_crc32":123,"block_mask_crc32":457});
        let report = compare(&[PathBuf::from("fall.png")], &[old], &[new]).unwrap();
        assert_eq!(report["added"].as_array().unwrap().len(), 1);
        assert_eq!(report["changed"].as_array().unwrap().len(), 1);
        assert_eq!(report["raw_mask_changes"], json!(["fall.png"]));
        assert_eq!(report["lost"], json!([]));
        assert!(!clean(Path::new("debug/masks/jump_001_raw.png")));
        assert!(!clean(Path::new("debug/jump_001_after.png")));
        assert!(clean(Path::new("debug/run_1/jump_001_raw_after.png")));
    }
}
