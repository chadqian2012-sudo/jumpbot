use super::*;
use regex::Regex;
use std::{collections::HashMap, sync::LazyLock};
pub type Row = HashMap<String, String>;
pub struct Rows(pub Vec<Row>);
pub(super) fn integer(s: Option<&String>) -> Option<i64> {
    s?.parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .map(|v| v as i64)
}
pub(super) fn number(r: &Row, key: &str) -> Option<f64> {
    r.get(key)?.parse::<f64>().ok().filter(|v| v.is_finite())
}
pub(super) fn text<'a>(r: &'a Row, key: &str) -> &'a str {
    r.get(key).map(String::as_str).unwrap_or("").trim()
}
fn round(v: f64, scale: f64) -> f64 {
    // Format the original binary float directly, as Python round(x, ndigits) does.
    // Multiplying first turns e.g. -0.34999999999999998 into a false midpoint.
    let digits = if scale == 10. { 1 } else { 2 };
    format!("{v:.digits$}").parse().unwrap()
}
fn rate(n: usize, total: usize) -> Option<f64> {
    (total > 0).then(|| round(n as f64 / total as f64 * 100., 100.))
}
pub(super) fn wilson(n: usize, total: usize) -> Option<[f64; 2]> {
    if total == 0 {
        return None;
    }
    let z = 1.959963984540054;
    let count = total as f64;
    let p = n as f64 / count;
    let den = 1. + z * z / count;
    let center = (p + z * z / (2. * count)) / den;
    let rad = z * (p * (1. - p) / count + z * z / (4. * count * count)).sqrt() / den;
    Some([
        round((center - rad).max(0.) * 100., 100.),
        round((center + rad).min(1.) * 100., 100.),
    ])
}
fn median(mut v: Vec<f64>) -> Option<f64> {
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).map(|x| round(*x, 10.))
}
static CENTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[center x(\d+)(?:\(重同步\))?\]").unwrap());
impl Rows {
    pub fn read(path: &Path) -> Result<Self> {
        let mut reader = csv::ReaderBuilder::new().flexible(true).from_path(path)?;
        let header = reader.headers()?.clone();
        let rows = reader
            .records()
            .map(|row| {
                let row = row?;
                Ok(header
                    .iter()
                    .enumerate()
                    .map(|(i, k)| (k.to_owned(), row.get(i).unwrap_or("").to_owned()))
                    .collect())
            })
            .collect::<std::result::Result<_, csv::Error>>()?;
        Ok(Self(rows))
    }
    pub fn one(dir: &Path) -> Result<Self> {
        let mut paths = vec![];
        for item in fs::read_dir(dir.join("logs"))? {
            let p = item?.path();
            if p.is_file()
                && p.file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.starts_with("run_") && s.ends_with(".csv"))
            {
                paths.push(p);
            }
        }
        if paths.len() != 1 {
            return Err("Expected one worker CSV for this round".into());
        }
        Self::read(&paths[0])
    }
    pub fn report(&self) -> Value {
        json!({"stats":analyze(&self.0),"recent100":analyze(&self.0[self.0.len().saturating_sub(100)..])})
    }
    pub fn recorded_counts(&self) -> Result<Value> {
        let (mut attempts, mut landed, mut deaths, mut hits, mut misses) = (0, 0, 0, 0, 0);
        for (i, r) in self.0.iter().enumerate() {
            let result = text(r, "result");
            let hit = r
                .get("center_hit")
                .ok_or("CSV missing center_hit")?
                .as_str();
            if text(r, "index") != (i + 1).to_string()
                || !matches!(result, "landed" | "dead" | "stopped" | "unknown")
                || !matches!(hit, "" | "0" | "1")
                || (result != "landed" && !hit.is_empty())
            {
                return Err(format!("Invalid worker CSV row {}", i + 1).into());
            }
            if result == "stopped" {
                continue;
            }
            attempts += 1;
            landed += i32::from(result == "landed");
            deaths += i32::from(result == "dead");
            hits += i32::from(hit == "1");
            misses += i32::from(hit == "0");
        }
        Ok(
            json!({"attempts":attempts,"landed":landed,"deaths":deaths,"hits":hits,"misses":misses,"judged":hits+misses,"unknown":attempts-deaths-hits-misses}),
        )
    }
}
pub(super) fn iter_labels(rows: &[Row]) -> impl Iterator<Item = (&Row, Option<i8>, i64)> {
    let mut prev = None;
    let mut streak = 0i64;
    rows.iter().map(move |r| {
        let score = integer(r.get("score"));
        let result = text(r, "result");
        let mut label = None;
        if result != "landed" {
            prev = None;
            streak = 0;
        } else if r.contains_key("mask_backend") {
            let note = text(r, "note");
            if let Some(cap) = CENTER.captures(note) {
                streak = cap[1].parse().unwrap_or(0);
                label = Some(1);
            } else {
                streak = 0;
                label = Some(if note.contains("[miss]") { 0 } else { -1 });
            }
            prev = score;
        } else if let Some(now) = score {
            if let Some(last) = prev {
                let delta = now - last;
                if delta < 0 {
                    streak = 0;
                } else if delta == 1 {
                    streak = 0;
                    label = Some(0);
                } else if delta == (2 * (streak + 1)).min(32) {
                    streak += 1;
                    label = Some(1);
                } else if (4..=32).contains(&delta) && delta % 2 == 0 {
                    streak = delta / 2;
                    label = Some(1);
                } else {
                    label = Some(-1);
                }
            }
            prev = Some(now);
        }
        (r, label, streak)
    })
}
pub(super) fn labels(rows: &[Row]) -> impl Iterator<Item = Option<bool>> + '_ {
    iter_labels(rows).map(|(_, label, _)| match label {
        Some(1) => Some(true),
        Some(0) => Some(false),
        _ => None,
    })
}
pub fn analyze(rows: &[Row]) -> Value {
    let (mut attempts, mut landed, mut hits, mut misses, mut unknown, mut died) =
        (0, 0, 0, 0, 0, 0);
    let (mut best_streak, mut alive, mut best_alive) = (0i64, 0, 0);
    let (mut prev_score, mut best_score) = (None, None::<i64>);
    let (mut deaths, mut false_deaths, mut resets) = (vec![], vec![], vec![]);
    let (mut errors, mut distances, mut holds) = (vec![], vec![], vec![]);
    for (i, (r, label, streak)) in iter_labels(rows).enumerate() {
        let result = text(r, "result");
        let score = integer(r.get("score"));
        let idx = integer(r.get("index"));
        let death = matches!(result, "dead" | "died");
        if result == "landed" {
            landed += 1;
            alive += 1;
            best_alive = best_alive.max(alive);
        }
        if death {
            died += 1;
            alive = 0;
            if let Some(idx) = idx {
                deaths.push(idx);
                let after = rows[i + 1..]
                    .iter()
                    .take(2)
                    .find_map(|r| integer(r.get("score")));
                if score.zip(after).is_some_and(|(a, b)| b > a) {
                    false_deaths.push(idx);
                }
            }
        }
        if let Some(score) = score {
            best_score = Some(best_score.unwrap_or(0).max(score));
            if prev_score.is_some_and(|s| score < s) {
                resets.extend(idx);
            }
            prev_score = Some(score);
        }
        match label {
            Some(1) => hits += 1,
            Some(0) => misses += 1,
            _ if !death && result != "stopped" => unknown += 1,
            _ => {}
        }
        attempts += usize::from(result != "stopped");
        best_streak = best_streak.max(streak);
        errors.extend(number(r, "error_px").filter(|e| e.abs() < 9999.));
        distances.extend(number(r, "dist_px"));
        holds.extend(integer(r.get("hold_ms")).map(|v| v as f64));
    }
    let judged = hits + misses;
    json!({"rows":rows.len(),"attempts":attempts,"landed":landed,"died":died,"unresolved":attempts-landed-died,"land_rate":rate(landed,attempts),"land_rate_ci95":wilson(landed,attempts),"score_hits":hits,"score_misses":misses,"score_unknown":unknown,"hit_rate":rate(hits,judged),"hit_rate_ci95":wilson(hits,judged),"judgement_coverage":rate(judged,attempts),"best_streak":best_streak,"best_alive":best_alive,"best_life_score":best_score,"death_indices":deaths,"false_death_indices":false_deaths,"reset_indices":resets,"err_mean":(!errors.is_empty()).then(||round(errors.iter().sum::<f64>()/errors.len() as f64,10.)),"err_abs_median":median(errors.iter().map(|e|e.abs()).collect()),"err_median":median(errors),"dist_median":median(distances),"hold_median":median(holds).map(|v|v as i64)})
}
pub fn summarize(dir: &Path, root: &Path) -> Result<Value> {
    let e = read_json(&dir.join("run-evidence.json"))?;
    let rows = Rows::one(dir)?;
    let mut r = rows.report();
    let stats = &r["stats"];
    if e.get("frozen_exe_sha256").is_some() && e.get("source_sha256").is_none() {
        let state = read_json(&dir.join("test-status.json"))?;
        let exe = Path::new(state["frozen_exe"].as_str().ok_or("Missing worker path")?);
        let unchanged = sha(exe)?.eq_ignore_ascii_case(
            e["frozen_exe_sha256"]
                .as_str()
                .ok_or("Missing worker digest")?,
        ) && sha(&exe.with_file_name("build-source.json"))?.eq_ignore_ascii_case(
            e["build_manifest_sha256"]
                .as_str()
                .ok_or("Missing manifest digest")?,
        );
        let counts = rows.recorded_counts()?;
        let verified = round::finish(
            &e["final_status"],
            &counts,
            e["requested_jumps"]
                .as_u64()
                .ok_or("Missing requested jumps")?,
        )
        .is_ok();
        r["round_pass"] = (unchanged
            && verified
            && e["return_code"] == 0
            && round::passed(&state)
            && !dir.join("stop.request").exists())
        .into();
        r["source_unchanged"] = unchanged.into();
        r["recorded_counts"] = counts;
        r["publication_ready"] = false.into();
        return Ok(r);
    }
    let mut owned = false;
    let (mut down, mut up) = (0u64, 0u64);
    let mut paused = None;
    let mut errors = vec![];
    let mut violations = vec![];
    let mut durations = vec![];
    for line in fs::read_to_string(dir.join("input-audit.jsonl"))?.lines() {
        let v: Value = serde_json::from_str(line.trim_start_matches('\u{feff}'))?;
        match v["kind"].as_str().ok_or("Missing audit event kind")? {
            "paused_confirmed" => {
                paused = Some(v["monotonic"].as_f64().ok_or("Missing pause timestamp")?)
            }
            "programmatic_resume_requested" => {
                if let Some(p) = paused.take() {
                    durations.push(v["monotonic"].as_f64().ok_or("Missing resume timestamp")? - p);
                }
            }
            "foreground" => {
                if paused.is_some() {
                    violations.push(v);
                }
            }
            "mouse" => {
                let flags = v["flags"].as_u64().ok_or("Missing mouse flags")?;
                if flags & 2 != 0 {
                    down += 1;
                    if owned {
                        errors.push("double down");
                    }
                    owned = true;
                }
                if flags & 4 != 0 {
                    up += 1;
                    if !owned {
                        errors.push("up without down");
                    }
                    owned = false;
                }
                let c = &v["control"];
                if paused.is_some()
                    || (c["paused"] == true
                        && c["pause_total_s"].as_f64().unwrap_or(0.) > 0.
                        && c["input_active"] != true)
                {
                    violations.push(v);
                }
            }
            _ => {}
        }
    }
    if owned {
        errors.push("held at process exit");
    }
    let mut bad = vec![];
    let resets = stats["reset_indices"].as_array().unwrap();
    for (i, row) in rows.0.iter().enumerate() {
        if let Some(index) = integer(row.get("index")) {
            if resets.contains(&json!(index))
                && !text(row, "note").contains("[score基准]")
                && !rows.0[i.saturating_sub(2)..i]
                    .iter()
                    .any(|r| matches!(text(r, "result"), "dead" | "died"))
            {
                bad.push(index);
            }
        }
    }
    let hashes = e["source_sha256"]
        .as_object()
        .ok_or("Missing source_sha256")?;
    let root = root.canonicalize()?;
    let mut unchanged = !hashes.is_empty();
    for (name, hash) in hashes {
        let path = root.join(name).canonicalize()?;
        if !path.starts_with(&root) {
            return Err("Invalid source path".into());
        }
        unchanged &=
            sha(&path)?.eq_ignore_ascii_case(hash.as_str().ok_or("Invalid source checksum")?);
    }
    let final_s = &e["final_status"];
    let agree = [
        ("attempts", "attempts"),
        ("landed", "landed"),
        ("hits", "score_hits"),
        ("misses", "score_misses"),
    ]
    .iter()
    .all(|&(a, b)| final_s[a] == stats[b]);
    let rate = stats["land_rate"].as_f64().is_some_and(|v| v >= 98.)
        && stats["hit_rate"].as_f64().is_some_and(|v| v >= 95.);
    let requested = e["requested_jumps"]
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or("Invalid requested jumps")?;
    let complete = stats["attempts"].as_u64().unwrap() >= requested;
    let safety = errors.is_empty()
        && violations.is_empty()
        && e["counters"]["paused_violations"] == 0
        && e.get("exception") == Some(&Value::Null)
        && e["return_code"] == 0
        && stats["false_death_indices"] == json!([])
        && bad.is_empty();
    for(k,v)in json!({"requested_jumps":requested,"counters":e["counters"],"down":down,"up":up,"balance_errors":errors,"paused_operations":violations,"programmed_pause_durations_s":durations,"source_unchanged":unchanged,"statistic_agreement":agree,"round_complete":complete,"rate_gate":rate,"safety_gate":safety,"round_pass":complete&&rate&&safety&&unchanged&&agree,"physical_hotkey_repeat_verified":e["physical_hotkey_repeat_verified"],"controlled_window_switch_verified":e["controlled_window_switch_verified"],"publication_ready":false,"unrecorded_restart_indices":bad,"source_sha256":hashes}).as_object().unwrap(){r[k]=v.clone();}
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(index: i64, result: &str, score: i64, note: &str, modern: bool) -> Row {
        let mut r = Row::from_iter([
            ("index".into(), index.to_string()),
            ("result".into(), result.into()),
            ("score".into(), score.to_string()),
            ("note".into(), note.into()),
        ]);
        if modern {
            r.insert("mask_backend".into(), "simd".into());
        }
        r
    }
    #[test]
    fn old_streak_restarts_each_file_and_modern_baseline_ignores_manual_scores() {
        let old: Vec<_> = [10, 12, 13, 45, 48, 3]
            .into_iter()
            .enumerate()
            .map(|(i, s)| row(i as i64, "landed", s, "", false))
            .collect();
        assert_eq!(
            iter_labels(&old).map(|(_, l, _)| l).collect::<Vec<_>>(),
            [None, Some(1), Some(0), Some(1), Some(-1), None]
        );
        assert_eq!(labels(&old[3..]).next(), Some(None));
        let rows = [
            row(1, "landed", 2, "[score基准]", true),
            row(2, "landed", 3, "[miss]", true),
            row(3, "landed", 35, "[score基准]", true),
            row(4, "landed", 37, "[center x1(重同步)]", true),
            row(5, "unknown", 37, "", true),
            row(6, "dead", 37, "", true),
        ];
        let s = analyze(&rows);
        assert_eq!(s["score_hits"], 1);
        assert_eq!(s["score_misses"], 1);
        assert_eq!(s["score_unknown"], 3);
        assert_eq!(s["unresolved"], 1);
        assert_eq!(wilson(0, 0), None);
        assert_eq!(wilson(0, 1), Some([0., 79.35]));
        assert_eq!(round(-0.35, 10.), -0.3);
    }
    #[test]
    fn csv_quotes_bom_short_rows_and_last_hundred() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "\u{feff}index,result,note,mask_backend").unwrap();
        for i in 1..=101 {
            writeln!(f, "{i},landed,\"[center x1] tb=1,2,3,4\",simd").unwrap();
        }
        writeln!(f, "102,stopped").unwrap();
        let rows = Rows::read(f.path()).unwrap();
        assert!(rows.0[0]["note"].ends_with("1,2,3,4"));
        assert_eq!(rows.0.last().unwrap()["mask_backend"], "");
        let s = rows.report();
        assert_eq!(s["stats"]["score_hits"], 101);
        assert_eq!(s["recent100"]["score_hits"], 99);
        assert_eq!(s["recent100"]["rows"], 100);
    }
    #[test]
    fn audit_balance_pause_and_unrecorded_reset_veto_pass() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        fs::create_dir(p.join("logs")).unwrap();
        fs::write(p.join("source"), "known").unwrap();
        fs::write(p.join("logs/run_1.csv"),"index,result,score,note,mask_backend\n1,landed,100,[center x1],cpu\n2,landed,2,[center x1],cpu\n").unwrap();
        let evidence = json!({"source_sha256":{"source":sha(&p.join("source")).unwrap()},"requested_jumps":2,"final_status":{"attempts":2,"landed":2,"hits":2,"misses":0},"return_code":0,"exception":null,"counters":{"paused_violations":0}});
        save(&p.join("run-evidence.json"), &evidence).unwrap();
        fs::write(p.join("input-audit.jsonl"),"{\"kind\":\"paused_confirmed\",\"monotonic\":1}\n{\"kind\":\"mouse\",\"flags\":2}\n{\"kind\":\"programmatic_resume_requested\",\"monotonic\":4}").unwrap();
        let s = summarize(p, p).unwrap();
        assert_eq!(s["unrecorded_restart_indices"], json!([2]));
        assert_eq!(s["round_pass"], false);
        assert_eq!(s["paused_operations"].as_array().unwrap().len(), 1);
        assert_eq!(s["balance_errors"], json!(["held at process exit"]));
        assert_eq!(s["programmed_pause_durations_s"], json!([3.0]));
    }
}
