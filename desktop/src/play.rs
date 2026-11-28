use crate::{
    asset_dir,
    control::Controls,
    landing,
    options::Options,
    platform::{CaptureMode, Overlay, PressOutcome, Window},
    records::{same_game_score, Records},
    runtime::{annotated, JumpTiming},
    screen::{CaptureRows, Screen, ScreenState},
    vision::{axis_components, points_close, Detection, Vision},
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
    capturer: crate::platform::Capturer,
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
        let mut wait = self.ctl.wait_clock();
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
            let now = self.ctl.wait_clock();
            let mut reason = "等待用户打开游戏窗口";
            let window = Window::find(self.c, (1, 1))?;
            let missing = window.is_none();
            if !missing {
                wait = now;
            }
            let elapsed = now.saturating_sub(wait);
            if let Some(w) = window {
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
            let timeout = timeout.filter(|_| missing);
            if timeout.is_some_and(|t| elapsed >= t) {
                if self.ctl.guard(g, false).is_err() {
                    continue;
                }
                return if !initial && self.o.daemon {
                    Ok(None)
                } else {
                    Err("等待游戏窗口超时".into())
                };
            }
            self.report("waiting_window",json!({"window":null,"countdown_s":null,"waited_s":elapsed.as_secs_f64(),"stop_reason":reason}));
            let poll = timeout.map_or(ms(200.), |t| ms(200.).min(t.saturating_sub(elapsed)));
            self.ctl.sleep(g, poll);
        }
    }
    fn recognition_frame(&mut self, w: &Window) -> Result<Option<(Frame, CaptureRows)>> {
        self.capturer
            .capture(w, self.mode)?
            .map(|raw| raw.mapped_for_recognition(self.c))
            .transpose()
    }
    // Only consult the window's own pixels after repeated failed screen reads.
    // They can explain a missing pawn, but must never supply a jump target.
    fn hidden_piece_overlay(&mut self, w: &Window, since: Instant) -> Option<Overlay> {
        if self.mode != CaptureMode::Screen || !w.is_foreground() {
            return None;
        }
        let raw = self.capturer.reference_frame(w, since)?;
        // Read the desktop again after PrintWindow; the overlay may have moved.
        let visible = self.capturer.capture(w, CaptureMode::Screen).ok()??;
        let [wx, wy, _, _] = w.client_rect;
        self.vision.reset_prior();
        let owner = hidden_piece_in_frames(self.vision, self.c, &raw, &visible, |[l, t, r, b]| {
            w.overlap([wx + l, wy + t, wx + r, wy + b])
        });
        self.vision.reset_prior();
        let owner = owner?;
        (w.still_current().ok()? && w.is_foreground()).then_some(owner)
    }

    fn jump(&self, w: &Window, g: u64, duration: f64) -> Result<PressOutcome> {
        let point = (self.c.at("press_point", 0), self.c.at("press_point", 1));
        w.jump_with(point, self.o.background, |p| self.press(w, g, p, duration))
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
    fn landing_readout(
        &mut self,
        f: &Frame,
        previous: Option<i32>,
        sampled: Option<&mut Detection>,
    ) -> Result<LandingReadout> {
        let score = match self.screen.state(f, self.c)? {
            state if state.is_result() => return Ok(LandingReadout::Dead),
            ScreenState::Board => self.screen.score(f, self.c),
            ScreenState::UnknownDark => {
                let fresh;
                let detection = if let Some(d) = sampled {
                    self.vision.complete_target(f, d)?;
                    &*d
                } else {
                    fresh = self.vision.detect(f)?;
                    &fresh
                };
                let Some(score) = self.screen.dark_board_score(f, self.c, detection) else {
                    return Ok(LandingReadout::Unclear);
                };
                Some(score)
            }
            _ => return Ok(LandingReadout::Unclear),
        };
        Ok(LandingReadout::board(score, previous))
    }
    fn settle(
        &mut self,
        w: &Window,
        g: u64,
        scores: &Records,
        before: &Frame,
        reference_deadline: Option<Instant>,
    ) -> Result<Option<Landing>> {
        let previous = scores.prev;
        let reference = before.motion_sample(self.c)?;
        let mut departed = false;
        let mut small = None;
        let mut anchor = None;
        let mut stable = 0;
        let mut outcome = Landing::default();
        let start = Instant::now();
        let timeout = sec(self.c.f("settle_timeout_s"));
        let mut deadline = timeout.saturating_mul(2);
        if let Some(until) = reference_deadline {
            deadline = deadline
                .max(self.frame_period)
                .min(until.saturating_duration_since(start));
        }
        let mut last_score = None;
        while start.elapsed() < deadline {
            let retry = start.elapsed() >= timeout;
            if self.ctl.state().stopped {
                outcome.result = "stopped";
                return Ok(Some(outcome));
            }
            if self.ctl.generation() != g {
                if self.status["state"] != "pausing" {
                    self.report("pausing", json!({}))
                }
                if reference_deadline.is_some() || retry || !w.is_foreground() {
                    return Ok(Some(outcome));
                }
            }
            let (frame, _) = match self.recognition_frame(w) {
                Ok(Some(frame)) => frame,
                Ok(None) => return Ok(None),
                Err(reason) => {
                    // A gap breaks consecutive-frame evidence. Only observe until the deadline.
                    outcome.error = Some(reason);
                    small = None;
                    anchor = None;
                    stable = 0;
                    last_score = None;
                    self.ctl.sleep(
                        self.ctl.generation(),
                        ms(200.).min(deadline.saturating_sub(start.elapsed())),
                    );
                    continue;
                }
            };
            outcome.error = None;
            let current = frame.motion_sample(self.c)?;
            let mean_diff = |a: &[u8], b: &[u8]| {
                a.iter()
                    .zip(b)
                    .map(|(a, b)| a.abs_diff(*b) as u64)
                    .sum::<u64>() as f64
                    / a.len() as f64
            };
            departed =
                departed || mean_diff(&reference, &current) > self.c.f("settle_diff_threshold");
            let diff = small.replace(current).map_or(999., |last: Vec<u8>| {
                mean_diff(&last, small.as_ref().unwrap())
            });
            outcome.detect = None;
            if departed && diff < self.c.f("settle_diff_threshold") {
                let d = self.vision.detect_piece_only(&frame)?;
                let p = d.piece.as_ref().map(|p| (p.x, p.y));
                let tol = self.c.f("settle_anchor_tol_px");
                let close = p.zip(anchor).is_some_and(|(a, b)| points_close(a, b, tol));
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
            let supported = stable >= self.c.i("settle_frames").max(1);
            if supported || retry {
                let t = Instant::now();
                let reading =
                    self.landing_readout(&frame, scores.last_known_score, outcome.detect.as_mut())?;
                (outcome.result, outcome.score) =
                    reading.verdict(supported, previous.filter(|_| departed), &mut last_score);
                outcome.score_ms += t.elapsed().as_secs_f64() * 1000.;
            }
            outcome.frame = Some(frame);
            if outcome.result != "unknown" {
                break;
            }
            let poll = if retry {
                ms(100.)
            } else {
                ms(self.c.f("settle_poll_ms"))
            };
            self.ctl.sleep(
                self.ctl.generation(),
                poll.max(self.frame_period)
                    .min(deadline.saturating_sub(start.elapsed())),
            );
        }
        let t = Instant::now();
        if outcome.result == "landed" && previous.is_some() {
            for delay in [self.c.f("score_reread_first_ms"), 250., 250.] {
                if outcome.score.is_some() && outcome.score != previous {
                    break;
                }
                if self.ctl.guard(g, true).is_err() {
                    break;
                }
                self.ctl.sleep(g, ms(delay));
                if self.ctl.guard(g, true).is_err() {
                    break;
                }
                // The landing is already confirmed. A missing HUD sample cannot undo it.
                let (f, _) = match self.recognition_frame(w) {
                    Ok(Some(f)) => f,
                    _ => break,
                };
                match self.landing_readout(&f, scores.last_known_score, None)? {
                    LandingReadout::Dead => {
                        outcome.result = "dead";
                        outcome.score = None;
                        outcome.frame = Some(f);
                        outcome.detect = None;
                        break;
                    }
                    LandingReadout::Board(Some(n)) => outcome.score = Some(n),
                    _ => {}
                }
            }
        }
        if outcome.result == "unknown" && !w.still_current()? {
            return Ok(None);
        }
        outcome.score_ms += t.elapsed().as_secs_f64() * 1000.;
        Ok(Some(outcome))
    }
}
enum LandingReadout {
    Board(Option<i32>),
    Dead,
    Unclear,
}
impl LandingReadout {
    fn board(raw: Option<i32>, previous: Option<i32>) -> Self {
        Self::Board(same_game_score(previous, raw))
    }

    fn verdict(
        self,
        supported: bool,
        takeoff_score: Option<i32>,
        last: &mut Option<i32>,
    ) -> (&'static str, Option<i32>) {
        match self {
            Self::Dead => ("dead", None),
            Self::Unclear => {
                *last = None;
                ("unknown", None)
            }
            Self::Board(score) => {
                let repeated_gain =
                    score == *last && takeoff_score.zip(score).is_some_and(|(old, n)| n > old);
                *last = score.filter(|_| takeoff_score.is_some());
                if supported || repeated_gain {
                    ("landed", score)
                } else {
                    ("unknown", None)
                }
            }
        }
    }
}
struct Landing {
    result: &'static str,
    error: Option<String>,
    score: Option<i32>,
    frame: Option<Frame>,
    detect: Option<Detection>,
    score_ms: f64,
}
impl Landing {
    // The deadline belongs to one in-flight PrintWindow call, across settle passes.
    fn wait_for_reference(
        &mut self,
        pending: bool,
        deadline: &mut Option<Instant>,
        now: Instant,
    ) -> bool {
        if !pending {
            return false;
        }
        let until = deadline.get_or_insert_with(|| now + Duration::from_secs(15));
        if now < *until {
            return true;
        }
        self.error = Some("等待本跳的窗口参考截图超过 15 秒".into());
        false
    }

    fn pause_reason(&self, stalled: i32) -> Option<&str> {
        match self.result {
            "unknown" => Some(
                self.error
                    .as_deref()
                    .unwrap_or("棋子和分数都未能确认本跳结果"),
            ),
            "landed" if stalled >= 3 => Some("连续三跳分数没有变化"),
            _ => None,
        }
    }

    fn after_input(observation: Result<Self>) -> Self {
        observation.unwrap_or_else(|reason| Self {
            error: Some(reason),
            ..Self::default()
        })
    }
}
impl Default for Landing {
    fn default() -> Self {
        Self {
            result: "unknown",
            error: None,
            score: None,
            frame: None,
            detect: None,
            score_ms: 0.,
        }
    }
}

fn occlusion_message(id: u32) -> String {
    format!("游戏画面持续无法识别人物，人物所在区域被悬浮窗遮挡（PID {id}），已停止")
}

fn hidden_piece_in_frames(
    vision: &mut Vision,
    cfg: &Config,
    game: &Frame,
    visible: &Frame,
    covering: impl FnOnce([i32; 4]) -> Option<Overlay>,
) -> Option<Overlay> {
    if (game.width, game.height) != (visible.width, visible.height) {
        return None;
    }
    let (normalized, rows) = game.resized_for_recognition(cfg).ok()?;
    let reference = normalized.as_ref().unwrap_or(game);
    let mut detection = vision.detect_piece_only(reference).ok()?;
    crate::runtime::map_detection_to_capture(
        &mut detection,
        game.width as f64 / reference.width as f64,
        rows,
    );
    let [x, y, width, height] = detection.piece?.bbox;
    let owner = covering([x, y, x + width, y + height])?;
    let changed = (y..y + height).any(|row| {
        let lo = (row as usize * game.width + x as usize) * 3;
        let roi = lo..lo + width as usize * 3;
        game.bgr[roi.clone()] != visible.bgr[roi]
    });
    if !changed {
        return None;
    }
    vision.reset_prior();
    let (normalized, _) = visible.resized_for_recognition(cfg).ok()?;
    vision
        .detect_piece_only(normalized.as_ref().unwrap_or(visible))
        .ok()?
        .piece
        .is_none()
        .then_some(owner)
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
        if frame.is_solid_color()
            || frame.has_dark_center(cfg)
            || matches!(
                game.screen.state(&frame, cfg)?,
                ScreenState::SkinModal | ScreenState::ResultPending
            )
        {
            return Err("当前画面不适合探测点击".into());
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
    let timing = JumpTiming::initial(cfg, o);
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
            let test_press = i == 0 && jump;
            if test_press {
                let first = d.aim();
                if first.is_none()
                    || !game
                        .screen
                        .state(&f, cfg)?
                        .playable(&f, cfg, game.screen, &d)
                {
                    return Err("画面未就绪，取消试跳".into());
                }
                game.ctl
                    .sleep(g, ms(cfg.f("acquire_poll_ms")).max(game.frame_period));
                drop(game.ctl.guard(g, true)?);
                f = raw_frame(&w)?.for_recognition(cfg)?;
                d = game.vision.detect(&f)?;
                if !game
                    .screen
                    .state(&f, cfg)?
                    .playable(&f, cfg, game.screen, &d)
                    || !d.aim().is_some_and(|aim| aim.confirmed(first, cfg))
                {
                    return Err("无法确认新帧目标，取消试跳".into());
                }
            }
            let explicit_ms = (test_press && o.hold_ms > 0.).then_some(o.hold_ms);
            let row = timing.detection(&d, explicit_ms);
            if test_press {
                let duration = row["hold_ms"].as_f64().ok_or("无法确认棋子和目标")?;
                if game.jump(&w, g, duration)? != PressOutcome::Sent {
                    return Err("控制请求已取消试跳".into());
                }
            }
            println!("{row}");
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
    let active_start = game.ctl.clock();
    if window.is_some() {
        game.round_start = Some(active_start)
    }
    let runtime_limit = cfg.f("max_runtime_s");
    let time_is_up = |clock: Duration| {
        runtime_limit > 0. && clock.saturating_sub(active_start) >= sec(runtime_limit)
    };
    let mut k = cfg.f("k");
    let base = k;
    let mut best_k = k;
    let mut longest = 0;
    let mut alive = 0;
    let mut death_streak = 0;
    let mut ema = 0.;
    let mut previous_aim = None;
    let mut attempts = 0;
    let mut rejects = 0;
    let mut missing_under_overlay = None;
    // Only a measured jump that landed can supply a fallback duration.
    let mut blind: Option<(f64, i64)> = None;
    let adaptive = !o.calibrate && cfg.f("gamma") > 0.;
    let mut last_error = None;
    let mut acquisition = Instant::now();
    let mut pressed_button = None;
    let mut calibration = Vec::new();
    let mut band = 0;
    let mut band_ok = 0;
    let mut band_attempts = 0;
    let mut baseline = true;
    let mut score_window = None;
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
        let valid_current = if interrupted {
            false
        } else {
            match window.as_ref() {
                Some((w, _)) => w.still_current()? && (o.background || w.is_foreground()),
                None => false,
            }
        };
        if interrupted || !valid_current {
            missing_under_overlay = None;
            baseline = true;
            previous_aim = None;
            attempts = 0;
            rejects = 0;
            ema = 0.;
            last_error = None;
            let timeout = if interrupted {
                let t = cfg.f("startup_wait_s");
                (t > 0.).then(|| sec(t))
            } else {
                Some(sec(cfg.f("window_wait_s")))
            };
            window = game.gate(false, timeout)?;
            if window.is_none() && !game.ctl.state().stopped {
                break End::WindowGone;
            }
            acquisition = Instant::now();
            last_progress = game.ctl.clock();
            warned = false;
            continue;
        }
        // Taking the snapshot invalidates the plan unless this iteration explicitly keeps it.
        let (w, g) = window.take().unwrap();
        let clock = game.ctl.clock();
        if time_is_up(clock) {
            break End::Finished("达到最长运行时间");
        }
        if !warned && clock.saturating_sub(last_progress) >= sec(90.) {
            eprintln!("已有 90 秒未完成一跳，请检查窗口");
            warned = true;
        }
        let (f, rows) = match game.recognition_frame(&w) {
            Ok(Some(frame)) => frame,
            Ok(None) => continue,
            Err(reason) => {
                missing_under_overlay = None;
                game.report(
                    "capture_blocked",
                    json!({"game_state":"capture_blocked","stop_reason":reason}),
                );
                previous_aim = None;
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
        let session = (w.hwnd, w.pid, g);
        let state = game.screen.state(&f, cfg)?;
        if let ScreenState::Start(p) | ScreenState::Restart(p) = state {
            missing_under_overlay = None;
            previous_aim = None;
            baseline = true;
            game.vision.reset_prior();
            let dead = state.is_result();
            game.report(
                "acquiring",
                json!({"game_state":if dead{"game_over"}else{"start_page"},"stop_reason":""}),
            );
            let button = (session, dead);
            if pressed_button != Some(button) {
                match game.press(&w, g, (p[0] / f.width as f64, rows.fraction(p[1])), 60.)? {
                    PressOutcome::Sent => {
                        pressed_button = Some(button);
                        score_window = None;
                    }
                    PressOutcome::WindowChanged | PressOutcome::Suppressed => {
                        // Re-read the visible button. Only the outer loop can tell
                        // whether the game window or its foreground status changed.
                        game.ctl.sleep(g, ms(250.));
                        window = Some((w, g));
                        continue;
                    }
                }
            }
            attempts += 1;
            if attempts >= cfg.i("detect_retry_limit") {
                return Err("开始/重玩按钮持续未消失".into());
            }
            game.ctl.sleep(g, ms(1400.));
            window = Some((w, g));
            continue;
        }
        let detection = match state {
            ScreenState::Board | ScreenState::UnknownDark => Some(game.vision.detect(&f)?),
            _ => None,
        };
        let overlay = detection
            .as_ref()
            .filter(|d| game.mode == CaptureMode::Screen && d.aim().is_none())
            .and_then(|_| w.overlap(w.client_rect));
        if let Some(overlay) = overlay {
            // An overlap alone cannot block a readable frame. Retry an unreadable
            // board without input, including the blind-jump fallback.
            if matches!(overlay, Overlay::Other(_))
                && detection.as_ref().is_some_and(|d| d.piece.is_none())
            {
                let (since, tries) = missing_under_overlay.get_or_insert((Instant::now(), 0_i64));
                let limit = cfg.i("detect_retry_limit");
                *tries = (*tries + 1).min(limit);
                if *tries >= limit {
                    if let Some(Overlay::Other(id)) = game.hidden_piece_overlay(&w, *since) {
                        if game.ctl.guard(g, true).is_ok() {
                            return Err(occlusion_message(id));
                        }
                    }
                    if !game.capturer.awaiting_reference(*since) {
                        missing_under_overlay = None;
                    }
                }
            } else {
                missing_under_overlay = None;
            }
            previous_aim = None;
            attempts = 0;
            rejects = 0;
            blind = None;
            baseline = true;
            game.vision.reset_prior();
            game.report(
                "acquiring",
                json!({"game_state":"acquiring","stop_reason":""}),
            );
            game.ctl.sleep(g, ms(300.));
            window = Some((w, g));
            continue;
        }
        missing_under_overlay = None;
        let detection = detection.filter(|d| state.playable(&f, cfg, game.screen, d));
        let Some(d) = detection else {
            let (phase, reason) = match state {
                ScreenState::ResultPending => ("result_pending", "正在结算，等待重玩按钮出现。"),
                ScreenState::SkinModal => ("blocked_modal", "请在游戏中关闭获得新皮肤提示。"),
                _ => (
                    "unrecognized_dark",
                    "画面较暗，尚无法确认棋子与平台，等待重新识别。",
                ),
            };
            if state == ScreenState::UnknownDark && game.status["state"] != phase {
                game.debug("last_unrecognized_dark.png", &f);
            }
            game.report(phase, json!({"game_state":phase,"stop_reason":reason}));
            pressed_button = None;
            attempts = 0;
            previous_aim = None;
            baseline = true;
            game.vision.reset_prior();
            game.ctl.sleep(g, ms(300.));
            window = Some((w, g));
            continue;
        };
        pressed_button = None;
        if baseline {
            blind = None;
            let score = game.screen.score(&f, cfg);
            if score_window.replace(session) == Some(session) {
                records.resync_same_game(score);
            } else {
                records.resync(score);
            }
            baseline = false
        }
        game.report("acquiring",json!({"piece_source":d.piece.as_ref().map(|p|p.source),"piece_shape_score":d.piece.as_ref().map(|p|p.shape_score),"game_state":"playing","stop_reason":"","score":records.prev}));
        let timing = JumpTiming::new(cfg, o, k, band, records.index);
        k = timing.k;
        let k_before = k;
        let pair = d.aim().zip(d.target.as_ref());
        let (dist, axis, hold, bias) = if let Some((aim, t)) = pair {
            if !aim.confirmed(previous_aim.replace(aim), cfg) {
                attempts += 1;
                if attempts >= cfg.i("detect_retry_limit") {
                    if game.mode != CaptureMode::Screen || w.overlap(w.client_rect).is_none() {
                        return Err("连续识别到的位置不稳定，已停止".into());
                    }
                    attempts = 0;
                }
                game.ctl
                    .sleep(g, ms(cfg.f("acquire_poll_ms")).max(game.frame_period));
                window = Some((w, g));
                continue;
            }
            attempts = 0;
            let (axis, dist, cross) =
                axis_components(t.x - aim.piece.0, t.y - aim.piece.1, cfg.f("axis_slope"));
            if dist <= 0.
                || dist < cfg.f("min_jump_dist_ratio") * f.width as f64 * 0.4
                || dist > cfg.f("max_jump_dist_ratio") * f.width as f64
                || cross / dist.abs().max(1.) > cfg.f("axis_max_cross_ratio") + 0.20
            {
                rejects += 1;
                if rejects >= cfg.i("detect_retry_limit") {
                    if game.mode != CaptureMode::Screen || w.overlap(w.client_rect).is_none() {
                        return Err(format!(
                            "沿轴距离或方向持续异常，已停止。本次距离 {dist:.1}px"
                        ));
                    }
                    rejects = 0;
                }
                previous_aim = None;
                game.ctl.sleep(g, ms(250.));
                window = Some((w, g));
                continue;
            }
            rejects = 0;
            let (timed_dist, hold) = timing.distance_and_hold(t, dist, ema);
            (Some(timed_dist), axis, hold, timing.bias_pct)
        } else {
            previous_aim = None;
            if d.unresolved_target_shadow {
                blind = None;
            }
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
            let Some((hold, used)) = blind
                .as_mut()
                .filter(|(_, n)| *n < cfg.i("blind_jump_limit"))
            else {
                return Err("识别重试次数已用尽".into());
            };
            *used += 1;
            attempts = 0;
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
        match game.jump(&w, g, hold)? {
            PressOutcome::Sent => (),
            PressOutcome::WindowChanged => {
                // Reacquire the aim if a toolbar intercepted every press point.
                // Only an actual window/focus change needs the startup countdown.
                previous_aim = None;
                game.ctl.sleep(g, ms(250.));
                window = Some((w, g));
                continue;
            }
            PressOutcome::Suppressed => {
                window = Some((w, g));
                continue;
            }
        }
        game.vision.reset_prior();
        previous_aim = None;
        game.report("landing", json!({"last_hold_ms":hold,"last_dist_px":dist}));
        let land_start = Instant::now();
        let mut observation = game.settle(&w, g, &records, &f, None);
        let mut hidden = None;
        let mut previous_cover = None;
        let mut reference_deadline = None;
        while let Ok(Some(landing)) = &mut observation {
            if landing.result != "unknown"
                || game.mode != CaptureMode::Screen
                || game.ctl.guard(g, true).is_err()
                || !w.is_foreground()
                || time_is_up(game.ctl.clock())
                || w.overlap(w.client_rect).is_none()
            {
                break;
            }
            match game.hidden_piece_overlay(&w, land_start) {
                Some(Overlay::Allowed) => {
                    previous_cover = None;
                    reference_deadline = None;
                }
                Some(Overlay::Other(id)) if previous_cover == Some(id) => {
                    let reason = occlusion_message(id);
                    landing.error = Some(reason.clone());
                    hidden = Some(reason);
                    break;
                }
                Some(Overlay::Other(id)) => {
                    previous_cover = Some(id);
                    reference_deadline = None;
                }
                None => {
                    if !landing.wait_for_reference(
                        game.capturer.awaiting_reference(land_start),
                        &mut reference_deadline,
                        Instant::now(),
                    ) {
                        break;
                    }
                }
            }
            observation = game.settle(&w, g, &records, &f, reference_deadline);
        }
        let changed = matches!(observation, Ok(None));
        let mut landed = Landing::after_input(
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
        blind = match (landed.result, dist) {
            ("landed", Some(_)) => Some((hold, 0)),
            ("landed", None) => blind,
            _ => None,
        };
        if landed.result == "landed" {
            alive += 1;
            death_streak = 0;
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
        let unresolved = outcome.result == "unknown" && game.ctl.guard(g, true).is_ok();
        if o.save_debug || outcome.result == "dead" || outcome.hit == Some(false) || unresolved {
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
            landed
                .error
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
        if time_is_up(game.ctl.clock()) && !game.ctl.state().pause {
            break End::Finished("达到最长运行时间");
        }
        if let Some(reason) = hidden {
            if game.ctl.guard(g, true).is_ok() {
                return Err(reason);
            }
        }
        if let Some(reason) = landed.pause_reason(records.stalled) {
            if (!changed || w.same_instance_exists()) && game.ctl.pause_if_current(g, reason) {
                let _ = writeln!(
                    std::io::stdout().lock(),
                    "已暂停：{}。确认游戏画面后按 {} 恢复。",
                    reason.replace(['\r', '\n'], " "),
                    ctl_key
                );
                game.report("pausing", json!({}));
                window = Some((w, g));
                continue;
            }
        }
        if changed {
            continue;
        }
        acquisition = Instant::now();
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
        capturer: Default::default(),
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
    fn pending_landing_reference_survives_multiple_observations_but_has_a_deadline() {
        let mut landing = Landing::default();
        let mut deadline = None;
        let start = Instant::now();
        let at = |millis| start + Duration::from_millis(millis);

        for elapsed in [0, 5200, 8000, 10400] {
            assert!(landing.wait_for_reference(true, &mut deadline, at(elapsed)));
            assert_eq!(deadline, Some(at(15000)));
        }
        // A completed read without evidence of a covered pawn ends observation.
        assert!(!landing.wait_for_reference(false, &mut deadline, at(10401)));
        assert!(landing.error.is_none());
        assert!(!landing.wait_for_reference(true, &mut deadline, at(15000)));
        assert_eq!(
            landing.pause_reason(0),
            Some("等待本跳的窗口参考截图超过 15 秒")
        );
    }

    #[test]
    fn overlay_requires_a_missing_visible_pawn_and_changed_pixels_under_the_cover() {
        let cfg = Config::default();
        let mut vision = Vision::new(&cfg, "cpu").unwrap();
        let game = Frame::read(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/skin-gold-play.png"),
        )
        .unwrap();
        let piece = vision.detect_piece_only(&game).unwrap().piece.unwrap();
        let [x, y, w, h] = piece.bbox;
        let mut visible = game.clone();
        let mut check = |screen: &Frame, pid| {
            vision.reset_prior();
            hidden_piece_in_frames(&mut vision, &cfg, &game, screen, |bounds| {
                assert_eq!(bounds, [x, y, x + w, y + h]);
                pid
            })
        };
        assert_eq!(check(&visible, Some(Overlay::Other(7))), None); // transparent shared-screen overlay
        visible.bgr[(y as usize * game.width + x as usize) * 3] ^= 1;
        assert_eq!(check(&visible, Some(Overlay::Other(7))), None); // different pixels, pawn still recognizable
        for row in y - 8..y + h + 8 {
            let lo = (row as usize * game.width + (x - 8) as usize) * 3;
            visible.bgr[lo..lo + (w + 16) as usize * 3].fill(251);
        }
        assert_eq!(
            check(&visible, Some(Overlay::Allowed)),
            Some(Overlay::Allowed)
        );
        assert_eq!(check(&visible, None), None); // no external window over the missing pawn
        assert_eq!(
            check(&visible, Some(Overlay::Other(7))),
            Some(Overlay::Other(7))
        );
        assert_eq!(check(&game, Some(Overlay::Other(7))), None); // restored before final confirmation
    }
    #[test]
    fn stalled_and_unconfirmed_jumps_are_recorded_before_requesting_pause() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Records::new(dir.path().into());
        r.resync(Some(10));
        let landed = Landing {
            result: "landed",
            score: Some(10),
            ..Landing::default()
        };
        let failed = Landing::after_input(Err("capture failed".into()));
        for (landing, expected) in [
            (&landed, None),
            (&landed, None),
            (&landed, Some("连续三跳分数没有变化")),
            (&failed, Some("capture failed")),
        ] {
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
                landing.error.as_deref().unwrap_or(""),
            )?;
            assert_eq!(landing.pause_reason(r.stalled), expected);
            if r.index == 3 {
                assert_eq!(r.fields()["landed"], 3);
                r.resync(Some(0)); // Resume after the player starts a fresh game.
                assert_eq!(landed.pause_reason(r.stalled), None);
                assert_eq!(r.prev, Some(0));
            }
        }
        let mut reader = csv::Reader::from_path(&r.path).unwrap();
        let rows = reader
            .records()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rows.len(), 4);
        assert_eq!((&rows[2][7], &rows[2][8]), ("landed", "10"));
        assert_eq!(&rows[3][7], "unknown");
        assert!(rows[3][9].contains("capture failed"));
        let stats = r.fields();
        assert_eq!(stats["landed"], 3);
        assert_eq!(stats["deaths"], 0);
        assert_eq!(stats["judged"], 0);
        assert_eq!(stats["unknown"], 4);
        Ok(())
    }
    #[test]
    fn late_hud_requires_consecutive_gains_or_a_supported_landing() {
        let previous = Some(467);
        let mut last = None;
        for _ in 0..2 {
            assert_eq!(
                LandingReadout::board(Some(499), previous).verdict(false, None, &mut last),
                ("unknown", None)
            );
        }
        assert_eq!(
            LandingReadout::board(Some(499), previous).verdict(false, previous, &mut last),
            ("unknown", None)
        );
        let mut read = |raw, supported| {
            LandingReadout::board(raw, previous).verdict(supported, previous, &mut last)
        };
        for raw in [Some(8), Some(467), None, Some(499), Some(8), Some(499)] {
            assert_eq!(read(raw, false), ("unknown", None));
        }
        assert_eq!(read(Some(499), false), ("landed", Some(499)));
        assert_eq!(read(Some(8), true), ("landed", None));
        assert_eq!(
            LandingReadout::Unclear.verdict(true, previous, &mut last),
            ("unknown", None)
        );
        assert_eq!(
            LandingReadout::Dead.verdict(true, previous, &mut last),
            ("dead", None)
        );
        for _ in 0..2 {
            assert_eq!(
                LandingReadout::board(Some(8), None).verdict(false, None, &mut last),
                ("unknown", None)
            );
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
        use crate::vision::Aim;
        let cfg = Config(json!({"target_consistency_px":8.0,"settle_anchor_tol_px":3.0}));
        let frame = |piece, target| Aim { piece, target };
        let current = frame((40., 150.), (120., 100.));
        assert!(!current.confirmed(None, &cfg));
        assert!(!current.confirmed(Some(frame((40., 150.), (110., 100.))), &cfg));
        assert!(!current.confirmed(Some(frame((40., 156.), (120., 100.))), &cfg));
        assert!(current.confirmed(Some(frame((42., 149.), (113., 105.))), &cfg));
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
