use serde_json::Value;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pose {
    pub along: f32,
    pub lift: f32,
    pub squash: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Jump {
    pub started: Instant,
    from: Pose,
    pub to: f32,
}

impl Jump {
    pub fn next(previous: Option<Self>, now: Instant) -> Self {
        Self {
            started: now,
            from: previous.map_or_else(Pose::default, |jump| jump.pose(now)),
            to: previous.map_or(1., |jump| 1. - jump.to),
        }
    }

    pub fn duration(self) -> Duration {
        Duration::from_millis(if self.to == 1. { 440 } else { 260 })
    }

    pub fn rest(self) -> Pose {
        Pose {
            along: self.to,
            ..Pose::default()
        }
    }

    pub fn pose(self, now: Instant) -> Pose {
        let t = now.saturating_duration_since(self.started).as_secs_f32()
            / self.duration().as_secs_f32();
        if t >= 1. {
            return self.rest();
        }
        // Reserve the final part of the movement for a single, foot-anchored landing.
        let flight = (t / 0.80).min(1.);
        let u = flight * flight * (3. - 2. * flight);
        let air = 4. * u * (1. - u);
        let apex: f32 = if self.to == 1. { 0.31 } else { 0.16 };
        let settle = ((t - 0.80) / 0.20).max(0.);
        let contact = 4. * settle * (1. - settle);
        Pose {
            along: self.from.along + (self.to - self.from.along) * u,
            lift: self.from.lift * (1. - u)
                + (apex - self.from.lift).max(0.) * (self.to - self.from.along).abs() * air,
            squash: self.from.squash * (1. - u) + contact * contact,
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
            if s["state"] != self.status["state"] || s["stop_reason"] != self.status["stop_reason"]
            {
                let reason = s["stop_reason"].as_str().unwrap_or("");
                match phase {
                    "waiting_window" | "capture_blocked" | "blocked_modal" | "result_pending"
                    | "unrecognized_dark"
                        if !reason.is_empty() =>
                    {
                        self.log(reason.to_owned())
                    }
                    "paused" => self.log("已暂停，可以切换窗口。".into()),
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
                let score = s["score"]
                    .as_i64()
                    .map_or_else(|| "待判定".to_owned(), |v| v.to_string());
                let result = match s["last_result"].as_str() {
                    Some("landed") => "已落地",
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
            "paused" => ("已暂停", "现在可以切换窗口。"),
            "pausing" => ("正在暂停", "正在等待当前按压结束。"),
            "playing" => ("运行中", "正在识别棋子与目标。"),
            "acquiring" => ("正在识别", "正在寻找可用的游戏画面。"),
            "landing" => ("正在核对落点", "本次按压已释放，正在读取落点。"),
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
    fn return_takes_a_lower_faster_arc_and_retargeting_preserves_the_pose() {
        let now = Instant::now();
        let out = Jump::next(None, now);
        let back = Jump::next(Some(out), now + out.duration());
        let mid_flight = |j: Jump| j.started + j.duration().mul_f32(0.4);
        assert!(back.duration() < out.duration());
        assert!(back.pose(mid_flight(back)).lift < out.pose(mid_flight(out)).lift);
        for fraction in [0.4, 0.9] {
            let interrupt = now + out.duration().mul_f32(fraction);
            let turn = Jump::next(Some(out), interrupt);
            assert_eq!(turn.pose(interrupt), out.pose(interrupt));
            assert_eq!(turn.pose(interrupt + turn.duration()), Pose::default());
        }
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
