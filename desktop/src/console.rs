use serde_json::Value;
use std::{collections::VecDeque, time::Instant};
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
    pub landed: Option<(Instant, bool)>,
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
        if self.mode == Mode::Live {
            let phase = s["state"].as_str().unwrap_or("starting");
            if phase != self.phase || s["stop_reason"] != self.status["stop_reason"] {
                let reason = s["stop_reason"].as_str().unwrap_or("");
                match phase {
                    "waiting_window" | "capture_blocked" | "blocked_modal" | "stopped"
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
            if phase != self.phase {
                if phase == "landing" {
                    self.landed =
                        Some((self.observed, !self.landed.is_some_and(|(_, right)| right)));
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
    pub fn exited(&mut self, test: bool, ok: bool, code: Option<i32>) {
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
        } else if !ok {
            self.notice = format!(
                "脚本退出（代码 {}）。请查看运行日志。",
                code.map(|n| n.to_string()).unwrap_or_else(|| "未知".into())
            );
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
            "blocked_modal" => ("窗口有弹窗", "请查看游戏窗口中的提示。"),
            "dead" => ("本局结束", "正在处理游戏结束画面。"),
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
        s.exited(false, true, Some(0));
        assert!(s.primary_enabled());
        s.started(true);
        assert!(s.logs.is_empty());
        assert!(!s.primary_enabled());
    }
}
