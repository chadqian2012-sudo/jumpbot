use crate::platform::{parse_hotkey, Hotkey, Hotkeys, SingleInstance};
use crate::Result;
use serde_json::{json, Value};
use std::{
    io::{self, BufRead, IsTerminal},
    sync::{mpsc, Arc, Condvar, Mutex, MutexGuard},
    thread,
    time::{Duration, Instant},
};

#[derive(Default)]
pub(crate) struct State {
    pub(crate) generation: u64,
    pub(crate) pause: bool,
    pub(crate) stopped: bool,
    acknowledged: Option<Instant>,
    pause_requested_at: Option<Instant>,
    requested_time: Duration,
    stopped_at: Option<Instant>,
    pause_count: u64,
    pub(crate) stop_reason: Option<&'static str>,
    landing_pause_reason: Option<String>,
    paused_time: Duration,
    registered: bool,
    error: Option<String>,
    needs_resume: bool,
}
struct Control {
    state: Mutex<State>,
    changed: Condvar,
}
pub(crate) struct Controls {
    shared: Arc<Control>,
    started: Instant,
    listener: Option<thread::JoinHandle<()>>,
    _instance: SingleInstance,
}
#[derive(Clone, Copy)]
enum Request {
    Pause,
    Resume,
    Toggle,
    Stop(&'static str),
}

impl State {
    fn request(&mut self, request: Request, now: Instant) {
        if self.stopped {
            return;
        }
        match request {
            Request::Stop(reason) => {
                self.stop_reason = Some(reason);
                self.stopped = true;
                self.stopped_at = Some(now);
            }
            _ => {
                let pause = match request {
                    Request::Pause => true,
                    // A GUI worker cannot recover its missing global shortcut.
                    Request::Resume if !self.registered => return,
                    Request::Resume => false,
                    _ => !self.pause,
                };
                if self.pause == pause {
                    return;
                }
                self.pause = pause;
                self.needs_resume = true;
                if pause {
                    self.pause_requested_at = Some(now)
                } else if let Some(start) = self.pause_requested_at.take() {
                    self.requested_time += now - start;
                }
            }
        }
        self.landing_pause_reason = None;
        self.generation += 1;
    }
    fn admits(&self, g: u64, ready: bool) -> bool {
        self.generation == g && !self.pause && !self.stopped && !(ready && self.needs_resume)
    }
    fn paused_for(&self, now: Instant) -> Duration {
        self.paused_time + self.acknowledged.map_or(Duration::ZERO, |t| now - t)
    }
    fn wait_clock(&self, start: Instant, now: Instant) -> Duration {
        (now - start).saturating_sub(
            self.requested_time + self.pause_requested_at.map_or(Duration::ZERO, |t| now - t),
        )
    }
}
impl Control {
    fn pause_if_current(&self, g: u64, reason: &str) -> bool {
        let mut state = self.state.lock().unwrap();
        if !state.admits(g, true) {
            return false;
        }
        state.request(Request::Pause, Instant::now());
        state.landing_pause_reason = Some(reason.to_owned());
        self.changed.notify_all();
        true
    }
    fn request(&self, request: Request) {
        self.state.lock().unwrap().request(request, Instant::now());
        self.changed.notify_all();
    }
}
impl Controls {
    pub(crate) fn start(hotkey: &str, pipe: bool) -> Result<Self> {
        let started = Instant::now();
        parse_hotkey(hotkey)?;
        let instance = SingleInstance::acquire()?;
        let shared = Arc::new(Control {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        });
        let ctrl = Arc::clone(&shared);
        ctrlc::set_handler(move || ctrl.request(Request::Stop("Ctrl+C 停止")))
            .map_err(|e| e.to_string())?;
        let ctrl = Arc::clone(&shared);
        let key = hotkey.to_string();
        let (ready, rx) = mpsc::sync_channel(0);
        let listener = thread::spawn(move || {
            let reg = Hotkeys::register(&key);
            {
                let mut s = ctrl.state.lock().unwrap();
                s.registered = reg.is_ok();
                if let Err(e) = &reg {
                    s.error = Some(e.clone());
                    s.request(Request::Pause, Instant::now());
                }
            }
            if ready.send(()).is_err() {
                return;
            }
            let mut keys = reg.ok();
            extern "C" {
                fn _kbhit() -> i32;
                fn _getch() -> i32;
            }
            let console = !pipe && io::stdin().is_terminal();
            while !ctrl.state.lock().unwrap().stopped {
                if let Some(k) = &mut keys {
                    k.poll(|event| {
                        ctrl.request(match event {
                            Hotkey::TogglePause => Request::Toggle,
                            Hotkey::Stop => Request::Stop("F8 停止"),
                        })
                    });
                } else if unsafe {
                    windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(0x77)
                } < 0
                {
                    ctrl.request(Request::Stop("F8 停止"));
                }
                if console && unsafe { _kbhit() } != 0 {
                    match unsafe { _getch() } {
                        27 | 81 | 113 => ctrl.request(Request::Stop("控制台停止")),
                        80 | 112 => ctrl.request(Request::Toggle),
                        0 | 224 => unsafe {
                            _getch();
                        },
                        _ => {}
                    }
                }
                thread::sleep(Duration::from_millis(15));
            }
        });
        rx.recv().map_err(|e| e.to_string())?;
        if pipe {
            let ctrl = Arc::clone(&shared);
            thread::spawn(move || {
                for line in io::stdin()
                    .lock()
                    .lines()
                    .map_while(std::result::Result::ok)
                {
                    match line.trim() {
                        "pause" => ctrl.request(Request::Pause),
                        "resume" => ctrl.request(Request::Resume),
                        "stop" => {
                            ctrl.request(Request::Stop("控制端要求停止"));
                            return;
                        }
                        _ => {}
                    }
                }
                ctrl.request(Request::Stop("控制端已断开"));
            });
        }
        Ok(Self {
            shared,
            listener: Some(listener),
            _instance: instance,
            started,
        })
    }
    pub(crate) fn state(&self) -> MutexGuard<'_, State> {
        self.shared.state.lock().unwrap()
    }
    pub(crate) fn pause_if_current(&self, g: u64, reason: &str) -> bool {
        self.shared.pause_if_current(g, reason)
    }
    pub(crate) fn generation(&self) -> u64 {
        self.state().generation
    }
    pub(crate) fn guard(&self, g: u64, ready: bool) -> Result<MutexGuard<'_, State>> {
        let s = self.state();
        if !s.admits(g, ready) {
            return Err("运行状态已改变".into());
        }
        Ok(s)
    }
    pub(crate) fn acknowledge(&self) {
        let mut s = self.state();
        if !s.stopped && s.pause && s.acknowledged.is_none() {
            s.acknowledged = Some(Instant::now());
            s.pause_count += 1
        }
    }
    pub(crate) fn ready(&self, g: u64) -> Result<()> {
        let mut s = self.guard(g, false)?;
        if let Some(t) = s.acknowledged.take() {
            s.paused_time += t.elapsed()
        }
        s.needs_resume = false;
        Ok(())
    }
    pub(crate) fn clock(&self) -> Duration {
        let s = self.state();
        let now = s.stopped_at.unwrap_or_else(Instant::now);
        (now - self.started).saturating_sub(s.paused_for(now))
    }
    pub(crate) fn wait_clock(&self) -> Duration {
        let s = self.state();
        s.wait_clock(self.started, s.stopped_at.unwrap_or_else(Instant::now))
    }
    pub(crate) fn status(&self, round_start: Option<Duration>, phase: &str) -> Value {
        let s = self.state();
        let now = s.stopped_at.unwrap_or_else(Instant::now);
        let paused = s.paused_for(now);
        let elapsed = round_start.map_or(Duration::ZERO, |start| {
            (now - self.started).saturating_sub(paused + start)
        });
        let phase = match phase {
            "stopped" => "stopped",
            _ if s.stopped => "stopping",
            _ if s.pause && s.acknowledged.is_some() => "paused",
            _ if s.pause => "pausing",
            _ if s.needs_resume && phase != "waiting_window" => "resuming",
            _ => phase,
        };
        json!({"state":phase,"alive":phase!="stopped","paused":s.pause,"landing_pause_reason":s.landing_pause_reason,"pause_count":s.pause_count,"control_generation":s.generation,"hotkey_registered":s.registered,"hotkey_error":s.error,"pause_total_s":paused.as_secs_f64(),"elapsed_running":round_start.is_some() && phase!="stopped" && s.acknowledged.is_none() && !s.stopped,"active_elapsed_s":elapsed.as_secs_f64()})
    }
    pub(crate) fn sleep(&self, g: u64, t: Duration) {
        let s = self.state();
        drop(
            self.shared
                .changed
                .wait_timeout_while(s, t, |s| s.generation == g && !s.stopped)
                .unwrap(),
        );
    }
}
impl Drop for Controls {
    fn drop(&mut self) {
        self.shared.request(Request::Stop("运行结束"));
        if let Some(t) = self.listener.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resume_wait_expires_while_active_clock_is_frozen() {
        let t = Instant::now();
        let mut s = State {
            registered: true,
            ..State::default()
        };
        let at = |n| t + Duration::from_secs(n);
        s.request(Request::Pause, at(2));
        s.acknowledged = Some(at(2));
        s.request(Request::Resume, at(62));
        assert_eq!(s.wait_clock(t, at(65)), Duration::from_secs(5));
        assert_eq!(at(65) - t - s.paused_for(at(65)), Duration::from_secs(2));
        assert!(!s.admits(0, true));
        assert!(!s.admits(s.generation, true));
        assert!(s.admits(s.generation, false));
    }
    #[test]
    fn landing_pause_only_applies_to_the_current_runnable_generation() {
        let ctrl = Control {
            state: Mutex::new(State {
                registered: true,
                ..State::default()
            }),
            changed: Condvar::new(),
        };
        let reason = "落点仍未确认";
        assert!(ctrl.pause_if_current(0, reason));
        ctrl.request(Request::Pause); // Already paused.
        assert!(!ctrl.pause_if_current(1, "迟到的原因"));
        assert_eq!(
            ctrl.state.lock().unwrap().landing_pause_reason.as_deref(),
            Some(reason)
        );

        ctrl.request(Request::Resume);
        assert!(ctrl.state.lock().unwrap().landing_pause_reason.is_none());
        ctrl.request(Request::Toggle); // Pause again before a window has been found.
        {
            let s = ctrl.state.lock().unwrap();
            assert!(s.pause);
            assert!(s.landing_pause_reason.is_none());
        }

        ctrl.request(Request::Resume);
        assert!(!ctrl.pause_if_current(4, reason)); // Resume countdown is still running.
        ctrl.state.lock().unwrap().needs_resume = false;
        assert!(!ctrl.pause_if_current(0, reason));
        assert!(ctrl.pause_if_current(4, reason));
        ctrl.request(Request::Stop("F8 停止"));
        assert!(!ctrl.pause_if_current(6, reason));
        let s = ctrl.state.lock().unwrap();
        assert_eq!(s.generation, 6);
        assert!(s.landing_pause_reason.is_none());
        assert_eq!(s.stop_reason, Some("F8 停止"));
    }
    #[test]
    fn hotkey_conflict_denies_pipe_resume_and_stop_wins_over_toggle() {
        let t = Instant::now();
        let mut s = State::default();
        s.request(Request::Pause, t);
        s.request(Request::Resume, t);
        assert!(!s.admits(s.generation, false));
        s.request(Request::Toggle, t); // explicit console P recovery
        assert!(s.admits(s.generation, false));
        assert!(!s.admits(s.generation, true));
        s.request(Request::Stop("fixture stop"), t);
        let g = s.generation;
        s.request(Request::Toggle, t);
        assert_eq!(g, s.generation);
        assert_eq!(s.stop_reason, Some("fixture stop"));
        assert!(!s.admits(g, false));
    }
}
