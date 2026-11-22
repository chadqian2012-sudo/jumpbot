#![windows_subsystem = "windows"]

mod console;
mod native;
mod painting;

use console::{Console, Mode};
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Write},
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    thread,
};

const BACKENDS: [(&str, &str); 5] = [
    ("自动选择", "auto"),
    ("CPU SIMD", "simd"),
    ("CPU", "cpu"),
    ("CUDA", "cuda"),
    ("OpenCL", "opencl"),
];
struct Engine {
    worker: PathBuf,
}
impl Engine {
    fn locate() -> Result<Self, String> {
        let worker = std::env::current_exe()
            .map_err(|e| e.to_string())?
            .with_file_name("jumpbot.exe");
        if !worker.is_file() {
            return Err("缺少 jumpbot.exe，请保留完整软件目录。".into());
        }
        Ok(Self { worker })
    }
    fn spawn(&self, backend: &str, daemon: bool, selftest: bool) -> std::io::Result<Child> {
        let mut cmd = Command::new(&self.worker);
        cmd.args(["--worker", "--backend", backend])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(0x08000000);
        if selftest {
            cmd.args(["--selftest", "--no-cuda", "--no-opencl"])
                .env("JUMPBOT_NO_INPUT", "1");
        } else {
            cmd.args(["--max-jumps", "0"]);
            if daemon {
                cmd.arg("--daemon");
            }
        }
        cmd.spawn()
    }
}

enum Event {
    Status(Value),
    Log { line: String, stderr: bool },
}
fn read_worker(
    stream: impl std::io::Read + Send + 'static,
    tx: Sender<Event>,
    stderr: bool,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            let event = if let Some(body) = line.strip_prefix("@jumpbot ") {
                let Ok(status) = serde_json::from_str(body) else {
                    continue;
                };
                Event::Status(status)
            } else {
                Event::Log { line, stderr }
            };
            if tx.send(event).is_err() {
                break;
            }
        }
    })
}
struct Session {
    child: Child,
    selftest: bool,
    events: Receiver<Event>,
    last_stderr: Option<String>,
    readers: [thread::JoinHandle<()>; 2],
}
impl Session {
    fn new(mut child: Child, selftest: bool) -> Self {
        let (tx, events) = mpsc::channel();
        let readers = [
            read_worker(child.stdout.take().unwrap(), tx.clone(), false),
            read_worker(child.stderr.take().unwrap(), tx, true),
        ];
        Self {
            child,
            selftest,
            events,
            last_stderr: None,
            readers,
        }
    }
    fn stop(&mut self) {
        if self.selftest {
            let _ = self.child.kill();
        } else {
            let _ = self.send(b"stop\n");
        }
        self.child.stdin.take();
    }
    fn send(&mut self, command: &[u8]) -> std::io::Result<()> {
        self.child
            .stdin
            .as_mut()
            .ok_or(std::io::ErrorKind::BrokenPipe)?
            .write_all(command)
    }
}
struct Host {
    engine: Result<Engine, String>,
    session: Option<Session>,
    ui: Console,
    closing: bool,
}
impl Host {
    fn new() -> Self {
        let engine = Engine::locate();
        let ui = Console::new(engine.as_ref().err().cloned());
        Self {
            engine,
            session: None,
            ui,
            closing: false,
        }
    }
    fn start(&mut self, index: usize, daemon: bool, selftest: bool) {
        if self.closing || self.session.is_some() || !self.ui.ready {
            return;
        }
        let backend = if selftest { "cpu" } else { BACKENDS[index].1 };
        match self
            .engine
            .as_ref()
            .unwrap()
            .spawn(backend, daemon, selftest)
        {
            Ok(child) => {
                self.session = Some(Session::new(child, selftest));
                self.ui.started(selftest);
                if !selftest {
                    self.ui.log(format!(
                        "已启动。请求后端：{}；窗口重开后{}。",
                        BACKENDS[index].0,
                        if daemon { "继续" } else { "结束本轮" }
                    ));
                }
            }
            Err(e) => self.ui.notice = e.to_string(),
        }
    }
    fn primary(&mut self, index: usize, daemon: bool) {
        if self.closing || !self.ui.primary_enabled() {
            return;
        }
        if self.ui.mode == Mode::Idle {
            self.start(index, daemon, false);
            return;
        }
        if let Some(session) = &mut self.session {
            let command = if self.ui.phase == "paused" {
                b"resume\n".as_slice()
            } else {
                b"pause\n"
            };
            self.ui.pending = self.ui.status["control_generation"].as_u64();
            if session.send(command).is_err() {
                self.stop();
                self.ui.notice = "控制连接已断开，正在停止。".into();
            }
        }
    }
    fn stop(&mut self) {
        if let Some(session) = &mut self.session {
            session.stop();
            self.ui.mode = Mode::Stopping;
            self.ui.phase = "stopping".into();
        }
    }
    fn poll(&mut self) {
        let Some(run) = &mut self.session else { return };
        // Both readers must finish before the final drain. Each session owns its own queue.
        let exit: Option<ExitStatus> = run
            .child
            .try_wait()
            .ok()
            .flatten()
            .filter(|_| run.readers.iter().all(thread::JoinHandle::is_finished));
        for event in run.events.try_iter() {
            match event {
                Event::Log { line, stderr } => {
                    if stderr && !line.trim().is_empty() {
                        run.last_stderr = Some(line.clone());
                    }
                    self.ui.log(line);
                }
                Event::Status(s) => self.ui.update(s),
            }
        }
        if let Some(code) = exit {
            self.ui.exited(
                run.selftest,
                code.success(),
                code.code(),
                run.last_stderr.as_deref(),
            );
            self.session = None;
        }
    }
}
fn main() {
    if let Err(e) = native::run(Host::new()) {
        native::startup_error(&format!("无法打开跳一跳控制台。\n\n{e}"));
        std::process::exit(1);
    }
}
