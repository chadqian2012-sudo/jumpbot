use serde_json::Value;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub along: f32,
    pub lift: f32,
    pub squash: f32,
    pub camera: f32,
    pub target_tile: i32,
}

impl Default for Pose {
    fn default() -> Self {
        Self {
            along: 0.0,
            lift: 0.0,
            squash: 0.0,
            camera: 0.0,
            target_tile: 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Jump {
    pub started: Instant,
    from_pawn: f32,
    from_lift: f32,
    from_squash: f32,
    from_cam: f32,
    pub to: i32,
    pub bullseye: bool,
}

impl Jump {
    pub const FLIGHT_DURATION: Duration = Duration::from_millis(340);
    pub const TOTAL_DURATION: Duration = Duration::from_millis(780);

    pub fn next(previous: Option<Self>, now: Instant) -> Self {
        match previous {
            None => Self {
                started: now,
                from_pawn: 0.0,
                from_lift: 0.0,
                from_squash: 0.0,
                from_cam: 0.0,
                to: 1,
                bullseye: false,
            },
            Some(prev) => {
                let p = prev.pose(now);
                Self {
                    started: now,
                    from_pawn: p.camera + p.along,
                    from_lift: p.lift,
                    from_squash: p.squash,
                    from_cam: p.camera,
                    to: prev.to + 1,
                    bullseye: false,
                }
            }
        }
    }

    #[cfg(test)]
    pub fn with_bullseye(mut self, bullseye: bool) -> Self {
        self.bullseye = bullseye;
        self
    }

    pub fn flight_duration(self) -> Duration {
        Self::FLIGHT_DURATION
    }

    pub fn duration(self) -> Duration {
        Self::TOTAL_DURATION
    }

    pub fn rest(self) -> Pose {
        Pose {
            along: 0.0,
            lift: 0.0,
            squash: 0.0,
            camera: self.to as f32,
            target_tile: self.to + 1,
        }
    }

    pub fn pose(self, now: Instant) -> Pose {
        let elapsed = now.saturating_duration_since(self.started).as_secs_f32();
        let total = Self::TOTAL_DURATION.as_secs_f32();
        if elapsed >= total {
            return self.rest();
        }
        let flight_s = Self::FLIGHT_DURATION.as_secs_f32();
        let pan_s = total - flight_s;
        let f = (elapsed / flight_s).min(1.0);
        let u = f * f * (3.0 - 2.0 * f);
        let air = 4.0 * u * (1.0 - u);
        let to_f = self.to as f32;
        let dist = (to_f - self.from_pawn).abs().min(1.5);
        let pawn_world = self.from_pawn + (to_f - self.from_pawn) * u;
        let lift = self.from_lift * (1.0 - u) + (0.31 - self.from_lift).max(0.0) * dist * air;

        // Foot-anchored landing squash right upon touchdown.
        let s = ((elapsed - flight_s) / 0.14).clamp(0.0, 1.0);
        let contact = 4.0 * s * (1.0 - s);
        let squash = self.from_squash * (1.0 - u) + contact * contact;

        // Camera smoothly follows immediately upon landing so the ground recedes beneath the pawn.
        let base_cam = (self.to - 1) as f32;
        let camera = if elapsed < flight_s {
            self.from_cam + (base_cam - self.from_cam) * u
        } else {
            let pan_t = ((elapsed - flight_s) / pan_s).clamp(0.0, 1.0);
            let s = pan_t * pan_t * (3.0 - 2.0 * pan_t);
            let ease = 1.0 - (1.0 - s) * (1.0 - s);
            base_cam + ease
        };
        let target_tile = if elapsed < flight_s {
            self.to
        } else {
            self.to + 1
        };
        Pose {
            along: pawn_world - camera,
            lift,
            squash,
            camera,
            target_tile,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Idle,
    Live,
    Selftest,
    Stopping,
}
pub struct Console {
    pub ready: bool,
    pub mode: Mode,
    pub phase: String,
    pub notice: String,
    pub status: Value,
    pub pending: Option<u64>,
    pub observed: Instant,
    pub logs: VecDeque<String>,
    pub log_sequence: u64,
    pub landed: Option<Jump>,
}
const LANDING_RETRY_NOTE: &str = "本次按压已释放，只读取游戏画面，不会重复按压。";
const LANDING_PAUSE_NOTE: &str = "请查看游戏后再恢复。";

impl Console {
    pub fn new(error: Option<String>) -> Self {
        Self {
            ready: error.is_none(),
            mode: Mode::Idle,
            phase: "idle".into(),
            notice: error.unwrap_or_default(),
            status: Value::Null,
            pending: None,
            observed: Instant::now(),
            logs: VecDeque::new(),
            log_sequence: 0,
            landed: None,
        }
    }
    pub fn started(&mut self, test: bool) {
        *self = Self::new(None);
        self.phase = if test { "selftest" } else { "starting" }.into();
        self.mode = if test { Mode::Selftest } else { Mode::Live };
    }
    pub fn primary_enabled(&self) -> bool {
        match self.mode {
            Mode::Idle => self.ready,
            Mode::Live => {
                self.pending.is_none()
                    && self.status["hotkey_registered"] == true
                    && self.status["control_generation"].is_u64()
                    && !matches!(
                        self.phase.as_str(),
                        "starting" | "stopped" | "stopping" | "pausing" | "resuming"
                    )
            }
            _ => false,
        }
    }
    pub fn is_charging(&self) -> bool {
        self.mode == Mode::Live && matches!(self.phase.as_str(), "playing" | "acquiring")
    }
    pub fn primary_label(&self) -> &str {
        match (self.mode, self.phase.as_str()) {
            (Mode::Idle, _) => "启动脚本",
            (_, "paused") => "恢复运行",
            _ => "暂停运行",
        }
    }
    pub fn update(&mut self, s: Value) {
        self.observed = Instant::now();
        if self.pending != s["control_generation"].as_u64() {
            self.pending = None;
        }
        if s["hotkey_error"].as_str().is_some_and(|v| !v.is_empty())
            && s["hotkey_error"] != self.status["hotkey_error"]
        {
            self.notice = "暂停快捷键无法注册。请停止脚本，释放快捷键后再启动。".into();
        }
        if matches!(self.mode, Mode::Live | Mode::Stopping) {
            let phase = s["state"].as_str().unwrap_or("starting");
            let state_changed = s["state"] != self.status["state"];
            if phase == "paused"
                && (state_changed || s["control_generation"] != self.status["control_generation"])
            {
                match s["landing_pause_reason"].as_str() {
                    Some(reason) => self.log(format!("已暂停。{reason}。{LANDING_PAUSE_NOTE}")),
                    None => self.log("已暂停，可以切换窗口。".into()),
                }
            }
            if state_changed || s["stop_reason"] != self.status["stop_reason"] {
                let reason = s["stop_reason"].as_str().unwrap_or("");
                match phase {
                    "waiting_window" | "capture_blocked" | "blocked_modal" | "result_pending"
                    | "unrecognized_dark"
                        if !reason.is_empty() =>
                    {
                        self.log(reason.to_owned())
                    }
                    "landing_retry" if state_changed => {
                        self.log(format!("正在复核落点。{LANDING_RETRY_NOTE}"))
                    }
                    "resuming" => self.log("正在重新获取游戏窗口。".into()),
                    "countdown" => self.log(format!(
                        "已找到游戏窗口，准备开始。进程 {}",
                        s["window"]["pid"]
                    )),
                    _ => {}
                }
            }
            if s["mask_backend"] != self.status["mask_backend"] {
                if let Some(backend) = s["mask_backend"].as_str().filter(|b| *b != "pending") {
                    self.log(format!("本轮实际识别后端：{backend}"));
                }
            }
            if s["jump_uid"].is_string() && s["jump_uid"] != self.status["jump_uid"] {
                let prev_score = self.status["score"].as_i64();
                let next_score = s["score"].as_i64();
                let score = next_score.map_or_else(|| "待判定".to_owned(), |v| v.to_string());
                let result = match s["last_result"].as_str() {
                    Some("landed") => {
                        if let (Some(prev), Some(next)) = (prev_score, next_score) {
                            if next - prev >= 2 {
                                if let Some(j) = self.landed.as_mut() {
                                    j.bullseye = true;
                                }
                            }
                        }
                        "已落地"
                    }
                    Some("dead") => "本局结束",
                    _ => "落点待判定",
                };
                self.log(format!(
                    "第 {} 跳　{}　分数 {}　蓄力 {:.0} ms",
                    s["last_index"],
                    result,
                    score,
                    s["last_hold_ms"].as_f64().unwrap_or_default()
                ));
            }
            if self.mode == Mode::Live && phase != self.phase {
                if phase == "landing" {
                    self.landed = Some(Jump::next(self.landed, self.observed));
                }
                self.phase = phase.into();
            }
        }
        self.status = s;
    }
    pub fn log(&mut self, s: String) {
        if s.is_empty() || self.logs.back() == Some(&s) {
            return;
        }
        if self.logs.len() == 60 {
            self.logs.pop_front();
        }
        self.logs.push_back(s);
        self.log_sequence += 1;
    }
    pub fn elapsed(&self, now: Instant) -> Option<f64> {
        self.status["active_elapsed_s"].as_f64().map(|base| {
            base + if self.status["elapsed_running"] == true {
                now.saturating_duration_since(self.observed).as_secs_f64()
            } else {
                0.
            }
        })
    }
    pub fn exited(&mut self, test: bool, ok: bool, code: Option<i32>, stderr: Option<&str>) {
        self.mode = Mode::Idle;
        self.pending = None;
        if let Some(v) = self.elapsed(Instant::now()) {
            self.status["active_elapsed_s"] = v.into();
        }
        if self.status.is_object() {
            self.status["elapsed_running"] = false.into();
        }
        self.phase = if test { "idle" } else { "stopped" }.into();
        if test {
            self.notice = if ok {
                "CPU 无输入自检通过。"
            } else {
                "自检失败。请查看运行日志。"
            }
            .into();
        } else {
            let reason = self.status["stop_reason"]
                .as_str()
                .filter(|s| self.status["state"] == "stopped" && !s.is_empty());
            if let (true, Some(reason)) = (ok, reason) {
                self.notice.clear();
                self.log(reason.to_owned());
            } else {
                let code = code.map_or_else(|| "未知".into(), |n| n.to_string());
                self.notice = match (reason, stderr) {
                    (None, Some(line)) if !ok => {
                        format!("脚本退出（代码 {code}），最后一条错误输出：{line}")
                    }
                    _ => format!(
                        "脚本退出（代码 {code}）：{}",
                        reason.unwrap_or("未收到停止原因。")
                    ),
                };
                self.log(self.notice.clone());
            }
        }
    }
    pub fn labels(&self) -> (&str, &str) {
        match self.phase.as_str() {
            "idle" => ("待机", "启动后会等待你手动打开小游戏。"),
            "waiting_window" => ("等待游戏窗口", "请在微信里手动打开「跳一跳」。"),
            "countdown" => ("启动倒计时", "倒计时结束后开始识别画面。"),
            "resuming" => ("正在恢复", "重新查找窗口并采集新画面。"),
            "paused" if self.status["landing_pause_reason"].is_string() => {
                ("已暂停", LANDING_PAUSE_NOTE)
            }
            "paused" => ("已暂停", "现在可以切换窗口。"),
            "pausing" => ("正在暂停", "正在等待当前按压结束。"),
            "playing" => ("运行中", "正在识别棋子与目标。"),
            "acquiring" => ("正在识别", "正在寻找可用的游戏画面。"),
            "landing" => ("正在核对落点", "本次按压已释放，正在读取落点。"),
            "landing_retry" => ("正在复核落点", LANDING_RETRY_NOTE),
            "capture_blocked" => (
                "无法读取画面",
                self.status["stop_reason"]
                    .as_str()
                    .unwrap_or("请检查游戏窗口是否完整可见。"),
            ),
            "blocked_modal" => ("皮肤提示", "请在游戏中关闭获得新皮肤提示。"),
            "result_pending" => ("等待重玩按钮", "本局已结束，按钮出现后会继续。"),
            "unrecognized_dark" => (
                "正在核对暗色画面",
                "尚未确认可跳跃的目标，正在重新读取画面。",
            ),
            "dead" => ("本局结束", "正在处理游戏结束画面。"),
            "stopped" if self.mode == Mode::Idle && !self.notice.is_empty() => {
                ("已停止", &self.notice)
            }
            "stopped" => (
                "已停止",
                self.status["stop_reason"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("可启动下一轮。"),
            ),
            "stopping" => ("正在停止", "等待脚本释放鼠标并退出。"),
            "selftest" => ("正在自检", "自检使用合成画面。"),
            _ => ("正在启动", "正在加载配置与识别后端。"),
        }
    }
    pub fn window(&self) -> String {
        let w = &self.status["window"];
        let Some(title) = w["title"].as_str().filter(|_| {
            !matches!(
                self.phase.as_str(),
                "starting" | "waiting_window" | "stopped"
            )
        }) else {
            return "窗口未连接".into();
        };
        if let Some([x, y, r, b]) = w["client"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_i64).collect::<Vec<_>>())
            .as_deref()
        {
            format!("{title}  /  {} × {}", r - x, b - y)
        } else {
            title.into()
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn forward_jump_recedes_ground_and_retargeting_preserves_the_pose() {
        let now = Instant::now();
        let first = Jump::next(None, now);
        let mid_flight = first.pose(now + first.flight_duration().mul_f32(0.5));
        assert!(mid_flight.lift > 0.2);
        assert!(mid_flight.along > 0.4 && mid_flight.along < 0.6);
        assert_eq!(mid_flight.camera, 0.0);

        // Immediately after touchdown, camera advances so ground recedes back toward 0.
        let mid_pan = first.pose(now + first.flight_duration() + Duration::from_millis(160));
        assert!(mid_pan.camera > 0.0 && mid_pan.camera < 1.0);
        assert!(mid_pan.along > 0.0 && mid_pan.along < 1.0);
        assert_eq!(mid_pan.target_tile, 2);

        let rested = first.pose(now + first.duration());
        assert_eq!(rested, first.rest());
        assert_eq!(rested.along, 0.0);
        assert_eq!(rested.camera, 1.0);

        // Next jump always advances forward (never backwards).
        let second = Jump::next(Some(first), now + first.duration()).with_bullseye(true);
        assert!(second.bullseye);
        assert_eq!(second.to, 2);
        let second_mid = second.pose(second.started + second.flight_duration().mul_f32(0.5));
        assert!(second_mid.along > 0.4 && second_mid.lift > 0.2);

        let mut c = Console::new(None);
        assert!(!c.is_charging());
        c.started(false);
        c.update(json!({"state": "playing", "score": 2}));
        assert!(c.is_charging());
        c.update(json!({"state": "landing", "score": 2}));
        assert!(!c.is_charging());
        c.update(json!({"state": "playing", "jump_uid": "j-1", "last_index": 1, "last_result": "landed", "last_hold_ms": 420.0, "score": 4}));
        assert!(c.landed.is_some_and(|j| j.bullseye));

        for fraction in [0.25, 0.7] {
            let interrupt = now + first.duration().mul_f32(fraction);
            let turn = Jump::next(Some(first), interrupt);
            let before = first.pose(interrupt);
            let after = turn.pose(interrupt);
            assert_eq!(
                (after.along, after.lift, after.squash, after.camera),
                (before.along, before.lift, before.squash, before.camera)
            );
            assert_eq!(after.target_tile, turn.to);
            assert_eq!(turn.pose(interrupt + turn.duration()), turn.rest());
        }
    }
    #[test]
    fn landing_retry_keeps_controls_and_only_logs_the_transition() {
        let mut s = Console::new(None);
        s.started(false);
        let mut packet = json!({"state":"landing","hotkey_registered":true,"control_generation":0});
        s.update(packet.clone());
        let jump = s.landed;
        packet["state"] = "landing_retry".into();
        s.update(packet.clone());
        assert_eq!(s.labels(), ("正在复核落点", LANDING_RETRY_NOTE));
        assert!(s.primary_enabled());
        assert_eq!(s.primary_label(), "暂停运行");
        assert!(!s.is_charging());
        s.log("其他事件".into());
        packet["stop_reason"] = "采集异常".into();
        s.update(packet.clone());
        assert_eq!(s.log_sequence, 2);
        assert_eq!(s.landed, jump);

        packet["state"] = "paused".into();
        packet["control_generation"] = 1.into();
        packet["landing_pause_reason"] = "落点仍未确认".into();
        s.update(packet.clone());
        assert_eq!(s.labels(), ("已暂停", LANDING_PAUSE_NOTE));
        assert_eq!(
            s.logs.back().unwrap(),
            "已暂停。落点仍未确认。请查看游戏后再恢复。"
        );
        assert!(s.primary_enabled());
        assert_eq!(s.primary_label(), "恢复运行");
        s.update(packet.clone());
        assert_eq!(s.log_sequence, 3);

        // Resume and a new manual pause can arrive between two UI updates.
        packet["control_generation"] = 3.into();
        packet["landing_pause_reason"] = Value::Null;
        s.update(packet);
        assert_eq!(s.labels(), ("已暂停", "现在可以切换窗口。"));
        assert_eq!(s.logs.back().unwrap(), "已暂停，可以切换窗口。");
    }
    #[test]
    fn generation_conflict_and_stop_gate_controls() {
        let mut s = Console::new(None);
        s.started(false);
        let mut status = json!({"state":"paused","hotkey_registered":false,"hotkey_error":"conflict","control_generation":3});
        s.update(status.clone());
        assert!(!s.primary_enabled());
        assert!(!s.notice.is_empty());
        status["hotkey_registered"] = true.into();
        status["hotkey_error"] = Value::Null;
        s.update(status.clone());
        assert!(s.primary_enabled());
        s.pending = Some(3);
        s.update(status.clone());
        assert!(!s.primary_enabled());
        status["control_generation"] = 4.into();
        status["state"] = "resuming".into();
        s.update(status.clone());
        assert!(!s.primary_enabled());
        status["state"] = "waiting_window".into();
        s.update(status.clone());
        assert!(s.primary_enabled());
        s.mode = Mode::Stopping;
        s.phase = "stopping".into();
        s.update(status);
        assert_eq!(s.phase, "stopping");
        assert!(!s.primary_enabled());
    }
    #[test]
    fn stop_drains_the_last_jump_before_logging_the_reason_once() {
        let mut s = Console::new(None);
        s.started(false);
        s.mode = Mode::Stopping;
        s.phase = "stopping".into();
        let mut packet = json!({"state":"playing","jump_uid":"round-3","last_index":3,
            "last_result":"landed","last_hold_ms":505.2,"score":12});
        s.update(packet.clone());
        assert_eq!(s.phase, "stopping");
        packet["state"] = "stopped".into();
        packet["stop_reason"] = "控制端要求停止".into();
        s.update(packet.clone());
        s.update(packet);
        s.exited(false, true, Some(0), None);
        assert_eq!(
            s.logs.into_iter().collect::<Vec<_>>(),
            ["第 3 跳　已落地　分数 12　蓄力 505 ms", "控制端要求停止"]
        );
    }

    #[test]
    fn abnormal_exit_prefers_the_terminal_reason_then_stderr() {
        for (state, reason, stderr, expected) in [
            (
                "stopped",
                "截图失败",
                Some("诊断信息"),
                "脚本退出（代码 7）：截图失败",
            ),
            (
                "capture_blocked",
                "暂时遮挡",
                Some("配置损坏"),
                "脚本退出（代码 7），最后一条错误输出：配置损坏",
            ),
            (
                "capture_blocked",
                "暂时遮挡",
                None,
                "脚本退出（代码 7）：未收到停止原因。",
            ),
        ] {
            let mut s = Console::new(None);
            s.started(false);
            s.update(json!({"state":state,"stop_reason":reason}));
            s.exited(false, false, Some(7), stderr);
            assert_eq!(s.logs.back().unwrap(), expected);
            assert_eq!(s.labels(), ("已停止", expected));
        }
    }
    #[test]
    fn elapsed_freezes_and_log_is_bounded() {
        let mut s = Console::new(None);
        s.started(false);
        s.update(json!({"state":"paused","active_elapsed_s":3.5,"elapsed_running":false}));
        assert_eq!(
            s.elapsed(s.observed + std::time::Duration::from_secs(8)),
            Some(3.5)
        );
        assert_eq!(
            s.logs.back().map(String::as_str),
            Some("已暂停，可以切换窗口。")
        );
        let before = s.log_sequence;
        for i in 0..75 {
            s.log(i.to_string());
            s.log(i.to_string());
        }
        assert_eq!((s.logs.len(), s.log_sequence), (60, before + 75));
        s.exited(false, true, Some(0), None);
        assert!(s.primary_enabled());
        s.started(true);
        assert!(s.logs.is_empty());
        assert!(!s.primary_enabled());
    }
}
