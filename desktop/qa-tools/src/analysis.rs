use super::{
    report::{self, Row, Rows},
    Result,
};
use clap::{Args, Subcommand};
use regex::Regex;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Args)]
pub struct Options {
    #[command(subcommand)]
    command: Kind,
}
#[derive(Subcommand)]
enum Kind {
    Compare {
        #[arg(long,required=true,value_delimiter=',',num_args=1..)]
        a: Vec<PathBuf>,
        #[arg(long,required=true,value_delimiter=',',num_args=1..)]
        b: Vec<PathBuf>,
    },
    Bias {
        csv: Vec<PathBuf>,
        #[arg(long)]
        by_distance: bool,
    },
    Cadence {
        csv: Vec<PathBuf>,
    },
    Miss {
        csv: Vec<PathBuf>,
        #[arg(long)]
        all: bool,
    },
}
fn paths(mut v: Vec<PathBuf>, all: bool) -> Result<Vec<PathBuf>> {
    if v.is_empty() {
        v = std::fs::read_dir("logs")?
            .collect::<std::io::Result<Vec<_>>>()?
            .into_iter()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.starts_with("run_") && s.ends_with(".csv"))
            })
            .collect();
        v.sort_by_cached_key(|p| p.metadata().and_then(|m| m.modified()).ok());
        if !all {
            v = v.pop().into_iter().collect();
        }
    }
    if v.is_empty() {
        return Err("没有运行日志".into());
    }
    Ok(v)
}
fn num(row: &Row, key: &str) -> Option<f64> {
    row.get(key)?
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite())
}
fn band(d: f64) -> &'static str {
    match d {
        v if v < 120. => "<120",
        v if v < 180. => "120-180",
        v if v < 240. => "180-240",
        v if v < 300. => "240-300",
        v if v < 360. => "300-360",
        _ => "360+",
    }
}
#[derive(Default, Copy, Clone)]
struct Count {
    hits: usize,
    judged: usize,
    unknown: usize,
}
impl Count {
    fn add(&mut self, label: Option<bool>) {
        if let Some(hit) = label {
            self.judged += 1;
            self.hits += usize::from(hit)
        } else {
            self.unknown += 1;
        }
    }
    fn rate(self) -> Option<f64> {
        (self.judged > 0).then(|| self.hits as f64 / self.judged as f64)
    }
    fn value(self) -> Value {
        json!({"hits":self.hits,"ordinary":self.judged-self.hits,"unknown":self.unknown,
        "judged":self.judged,"rate_pct":self.rate().map(|r|r*100.),"wilson95_pct":report::wilson(self.hits,self.judged)})
    }
}
fn visit(files: &[PathBuf], mut add: impl FnMut(&Path, &Row, Option<bool>)) -> Result<()> {
    for file in files {
        let rows = Rows::read(file)?;
        for (row, label) in rows.0.iter().zip(report::labels(&rows.0)) {
            if row.get("result").is_some_and(|v| v == "landed") {
                add(file, row, label);
            }
        }
    }
    Ok(())
}
fn compare(a: Count, b: Count) -> Value {
    let delta = a.rate().zip(b.rate()).map(|(x, y)| y - x);
    let p = delta.map(|diff| {
        let pooled = (a.hits + b.hits) as f64 / (a.judged + b.judged) as f64;
        let error = (pooled * (1. - pooled) * (1. / a.judged as f64 + 1. / b.judged as f64)).sqrt();
        if error == 0. {
            1.
        } else {
            libm::erfc(diff.abs() / (error * std::f64::consts::SQRT_2))
        }
    });
    json!({"a":a.value(),"b":b.value(),"delta_percentage_points":delta.map(|v|v*100.),"two_sided_p":p})
}
fn group(files: &[PathBuf]) -> Result<BTreeMap<String, Count>> {
    let mut counts = BTreeMap::<String, Count>::new();
    visit(files, |_, r, label| {
        counts.entry("all".into()).or_default().add(label);
        if let Some(d) = num(r, "dist_px") {
            counts.entry(band(d).into()).or_default().add(label);
        }
    })?;
    Ok(counts)
}
fn median(v: &mut [f64]) -> f64 {
    v.sort_unstable_by(f64::total_cmp);
    (v[(v.len() - 1) / 2] + v[v.len() / 2]) * 0.5
}
fn cadence(path: &Path) -> Result<Value> {
    let rows = Rows::read(path)?;
    let (mut gaps, mut holds, mut overhead) = (vec![], vec![], vec![]);
    let mut phases = BTreeMap::<&str, Vec<f64>>::new();
    let mut previous = None;
    for r in &rows.0 {
        if !r.get("result").is_some_and(|v| v == "landed") {
            continue;
        }
        let Some(t) = r.get("timestamp").and_then(|s| {
            let raw = s.rsplit(['T', ' ']).next()?;
            let mut parts = raw.split(':');
            let h = parts.next()?.parse::<u32>().ok()?;
            let m = parts.next()?.parse::<u32>().ok()?;
            let sec = parts.next()?.parse::<f64>().ok()?;
            (h < 24 && m < 60 && (0.0..60.).contains(&sec))
                .then_some(f64::from(h * 3600 + m * 60) + sec)
        }) else {
            continue;
        };
        if let Some((last, hold)) = previous {
            let gap = (t - last) * 1000.;
            if gap > 0. && gap < 20000. {
                gaps.push(gap);
                holds.push(hold);
                overhead.push(gap - hold);
            }
        }
        previous = Some((t, num(r, "hold_ms").unwrap_or(0.)));
        for key in ["t_acquire_ms", "t_landing_ms", "t_score_ms", "t_loop_ms"] {
            if let Some(v) = num(r, key) {
                phases.entry(key).or_default().push(v);
            }
        }
    }
    let n = gaps.len();
    if n < 20 {
        return Ok(json!({"file":path,"n":n,"insufficient":true}));
    }
    let over_median = median(&mut overhead);
    Ok(
        json!({"file":path,"n":n,"gap_med_ms":median(&mut gaps),"gap_mean_ms":gaps.iter().sum::<f64>()/n as f64,
        "hold_med_ms":median(&mut holds),"overhead_med_ms":over_median,"overhead_mean_ms":overhead.iter().sum::<f64>()/n as f64,
        "overhead_p90_ms":overhead[(n*9)/10],"over_1200ms_pct":overhead.iter().filter(|&&x|x>1200.).count() as f64*100./n as f64,
        "phase_medians_ms":phases.iter_mut().filter(|(_,v)|v.len()>=20).map(|(k,v)|(*k,median(v))).collect::<BTreeMap<_,_>>() }),
    )
}
pub fn run(o: Options) -> Result<bool> {
    let result = match o.command {
        Kind::Compare { a, b } => {
            let a = group(&a)?;
            let b = group(&b)?;
            let mut output = BTreeMap::new();
            for key in [
                "all", "<120", "120-180", "180-240", "240-300", "300-360", "360+",
            ] {
                output.insert(
                    key,
                    compare(
                        a.get(key).copied().unwrap_or_default(),
                        b.get(key).copied().unwrap_or_default(),
                    ),
                );
            }
            json!(output)
        }
        Kind::Bias { csv, by_distance } => {
            let mut levels = BTreeMap::<String, Count>::new();
            let mut distances = BTreeMap::<String, Count>::new();
            visit(&paths(csv, true)?, |_, r, l| {
                let Some(bias) = num(r, "bias_pct") else {
                    return;
                };
                let key = bias.to_string();
                levels.entry(key.clone()).or_default().add(l);
                if let Some(d) = num(r, "dist_px") {
                    distances
                        .entry(format!("{key}@{}", band(d)))
                        .or_default()
                        .add(l);
                }
            })?;
            let best = levels
                .iter()
                .filter_map(|(k, c)| Some((k, c.rate()?)))
                .max_by(|a, b| a.1.total_cmp(&b.1));
            let zero = levels.get("0").and_then(|c| c.rate());
            json!({"levels":levels.iter().map(|(k,c)|(k,c.value())).collect::<BTreeMap<_,_>>(),
                "best_bias_pct":best.map(|(k,_)|k),"best_minus_zero_pp":best.zip(zero).map(|((_,x),y)|(x-y)*100.),
                "distance":by_distance.then(||distances.iter().map(|(k,c)|(k,c.value())).collect::<BTreeMap<_,_>>())})
        }
        Kind::Cadence { csv } => json!(paths(csv, false)?
            .iter()
            .map(|p| cadence(p))
            .collect::<Result<Vec<_>>>()?),
        Kind::Miss { csv, all } => {
            let re = Regex::new(r"tb=(-?\d+),(-?\d+),(\d+),(\d+)\s+tw=(\d+)")?;
            let mut counts = BTreeMap::<&str, Count>::new();
            let mut misses = Vec::new();
            visit(&paths(csv, all)?, |file, r, l| {
                let Some(hit) = l else { return };
                let Some(d) = num(r, "dist_px") else { return };
                let dims = r
                    .get("note")
                    .and_then(|note| re.captures(note))
                    .map(|c| [3, 4, 5].map(|i| c[i].parse::<u32>().unwrap_or(0)));
                let class = match dims {
                    None => "旧日志无几何信息",
                    Some([w, h, t]) => {
                        let w1 = f64::from(w.max(1));
                        let ratio = h as f64 / w1;
                        if ratio > 1.8 {
                            "细长高物件"
                        } else if ratio < 0.85 {
                            "扁宽块"
                        } else if w < 90 {
                            "小块"
                        } else if t as f64 / w1 < 0.45 {
                            "粘连块"
                        } else {
                            "常规方块"
                        }
                    }
                };
                counts.entry(class).or_default().add(Some(hit));
                if !hit && misses.len() < 40 {
                    misses.push(json!({"file":file,"index":r.get("index"),"distance":d,"geometry":dims,"class":class,"error_px":num(r,"error_px")}));
                }
            })?;
            json!({"groups":counts.iter().map(|(k,c)|(k,c.value())).collect::<BTreeMap<_,_>>(),"first_40_misses":misses})
        }
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(true)
}
