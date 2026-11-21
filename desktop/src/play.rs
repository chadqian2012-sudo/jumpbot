use crate::{
    asset_dir,
    control::Controls,
    landing,
    options::Options,
    platform::{CaptureMode, PressOutcome, Window},
    records::Records,
    runtime::{annotated, detection},
    screen::Screen,
    vision::{axis_components, Detection, Vision},
    Config, Frame, Result,
};
use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    path::Path,
    time::{Duration, Instant},
};

fn sec(s: f64) -> Duration {
    Duration::from_secs_f64(s)
}
fn ms(s: f64) -> Duration {
    sec(s / 1000.)
}
fn merge(data: &mut Value, parts: Value) {
    if let Some(fields) = parts.as_object() {
        data.as_object_mut().unwrap().extend(fields.clone())
    }
}
fn monotonic_seconds() -> Option<f64> {
    // Python's Windows time.monotonic() uses this same cross-process clock.
    #[link(name = "kernel32")]
    extern "system" {
        fn QueryPerformanceCounter(out: *mut i64) -> i32;
        fn QueryPerformanceFrequency(out: *mut i64) -> i32;
    }
    let (mut count, mut freq) = (0, 0);
    let ok = unsafe {
        QueryPerformanceFrequency(&mut freq) != 0 && QueryPerformanceCounter(&mut count) != 0
    };
    (ok && freq > 0).then(|| count as f64 / freq as f64)
}
fn publish_status(dir: &Path, status: &Value) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    serde_json::to_writer(&mut file, status)?;
    file.flush()?;
    file.persist(dir.join("status.json"))?;
    Ok(())
}
struct Game<'a> {
    o: &'a Options,
    c: &'a Config,
    ctl: Controls,
    screen: &'a Screen,
    vision: &'a mut Vision,
    mode: CaptureMode,
    frame_period: Duration,
    round_start: Option<Duration>,
    status: Value,
}
impl Game<'_> {
    fn report(&mut self, phase: &str, fields: Value) {
        merge(&mut self.status, fields);
        merge(&mut self.status, self.ctl.status(self.round_start, phase));
        self.status["mask_backend"] = json!(self.vision.backend());
        self.status["updated_at"] = json!(chrono::Local::now().timestamp_millis() as f64 / 1000.);
        self.status["updated_monotonic_s"] = json!(monotonic_seconds());
        if self.o.worker {
            let _ = writeln!(std::io::stdout().lock(), "@jumpbot {}", self.status);
        } else {
            let dir = asset_dir().join("debug");
            if let Err(e) = publish_status(&dir, &self.status) {
                eprintln!("无法写入状态文件: {e}")
            }
            let _ = writeln!(
                std::io::stdout().lock(),
                "{}  跳数 {}  分数 {}  命中率 {}  落地率 {}",
                self.status["state"],
                self.status["jumps"],
                self.status["score"],
                self.status["hit_rate"],
                self.status["landing_rate"]
            );
        }
    }
    fn gate(&mut self, initial: bool, timeout: Option<Duration>) -> Result<Option<(Window, u64)>> {
        self.vision.reset_prior();
        let wait = self.ctl.wait_clock();
        let mut countdown = None;
        loop {
            let g = self.ctl.generation();
            if self.ctl.state().stopped {
                return Ok(None);
            }
            if self.ctl.state().pause {
                self.ctl.acknowledge();
                self.report("paused", json!({"countdown_s":null,"window":null}));
                self.ctl.sleep(g, sec(1.));
                countdown = None;
                continue;
            }
            let elapsed = self.ctl.wait_clock().saturating_sub(wait);
            let mut reason = "等待用户打开游戏窗口";
            if let Some(w) = Window::find(self.c, (1, 1))? {
                let Ok(guard) = self.ctl.guard(g, false) else {
                    continue;
                };
                let foreground = self.o.background || w.is_foreground();
                if !foreground {
                    let _ = w.activate();
                }
                drop(guard);
                if foreground {
                    let id = (g, w.hwnd, w.pid, w.client_rect);
                    let (old, start) = countdown.get_or_insert((id, Instant::now()));
                    if *old != id {
                        *old = id;
                        *start = Instant::now()
                    }
                    let left = sec(self.c.f("start_delay_s")).saturating_sub(start.elapsed());
                    self.report(if initial && g==0{"countdown"}else{"resuming"},json!({"window":{"pid":w.pid,"exe":w.exe,"title":w.title,"hwnd":w.hwnd,"client":w.client_rect},"countdown_s":left.as_secs_f64(),"stop_reason":""}));
                    if left.is_zero() {
                        if self.ctl.ready(g).is_ok() {
                            return Ok(Some((w, g)));
                        }
                        continue;
                    }
                    self.ctl.sleep(g, left.min(ms(100.)));
                    continue;
                }
                reason = "请将游戏窗口切到前台";
            }
            countdown = None;
            if timeout.is_some_and(|t| elapsed >= t) {
                return Ok(None);
            }
            self.report("waiting_window",json!({"window":null,"countdown_s":null,"waited_s":elapsed.as_secs_f64(),"stop_reason":reason}));
            let poll = timeout.map_or(ms(200.), |t| ms(200.).min(t.saturating_sub(elapsed)));
            self.ctl.sleep(g, poll);
        }
    }
    fn recognition_frame(&self, w: &Window) -> Result<Option<Frame>> {
        w.capture(self.mode)?
            .map(|raw| raw.for_recognition(self.c))
            .transpose()
    }
    fn press(&self, w: &Window, g: u64, p: (f64, f64), duration: f64) -> Result<PressOutcome> {
        if self.o.dry_run {
            return Err("只读检测模式已阻止按压".into());
        }
        let mut denied = false;
        match w.press(p, ms(duration), self.o.background, || {
            self.ctl.guard(g, true).inspect_err(|_| denied = true)
        }) {
            Ok(PressOutcome::Suppressed) => Err("无输入模式已阻止按压".into()),
            Err(_) if denied => Ok(PressOutcome::Suppressed),
            result => result,
        }
    }
    fn ensure_artifact(&self, name: &str, frame: &Frame) -> Result<()> {
        let dir = self.o.out_dir.clone().unwrap_or_else(|| {
            asset_dir().join("debug").join(if self.o.dry_run {
                "dryrun"
            } else if self.o.jump_test {
                "jumponce"
            } else if self.o.probe && self.o.samples > 0 {
                "samples"
            } else {
                ""
            })
        });
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        frame.write(&dir.join(name))
    }
    fn debug(&self, name: &str, f: &Frame) {
        if let Err(e) = self.ensure_artifact(name, f) {
            eprintln!("{e}")
        }
    }
    fn settle(
        &mut self,
        w: &Window,
        g: u64,
        previous: Option<i32>,
        before: &Frame,
    ) -> Result<Option<Landing>> {
        let reference = before.motion_sample(self.c)?;
        let mut departed = false;
        let mut small = None;
        let mut anchor = None;
        let mut stable = 0;
        let mut outcome = Landing::default();
        let start = Instant::now();
        while start.elapsed() < sec(self.c.f("settle_timeout_s")) {
            if self.ctl.state().stopped {
                outcome.result = "stopped";
                return Ok(Some(outcome));
            }
            if self.ctl.generation() != g {
                if self.status["state"] != "pausing" {
                    self.report("pausing", json!({}))
                }
                if !w.is_foreground() {
                    return Ok(Some(Landing::default()));
                }
            }
            let Some(frame) = self.recognition_frame(w)? else {
                return Ok(None);
            };
            let current = frame.motion_sample(self.c)?;
            let mean_diff = |a: &[u8], b: &[u8]| {
                a.iter()
                    .zip(b)
                    .map(|(a, b)| a.abs_diff(*b) as u64)
                    .sum::<u64>() as f64
                    / a.len() as f64
            };
            departed |= mean_diff(&reference, &current) > self.c.f("settle_diff_threshold");
            let diff = small.replace(current).map_or(999., |last: Vec<u8>| {
                mean_diff(&last, small.as_ref().unwrap())
            });
            outcome.detect = None;
            if departed && diff < self.c.f("settle_diff_threshold") {
                let d = self.vision.detect_piece_only(&frame)?;
                let p = d.piece.as_ref().map(|p| (p.x, p.y));
                let tol = self.c.f("settle_anchor_tol_px");
                let close =
                    p.zip(anchor)
                        .is_some_and(|((x, y), (u, v)): ((f64, f64), (f64, f64))| {
                            (x - u).abs() <= tol && (y - v).abs() <= tol
                        });
                stable = if close && landing::foot(&frame, &d) {
                    stable + 1
                } else {
                    0
                };
                anchor = p;
                outcome.detect = Some(d);
            } else {
                anchor = None;
                stable = 0
            }
            outcome.frame = Some(frame);
            if stable >= self.c.i("settle_frames").max(1) {
                outcome.result = "landed";
                break;
            }
            self.ctl.sleep(
                self.ctl.generation(),
                ms(self.c.f("settle_poll_ms"))
                    .max(self.frame_period)
                    .min(sec(self.c.f("settle_timeout_s")).saturating_sub(start.elapsed())),
            );
        }
        let t = Instant::now();
        if let Some(f) = &outcome.frame {
            if self.screen.button(f)?.is_some_and(|(dead, _)| dead) {
                outcome.result = "dead";
            }
            outcome.score = self.screen.score(f, self.c);
        }
        if outcome.result != "dead" && previous.is_some() {
            for delay in [self.c.f("score_reread_first_ms"), 250., 250.] {
                if outcome.score.is_some() && outcome.score != previous {
                    break;
                }
                if self.ctl.state().stopped || self.ctl.generation() != g {
                    break;
                }
                self.ctl.sleep(g, ms(delay));
                if self.ctl.guard(g, true).is_err() {
                    break;
                }
                let Some(f) = self.recognition_frame(w)? else {
                    return Ok(None);
                };
                if self.screen.button(&f)?.is_some_and(|(dead, _)| dead) {
                    outcome.result = "dead";
                    outcome.score = self.screen.score(&f, self.c);
                    outcome.frame = Some(f);
                    outcome.detect = None;
                    break;
                }
                if let Some(n) = self.screen.score(&f, self.c) {
                    outcome.score = Some(n)
                }
            }
        }
        outcome.result = score_verdict(outcome.result, previous, outcome.score);
        if !w.still_current()? {
            return Ok(None);
        }
        outcome.score_ms = t.elapsed().as_secs_f64() * 1000.;
        Ok(Some(outcome))
    }
}
fn score_verdict(observed: &'static str, previous: Option<i32>, now: Option<i32>) -> &'static str {
    if observed == "unknown" && previous.zip(now).is_some_and(|(a, b)| b > a) {
        "landed"
    } else {
        observed
    }
}
struct Landing {
    result: &'static str,
    score: Option<i32>,
    frame: Option<Frame>,
    detect: Option<Detection>,
    score_ms: f64,
}
impl Landing {
    fn after_input(observation: Result<Self>) -> (Self, Option<String>) {
        match observation {
            Ok(landed) => (landed, None),
            Err(reason) => (Self::default(), Some(reason)),
        }
    }
}
impl Default for Landing {
    fn default() -> Self {
        Self {
            result: "unknown",
            score: None,
            frame: None,
            detect: None,
            score_ms: 0.,
        }
    }
}

fn same_window(a: &Window, b: &Window) -> bool {
    a.hwnd == b.hwnd && a.pid == b.pid && a.title == b.title && a.client_rect == b.client_rect
}
fn refresh_period() -> Duration {
    use windows_sys::Win32::Graphics::Dwm::{DwmGetCompositionTimingInfo, DWM_TIMING_INFO};
    let mut info = DWM_TIMING_INFO {
        cbSize: std::mem::size_of::<DWM_TIMING_INFO>() as u32,
        ..Default::default()
    };
    if unsafe { DwmGetCompositionTimingInfo(std::ptr::null_mut(), &mut info) } >= 0 {
        let rate = info.rateRefresh;
        if rate.uiNumerator > 0 && rate.uiDenominator > 0 {
            return sec(rate.uiDenominator as f64 / rate.uiNumerator as f64);
        }
    }
    ms(17.)
}
fn confirmed(old: Option<(f64, f64)>, new: (f64, f64), tol: f64) -> bool {
    old.is_some_and(|(x, y)| (x - new.0).abs() <= tol && (y - new.1).abs() <= tol)
}
fn diagnostics(game: &mut Game<'_>) -> Result<()> {
    let o = game.o;
    let cfg = game.c;
    if o.probe && o.jump_test {
        return Err("探测与单次试跳不能同时使用".into());
    }
    if o.click_xy.is_some() && (!o.probe || o.jump_test || o.dry_run) {
        return Err("--click-xy 仅用于独立的 --probe".into());
    }
    let find = || Window::find(cfg, (1, 1))?.ok_or_else(|| "未找到游戏窗口".to_string());
    let g = game.ctl.generation();
    let mut w = find()?;
    let raw_frame = |w: &Window| {
        w.capture(game.mode)?
            .ok_or_else(|| "客户区已变化，取消本次诊断".to_string())
    };
    if !o.dry_run && !o.no_activate && !o.background {
        {
            let _guard = game.ctl.guard(g, true)?;
            w.activate()?
        }
        game.ctl.sleep(g, ms(400.));
        w = find()?;
    }
    if let Some(xy) = &o.click_xy {
        let (x, y) = xy.split_once(',').ok_or("click-xy 需要两个比例")?;
        drop(game.ctl.guard(g, true)?);
        let frame = raw_frame(&w)?.for_recognition(cfg)?;
        if frame.modal(cfg) {
            return Err("弹窗遮挡，取消探测点击".into());
        }
        if game.press(
            &w,
            g,
            (
                x.parse().map_err(|_| "x 无效")?,
                y.parse().map_err(|_| "y 无效")?,
            ),
            60.,
        )? != PressOutcome::Sent
        {
            return Err("控制请求已取消探测点击".into());
        }
        game.ctl.sleep(g, ms(400.));
    }
    let jump = o.jump_test && !o.dry_run;
    let count = if jump {
        o.burst.max(1).checked_add(1).ok_or("连拍次数过大")?
    } else {
        o.samples.max(1)
    };
    for i in 0..count {
        drop(game.ctl.guard(g, true)?);
        let fresh = find()?;
        if !same_window(&w, &fresh) {
            game.vision.reset_prior()
        }
        w = fresh;
        let raw = raw_frame(&w)?;
        if o.dry_run || jump {
            let mut f = raw.for_recognition(cfg)?;
            let mut d = game.vision.detect(&f)?;
            if i == 0 && jump {
                let first = d.target.as_ref().map(|t| (t.x, t.y));
                if d.piece.is_none()
                    || first.is_none()
                    || game.screen.button(&f)?.is_some()
                    || f.modal(cfg)
                {
                    return Err("画面未就绪，取消试跳".into());
                }
                game.ctl
                    .sleep(g, ms(cfg.f("acquire_poll_ms")).max(game.frame_period));
                drop(game.ctl.guard(g, true)?);
                f = raw_frame(&w)?.for_recognition(cfg)?;
                d = game.vision.detect(&f)?;
                if game.screen.button(&f)?.is_some()
                    || f.modal(cfg)
                    || !d.target.as_ref().is_some_and(|t| {
                        confirmed(first, (t.x, t.y), cfg.f("target_consistency_px"))
                    })
                {
                    return Err("无法确认新帧目标，取消试跳".into());
                }
                let hold = detection(&d, cfg)["hold_ms"]
                    .as_f64()
                    .ok_or("无法确认棋子和目标")?;
                let duration = if o.hold_ms > 0. { o.hold_ms } else { hold };
                if game.press(
                    &w,
                    g,
                    (cfg.at("press_point", 0), cfg.at("press_point", 1)),
                    duration.clamp(
                        cfg.f("press_duration_min_ms"),
                        cfg.f("press_duration_max_ms"),
                    ),
                )? != PressOutcome::Sent
                {
                    return Err("控制请求已取消试跳".into());
                }
            }
            println!("{}", detection(&d, cfg));
            let name = if !jump {
                format!("dry_{i:03}.png")
            } else if i == 0 {
                "before.png".into()
            } else {
                format!("after_{:02}.png", i - 1)
            };
            game.ensure_artifact(&name, &annotated(&f, &d))?;
        } else {
            let name = if o.samples == 0 {
                "probe.png".into()
            } else {
                format!("sample_{i:03}.png")
            };
            game.ensure_artifact(&name, &raw)?
        }
        if i + 1 < count {
            game.ctl.sleep(
                g,
                sec(if jump {
                    o.burst_interval
                } else {
                    o.sample_interval
                }),
            )
        }
    }
    Ok(())
}
enum End {
    Stopped,
    Finished(&'static str),
    WindowGone,
}
fn round(game: &mut Game<'_>, config_path: &Path, wait: f64) -> Result<End> {
    let cfg = game.c;
    let o = game.o;
    let ctl_key = cfg.s("pause_hotkey");
    game.round_start = None;
    let dir = o
        .log_dir
        .clone()
        .unwrap_or_else(|| asset_dir().join("logs"));
    let mut records = Records::new(dir.clone());
    game.status = records.fields();
    merge(
        &mut game.status,
        json!({"pid":std::process::id(),"started":chrono::Local::now().timestamp(),"pause_hotkey":ctl_key,"max_jumps":cfg.0["max_jumps"],"daemon":o.daemon,"config":config_path,"k":cfg.f("k"),"background_kernel":"rust"}),
    );
    game.report("starting", json!({}));
    let mut window = game.gate(true, (wait > 0.).then(|| sec(wait)))?;
    if window.is_none() && !game.ctl.state().stopped {
        return Err("等待游戏窗口超时".into());
    }
    let active_start = game.ctl.clock();
    if window.is_some() {
        game.round_start = Some(active_start)
    }
    let mut k = cfg.f("k");
    let base = k;
    let mut best_k = k;
    let mut longest = 0;
    let mut alive = 0;
    let mut death_streak = 0;
    let mut ema = 0.;
    let mut previous_center = None;
    let mut attempts = 0;
    let mut rejects = 0;
    let mut blind = Some(0);
    let mut last_hold: f64 = 0.;
    let adaptive = !o.calibrate && cfg.f("gamma") > 0.;
    let mut last_error = None;
    let mut acquisition = Instant::now();
    let mut pressed_button = None;
    let mut calibration = Vec::new();
    let mut band = 0;
    let mut band_ok = 0;
    let mut band_attempts = 0;
    let mut baseline = true;
    let mut last_progress = game.ctl.clock();
    let mut warned = false;
    let limit = cfg.0["max_jumps"].as_u64().unwrap();
    let end = loop {
        if game.ctl.state().stopped {
            break End::Stopped;
        }
        if limit != 0 && records.index >= limit && !game.ctl.state().pause {
            break End::Finished("达到最大跳跃次数");
        }
        let g = window
            .as_ref()
            .map_or_else(|| game.ctl.generation(), |(_, g)| *g);
        let interrupted = game.ctl.guard(g, true).is_err();
        let fresh = if interrupted {
            None
        } else {
            Window::find(cfg, (1, 1))?
        };
        if interrupted
            || window
                .as_ref()
                .zip(fresh.as_ref())
                .is_none_or(|((w, _), f)| {
                    !same_window(f, w) || (!o.background && !f.is_foreground())
                })
        {
            let missing = !interrupted && fresh.is_none();
            baseline = true;
            records.resync(None);
            previous_center = None;
            attempts = 0;
            rejects = 0;
            ema = 0.;
            last_hold = 0.;
            pressed_button = None;
            last_error = None;
            let timeout = if interrupted {
                let t = cfg.f("startup_wait_s");
                (t > 0.).then(|| sec(t))
            } else {
                Some(sec(cfg.f("window_wait_s")))
            };
            // Reacquisition starts from a fresh board, including after a pause.
            // Allow blind retries only after a new target passes both frame checks.
            blind = None;
            window = game.gate(false, timeout)?;
            if window.is_none() && !game.ctl.state().stopped {
                if o.daemon && missing {
                    break End::WindowGone;
                }
                return Err("等待游戏窗口超时".into());
            }
            acquisition = Instant::now();
            last_progress = game.ctl.clock();
            warned = false;
            continue;
        }
        // Taking the snapshot invalidates the plan unless this iteration explicitly keeps it.
        let (w, g) = window.take().unwrap();
        let clock = game.ctl.clock();
        if cfg.f("max_runtime_s") > 0.
            && clock.saturating_sub(active_start) >= sec(cfg.f("max_runtime_s"))
        {
            break End::Finished("达到最长运行时间");
        }
        if !warned && clock.saturating_sub(last_progress) >= sec(90.) {
            eprintln!("已有 90 秒未完成一跳，请检查窗口");
            warned = true;
        }
        let f = match game.recognition_frame(&w) {
            Ok(Some(frame)) => frame,
            Ok(None) => continue,
            Err(reason) => {
                game.report(
                    "blocked_modal",
                    json!({"game_state":"blocked_modal","stop_reason":reason}),
                );
                previous_center = None;
                baseline = true;
                attempts = 0;
                game.vision.reset_prior();
                game.ctl.sleep(g, ms(250.));
                window = Some((w, g));
                continue;
            }
        };
        if game.ctl.guard(g, true).is_err() {
            window = Some((w, g));
            continue;
        }
        let classified = game.screen.button(&f)?;
        if let Some((dead, p)) = classified {
            game.report(
                "acquiring",
                json!({"game_state":if dead{"game_over"}else{"start_page"}}),
            );
            if pressed_button != Some(dead) {
                match game.press(&w, g, (p[0] / f.width as f64, p[1] / f.height as f64), 60.)? {
                    PressOutcome::Sent => pressed_button = Some(dead),
                    PressOutcome::WindowChanged => continue,
                    PressOutcome::Suppressed => {
                        window = Some((w, g));
                        continue;
                    }
                }
            }
            attempts += 1;
            if attempts >= cfg.i("detect_retry_limit") {
                return Err("开始/重玩按钮持续未消失".into());
            }
            previous_center = None;
            baseline = true;
            game.vision.reset_prior();
            game.ctl.sleep(g, ms(1400.));
            window = Some((w, g));
            continue;
        }
        if f.modal(cfg) {
            game.report(
                "blocked_modal",
                json!({"game_state":"blocked_modal","stop_reason":"请关闭遮挡游戏的弹窗"}),
            );
            previous_center = None;
            baseline = true;
            game.vision.reset_prior();
            game.ctl.sleep(g, ms(300.));
            window = Some((w, g));
            continue;
        }
        pressed_button = None;
        let d = game.vision.detect(&f)?;
        if baseline {
            records.resync(game.screen.score(&f, cfg));
            baseline = false
        }
        game.report("acquiring",json!({"piece_source":d.piece.as_ref().map(|p|p.source),"piece_shape_score":d.piece.as_ref().map(|p|p.shape_score),"game_state":"playing","stop_reason":"","score":records.prev}));
        if o.calibrate {
            k = base * o.calib_k_scales[band]
        }
        let k_before = k;
        let pair = d.piece.as_ref().zip(d.target.as_ref());
        let (dist, axis, hold, bias) = if let Some((p, t)) = pair {
            let tol = cfg.f("target_consistency_px");
            if !confirmed(previous_center.replace((t.x, t.y)), (t.x, t.y), tol) {
                attempts += 1;
                if attempts >= cfg.i("detect_retry_limit") {
                    return Err("连续识别到的位置不稳定，已停止".into());
                }
                game.ctl
                    .sleep(g, ms(cfg.f("acquire_poll_ms")).max(game.frame_period));
                window = Some((w, g));
                continue;
            }
            attempts = 0;
            let (axis, mut dist, cross) =
                axis_components(t.x - p.x, t.y - p.y, cfg.f("axis_slope"));
            if dist <= 0.
                || dist < cfg.f("min_jump_dist_ratio") * f.width as f64 * 0.4
                || dist > cfg.f("max_jump_dist_ratio") * f.width as f64
                || cross / dist.abs().max(1.) > cfg.f("axis_max_cross_ratio") + 0.20
            {
                rejects += 1;
                if rejects >= cfg.i("detect_retry_limit") {
                    return Err(format!(
                        "沿轴距离或方向持续异常，已停止。本次距离 {dist:.1}px"
                    ));
                }
                previous_center = None;
                game.ctl.sleep(g, ms(250.));
                window = Some((w, g));
                continue;
            }
            rejects = 0;
            blind = Some(0);
            if cfg.f("low_ratio_trim_ratio") > 0.
                && t.top_width < cfg.f("low_ratio_gate") * t.bbox[2] as f64
            {
                dist *= 1. - cfg.f("low_ratio_trim_ratio")
            }
            let bias = if o.bias_sweep {
                o.bias_levels
                    [records.index as usize / o.bias_per_level.max(1) % o.bias_levels.len()]
            } else {
                0.
            };
            dist *= 1. + bias / 100.;

            let mut hold = if !o.calibrate && cfg.f("transfer_a") > 0.01 {
                (dist - cfg.f("transfer_c")) / cfg.f("transfer_a")
            } else {
                k * dist + cfg.f("b")
            };
            hold *= 1. + cfg.f("press_bias_ratio");
            if adaptive {
                let trim = cfg.f("residual_trim_ratio");
                hold *= 1. + (-0.3_f64 * ema).clamp(-trim, trim)
            }
            hold = hold.clamp(
                cfg.f("press_duration_min_ms"),
                cfg.f("press_duration_max_ms"),
            );
            (Some(dist), axis, hold, bias)
        } else {
            previous_center = None;
            game.vision.reset_prior();
            attempts += 1;
            if attempts < cfg.i("detect_retry_limit") {
                game.ctl.sleep(g, ms(300.));
                window = Some((w, g));
                continue;
            }
            game.debug(
                &format!(
                    "fail_{}.png",
                    chrono::Local::now().format("%Y%m%d_%H%M%S_%f")
                ),
                &f,
            );
            if d.piece.is_none() && cfg.b("piece_skin_shape_enabled") {
                return Err("无法可靠识别人物或皮肤，已停止".into());
            }
            let Some(used) = blind.as_mut().filter(|n| **n < cfg.i("blind_jump_limit")) else {
                return Err("识别重试次数已用尽".into());
            };
            *used += 1;
            attempts = 0;
            let hold = if last_hold > 0. { last_hold } else { k * 270. };
            (
                None,
                (0., 0.),
                hold.clamp(
                    cfg.f("press_duration_min_ms"),
                    cfg.f("press_duration_max_ms"),
                ),
                0.,
            )
        };
        let acquire = acquisition.elapsed().as_secs_f64() * 1000.;
        match game.press(
            &w,
            g,
            (cfg.at("press_point", 0), cfg.at("press_point", 1)),
            hold,
        )? {
            PressOutcome::Sent => (),
            PressOutcome::WindowChanged => continue,
            PressOutcome::Suppressed => {
                window = Some((w, g));
                continue;
            }
        }
        game.vision.reset_prior();
        previous_center = None;
        game.report("landing", json!({"last_hold_ms":hold,"last_dist_px":dist}));
        let land_start = Instant::now();
        let observation = game.settle(&w, g, records.prev, &f);
        let changed = matches!(observation, Ok(None));
        let (mut landed, settle_error) = Landing::after_input(
            observation.and_then(|o| o.ok_or_else(|| "落地观测期间客户区已变化".to_string())),
        );
        if game.ctl.state().stopped && landed.result == "unknown" {
            landed.result = "stopped";
            landed.score = None;
        }
        let landing_ms = (land_start.elapsed().as_secs_f64() * 1000. - landed.score_ms).max(0.);
        let t = pair.map(|(_, t)| t);
        let err = if landed.result == "landed" {
            landed
                .frame
                .as_ref()
                .zip(landed.detect.as_ref())
                .zip(t)
                .and_then(|((after, d), t)| landing::error(&f, after, d, t, cfg, axis))
        } else {
            None
        };
        let dot_error = if let (Some((e, _, "mask")), Some(after), Some(piece)) = (
            err,
            landed.frame.as_ref(),
            landed.detect.as_ref().and_then(|d| d.piece.as_ref()),
        ) {
            if cfg.f("center_dot_max_offset_px") > 0. {
                let anchor = piece.y + cfg.f("piece_anchor_dy_ratio") * after.height as f64;
                match crate::vision::find_dot_near(
                    after,
                    piece.x - e * axis.0,
                    anchor + 18.,
                    cfg.f("center_dot_landing_radius_px"),
                    cfg,
                ) {
                    Ok(dot) => dot.map(|(x, y, _)| (piece.x - x) * axis.0 + (anchor - y) * axis.1),
                    Err(e) => {
                        eprintln!("落地白点读取失败: {e}");
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        };
        if o.save_debug {
            game.debug(
                &format!("jump_{:04}_before.png", records.index + 1),
                &annotated(&f, &d),
            )
        }
        if landed.result == "landed" {
            alive += 1;
            death_streak = 0;
            if dist.is_some() {
                last_hold = hold
            }
            if let (Some((e, width, _)), Some(dist)) = (err, dist) {
                if o.calibrate {
                    calibration.push((hold, dist + e, dist, k))
                }
                if adaptive && e.abs() < width * 0.35 {
                    ema = 0.55 * ema + 0.45 * e / dist.max(1.);
                    let step = cfg.f("max_step_ratio");
                    k = (k * (1. + (-cfg.f("gamma") * ema).clamp(-step, step)))
                        .clamp(base * cfg.f("min_k_ratio"), base * cfg.f("max_k_ratio"));
                }
            }
            if alive > longest {
                longest = alive;
                best_k = k
            }
        } else if landed.result == "dead" {
            alive = 0;
            death_streak += 1;
            if adaptive {
                let factor = if last_error.is_some_and(|e| e > 0.) {
                    0.97
                } else {
                    1.03
                };
                k = (k * factor).clamp(base * cfg.f("min_k_ratio"), base * cfg.f("max_k_ratio"));
                if death_streak >= 3 {
                    k = best_k;
                    death_streak = 0
                }
            }
        }
        let outcome = records.judge(landed.result, landed.score, cfg.i("score_bonus_cap") as i32);
        if o.save_debug || outcome.result == "dead" || outcome.hit == Some(false) {
            let id = chrono::Local::now().format("%Y%m%d_%H%M%S_%f").to_string();
            let tag = if outcome.hit == Some(false) {
                "miss"
            } else {
                outcome.result
            };
            game.debug(&format!("{tag}_{id}_before.png"), &f);
            if let Some(after) = &landed.frame {
                game.debug(&format!("{tag}_{id}_after.png"), after)
            }
        }
        last_error = err.map(|e| e.0);
        let time = game.ctl.status(game.round_start, "playing");
        records.emit(
            outcome,
            k_before,
            k,
            dist,
            hold,
            err,
            t.is_some_and(|t| t.dot.is_some()),
            bias,
            game.vision.backend(),
            time["pause_total_s"].as_f64().unwrap(),
            time["active_elapsed_s"].as_f64().unwrap(),
            [
                acquire,
                landing_ms,
                landed.score_ms,
                acquisition.elapsed().as_secs_f64() * 1000.,
            ],
            cfg.f("center_hit_tolerance_px"),
            t,
            dot_error,
            settle_error
                .as_deref()
                .unwrap_or(if dist.is_none() { "盲跳" } else { "" }),
        )?;
        last_progress = game.ctl.clock();
        warned = false;
        let mut fields = records.fields();
        fields["score"] = json!(outcome.score);
        merge(
            &mut fields,
            json!({"last_index":records.index,"last_result":outcome.result,"k":k,"calibration_samples":calibration.len(),"calibration_band":o.calibrate.then_some(band+1),"jump_uid":format!("{}-{}",records.run_uid,records.index)}),
        );
        game.report(
            if outcome.result == "dead" {
                "dead"
            } else {
                "playing"
            },
            fields,
        );
        if game.ctl.state().stopped {
            break End::Stopped;
        }
        if changed {
            continue;
        }
        if outcome.result == "unknown"
            && game.ctl.guard(g, true).is_ok()
            && Window::find(cfg, (1, 1))?.is_some_and(|fresh| same_window(&w, &fresh))
        {
            return Err(
                settle_error.unwrap_or_else(|| "无法确认本跳是否完成，已停止以免重复按压".into())
            );
        }
        acquisition = Instant::now();
        if records.stalled >= 3 {
            return Err("连续三跳分数没有变化，已停止".into());
        }
        if o.calibrate {
            band_attempts += 1;
            if outcome.result == "landed" {
                band_ok += 1
            }
            if band_ok >= o.calib_per_band.max(1)
                || band_attempts >= o.calib_per_band.max(1).saturating_mul(3)
            {
                band += 1;
                band_ok = 0;
                band_attempts = 0;
                if band == o.calib_k_scales.len() {
                    break End::Finished("标定采样完成");
                }
            }
        }
        if outcome.result == "stopped" {
            break End::Stopped;
        }
        window = Some((w, g));
    };
    if o.calibrate {
        crate::calibration::finish(&calibration, cfg, config_path, &dir, o.persist_calibration)?
    }
    if o.persist_best_k && longest > 0 {
        crate::update_config(config_path, [("k", json!(best_k))])?
    }
    Ok(end)
}
pub(crate) fn run(
    o: &Options,
    cfg: &Config,
    screen: &Screen,
    vision: &mut Vision,
    path: &Path,
) -> Result<()> {
    if (o.calibrate
        && (o.calib_k_scales.is_empty()
            || o.calib_k_scales.iter().any(|v| !v.is_finite() || *v <= 0.)))
        || (o.bias_sweep
            && (o.bias_levels.is_empty()
                || o.bias_levels.iter().any(|v| !v.is_finite() || *v <= -100.)))
    {
        return Err("标定比例或扫描档位无效".into());
    }
    let ctl = Controls::start(cfg.s("pause_hotkey"), o.worker)?;
    let mode = if o.capture == "printwindow" {
        CaptureMode::PrintWindow
    } else {
        CaptureMode::Screen
    };
    let mut game = Game {
        o,
        c: cfg,
        ctl,
        screen,
        vision,
        mode,
        frame_period: refresh_period(),
        round_start: None,
        status: json!({}),
    };
    if o.probe || o.dry_run || o.jump_test {
        return diagnostics(&mut game);
    }
    let mut rounds = 0;
    let mut wait = cfg.f("startup_wait_s");
    let outcome = loop {
        match round(&mut game, path, wait) {
            Ok(End::Stopped) => break Ok(game.ctl.state().stop_reason.unwrap_or("用户停止")),
            Ok(End::Finished(reason)) => {
                rounds += 1;
                if !o.daemon || (o.daemon_max_rounds > 0 && rounds >= o.daemon_max_rounds) {
                    break Ok(reason);
                }
            }
            Ok(End::WindowGone) => {}
            Err(e) => break Err(e),
        }
        if let Some(reason) = game.ctl.state().stop_reason {
            break Ok(reason);
        }
        wait = o.daemon_wait_s;
    };
    if game.ctl.state().stopped {
        game.report("stopping", json!({}))
    }
    game.report(
        "stopped",
        json!({"stop_reason":match &outcome{Ok(s)=>*s,Err(e)=>e.as_str()},"elapsed_running":false}),
    );
    outcome.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_observation_is_recorded_once_before_returning_its_error() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Records::new(dir.path().into());
        r.resync(Some(10));
        let (landing, error) = Landing::after_input(Err("capture failed".into()));
        let o = r.judge(landing.result, landing.score, 32);
        r.emit(
            o,
            2.6,
            2.6,
            Some(30.),
            78.,
            None,
            false,
            0.,
            "cpu",
            0.,
            1.,
            [0.; 4],
            12.,
            None,
            None,
            error.as_deref().unwrap(),
        )?;
        let mut reader = csv::Reader::from_path(&r.path).unwrap();
        let rows = reader
            .records()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(&rows[0][7], "unknown");
        assert!(rows[0][9].contains("capture failed"));
        assert_eq!(r.fields()["deaths"], 0);
        assert_eq!(r.fields()["unknown"], 1);
        Ok(())
    }
    #[test]
    fn late_score_requires_a_baseline_and_cannot_override_game_over() {
        assert_eq!(score_verdict("unknown", Some(10), Some(12)), "landed");
        for previous in [Some(12), None] {
            assert_eq!(score_verdict("unknown", previous, Some(12)), "unknown")
        }
        for known in ["dead", "stopped"] {
            assert_eq!(score_verdict(known, Some(10), Some(12)), known)
        }
    }
    #[test]
    fn moved_window_and_first_target_cannot_confirm_a_press() {
        let w = Window {
            hwnd: 1,
            pid: 2,
            title: "fixture".into(),
            exe: "test".into(),
            client_rect: [10, 10, 565, 1060],
        };
        let mut moved = w.clone();
        moved.client_rect[0] += 1;
        assert!(!same_window(&w, &moved));
        moved = w.clone();
        moved.pid += 1;
        assert!(!same_window(&w, &moved));
        assert!(!confirmed(None, (120., 100.), 8.));
        assert!(!confirmed(Some((110., 100.)), (120., 100.), 8.));
        assert!(confirmed(Some((113., 105.)), (120., 100.), 8.));
    }
    #[test]
    fn cli_dashboard_replaces_complete_json_on_stop() {
        let dir = tempfile::tempdir().unwrap();
        let started = json!({"pid":123,"state":"starting","jumps":0});
        publish_status(dir.path(), &started).unwrap();
        let stopped = json!({"pid":123,"state":"stopped","stop_reason":"fixture stop","control_generation":2,"jumps":1,"elapsed_running":false});
        publish_status(dir.path(), &stopped).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(dir.path().join("status.json")).unwrap())
                .unwrap(),
            stopped
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
