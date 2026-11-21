use crate::Result;
use chrono::Local;
use serde_json::{json, Value};
use std::{collections::VecDeque, fs, path::PathBuf};
#[derive(Clone, Copy)]
pub(crate) struct Outcome {
    pub result: &'static str,
    pub score: Option<i32>,
    pub hit: Option<bool>,
}
#[derive(Default)]
struct Counts {
    n: u64,
    land: u64,
    dead: u64,
    hits: u64,
    misses: u64,
}
impl Counts {
    fn add(&mut self, o: Outcome) {
        if o.result == "stopped" {
            return;
        }
        self.n += 1;
        self.land += u64::from(o.result == "landed");
        self.dead += u64::from(o.result == "dead");
        self.hits += u64::from(o.hit == Some(true));
        self.misses += u64::from(o.hit == Some(false))
    }
    fn fields(&self) -> Value {
        let judged = self.hits + self.misses;
        let rate = |a: u64, b: u64| (b != 0).then(|| 100. * a as f64 / b as f64);
        json!({"attempts":self.n,"landed":self.land,"deaths":self.dead,"hits":self.hits,"misses":self.misses,"unknown":self.n-self.dead-judged,"judged":judged,"hit_rate":rate(self.hits,judged),"landing_rate":rate(self.land,self.n),"judgement_coverage":rate(judged,self.n)})
    }
}
pub(crate) struct Records {
    pub path: PathBuf,
    pub run_uid: String,
    writer: Option<csv::Writer<std::fs::File>>,
    all: Counts,
    recent: VecDeque<Outcome>,
    pub index: u64,
    pub prev: Option<i32>,
    pub streak: i32,
    pub stalled: i32,
    best_streak: i32,
    best_score: Option<i32>,
}
impl Records {
    pub fn new(dir: PathBuf) -> Self {
        let run_uid = format!(
            "{}_{}",
            Local::now().format("%Y%m%d_%H%M%S_%9f"),
            std::process::id()
        );
        Self {
            path: dir.join(format!("run_{run_uid}.csv")),
            run_uid,
            best_streak: 0,
            best_score: None,
            writer: None,
            all: Counts::default(),
            recent: VecDeque::new(),
            index: 0,
            prev: None,
            streak: 0,
            stalled: 0,
        }
    }
    pub fn resync(&mut self, score: Option<i32>) {
        self.prev = score;
        self.streak = 0;
        self.stalled = 0;
    }
    pub fn judge(&mut self, result: &'static str, score: Option<i32>, cap: i32) -> Outcome {
        let mut hit = None;
        if result == "landed" {
            if let Some(now) = score {
                if let Some(prev) = self.prev {
                    let delta = now - prev;
                    let delayed = self.stalled > 0;
                    self.stalled = if delta == 0 { self.stalled + 1 } else { 0 };
                    if delayed && delta != 0 {
                        self.streak = 0
                    } else if delta == 1 {
                        hit = Some(false);
                        self.streak = 0
                    } else if delta > 0
                        && delta <= 200
                        && delta == self.streak.saturating_add(1).saturating_mul(2).min(cap)
                    {
                        hit = Some(true);
                        self.streak = self.streak.saturating_add(1)
                    } else if (4..=cap.min(200)).contains(&delta) && delta % 2 == 0 {
                        hit = Some(true);
                        self.streak = delta / 2
                    } else if delta < 0 {
                        self.streak = 0
                    }
                }
                self.prev = Some(now);
                self.best_score = Some(self.best_score.map_or(now, |old| old.max(now)));
                self.best_streak = self.best_streak.max(self.streak);
            } else {
                self.resync(None)
            }
        } else {
            self.resync(None)
        }
        Outcome { result, score, hit }
    }
    pub fn fields(&self) -> Value {
        let mut data = self.all.fields();
        let mut recent = Counts::default();
        for o in &self.recent {
            recent.add(*o)
        }
        data["recent100"] = recent.fields();
        data["jumps"] = json!(self.index);
        data["streak"] = json!(self.streak);
        data["best_streak"] = json!(self.best_streak);
        data["best_life_score"] = json!(self.best_score);
        data["csv"] = json!(self.path);
        data["run_uid"] = json!(self.run_uid);
        data
    }
    #[allow(clippy::too_many_arguments)]
    pub fn emit(
        &mut self,
        o: Outcome,
        k: f64,
        after: f64,
        dist: Option<f64>,
        hold: f64,
        error: Option<(f64, f64, &str)>,
        dot: bool,
        bias: f64,
        backend: &str,
        pause: f64,
        active: f64,
        times: [f64; 4],
        tol: f64,
        target: Option<&crate::vision::Block>,
        dot_error: Option<f64>,
        detail: &str,
    ) -> Result<()> {
        let now = Local::now();
        if self.writer.is_none() {
            use std::io::Write;
            fs::create_dir_all(self.path.parent().unwrap()).map_err(|e| e.to_string())?;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&self.path)
                .map_err(|e| e.to_string())?;
            file.write_all(b"\xef\xbb\xbf").map_err(|e| e.to_string())?;
            let mut writer = csv::WriterBuilder::new()
                .terminator(csv::Terminator::CRLF)
                .from_writer(file);
            writer.write_record("index,timestamp,k_before,dist_px,hold_ms,k_after,error_px,result,score,note,achieved_px,center_hit,t_acquire_ms,t_landing_ms,t_score_ms,t_loop_ms,err_src,err_dot_px,aim_dot,bias_pct,mask_backend,pause_total_s,active_elapsed_s,pixel_center_hit".split(',')).map_err(|e|e.to_string())?;
            self.writer = Some(writer);
            let _ = writeln!(std::io::stdout().lock(), "日志 {}", self.path.display());
        }
        let mut note = match o.hit {
            Some(true) => format!("[center x{}]", self.streak),
            Some(false) => "[miss]".into(),
            None => "不可判定".into(),
        };
        if !detail.is_empty() {
            note.push(' ');
            note.push_str(detail)
        }
        if let Some(t) = target {
            let [x, y, w, h] = t.bbox;
            note.push_str(&format!(" tb={x},{y},{w},{h} tw={:.0}", t.top_width))
        }
        let err = error.map(|e| e.0);
        let fmt = |v: Option<f64>| v.map(|a| format!("{a:.1}")).unwrap_or_default();
        let row = [
            (self.index + 1).to_string(),
            now.format("%H:%M:%S").to_string(),
            format!("{k:.4}"),
            fmt(dist),
            format!("{hold:.0}"),
            format!("{after:.4}"),
            fmt(err),
            o.result.into(),
            o.score.map(|v| v.to_string()).unwrap_or_default(),
            note,
            fmt(err.zip(dist).map(|(e, d)| d + e)),
            o.hit.map(|b| u8::from(b).to_string()).unwrap_or_default(),
            format!("{:.0}", times[0]),
            format!("{:.0}", times[1]),
            format!("{:.0}", times[2]),
            format!("{:.0}", times[3]),
            error.map(|e| e.2).unwrap_or("").into(),
            fmt(dot_error),
            u8::from(dot).to_string(),
            bias.to_string(),
            backend.into(),
            pause.to_string(),
            active.to_string(),
            err.map(|e| u8::from(e.abs() < tol).to_string())
                .unwrap_or_default(),
        ];
        let w = self.writer.as_mut().unwrap();
        w.write_record(row).map_err(|e| e.to_string())?;
        w.flush().map_err(|e| e.to_string())?;
        self.index += 1;
        self.all.add(o);
        self.recent.push_back(o);
        if self.recent.len() > 100 {
            self.recent.pop_front();
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_hud_breaks_the_delta_chain() {
        let mut r = Records::new(PathBuf::new());
        r.resync(Some(10));
        assert_eq!(r.judge("landed", None, 32).hit, None);
        assert_eq!(r.judge("landed", Some(14), 32).hit, None);
        assert_eq!(r.judge("landed", Some(15), 32).hit, Some(false));
    }
    #[test]
    fn late_hud_delta_after_an_unchanged_read_is_not_one_jumps_bonus() {
        let mut r = Records::new(PathBuf::new());
        r.resync(Some(10));
        assert_eq!(r.judge("landed", Some(10), 32).hit, None);
        assert_eq!(r.judge("landed", Some(14), 32).hit, None);
        assert_eq!(r.judge("landed", Some(15), 32).hit, Some(false));
    }
    #[test]
    fn csv_roundtrips_geometry_and_bounded_stats() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Records::new(dir.path().into());
        let t = crate::vision::Block {
            x: 1.,
            y: 2.,
            bbox: [79, 469, 122, 112],
            area: 200,
            top_y: 469.,
            top_width: 83.,
            dot: None,
        };
        assert!(!r.path.exists());
        for i in 0..102 {
            let result = match i {
                0 => "dead",
                101 => "stopped",
                _ => "landed",
            };
            let o = r.judge(result, Some(i), 32);
            r.emit(
                o,
                2.61,
                2.61,
                Some(355.2),
                927.,
                None,
                false,
                0.,
                "cpu",
                0.,
                1.,
                [0.; 4],
                12.,
                Some(&t),
                (i == 2).then_some(-3.12),
                "",
            )?;
        }
        let fields = r.fields();
        assert_eq!(fields["attempts"], 101);
        assert_eq!(fields["recent100"]["attempts"], 99);
        assert_eq!(fields["recent100"]["deaths"], 0);
        assert_eq!(fields["recent100"]["hits"], 0);
        let mut csv = csv::Reader::from_path(&r.path).unwrap();
        assert_eq!(csv.headers().unwrap().iter().collect::<Vec<_>>().join(","),"index,timestamp,k_before,dist_px,hold_ms,k_after,error_px,result,score,note,achieved_px,center_hit,t_acquire_ms,t_landing_ms,t_score_ms,t_loop_ms,err_src,err_dot_px,aim_dot,bias_pct,mask_backend,pause_total_s,active_elapsed_s,pixel_center_hit");
        let row = csv.records().nth(2).unwrap().unwrap();
        assert_eq!(&row[4], "927");
        assert_eq!(&row[9], "[miss] tb=79,469,122,112 tw=83");
        assert_eq!(&row[11], "0");
        assert_eq!(&row[17], "-3.1");
        assert_eq!(&row[23], "");
        Ok(())
    }
    #[test]
    fn hud_counts_follow_score_and_resync() {
        let mut r = Records::new(PathBuf::new());
        r.resync(Some(10));
        assert_eq!(r.judge("landed", Some(12), 32).hit, Some(true));
        assert_eq!(r.judge("landed", Some(13), 32).hit, Some(false));
        assert_eq!(r.judge("landed", Some(16), 32).hit, None);
        r.resync(Some(500));
        assert_eq!(r.judge("landed", Some(532), 32).hit, Some(true));
        assert_eq!(r.judge("unknown", None, 32).hit, None);
    }
}
