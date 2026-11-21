#![windows_subsystem = "windows"]

use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    error::Error,
    io::{BufRead, BufReader, Write},
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    window::WindowBuilder,
};
use wry::{http, WebView, WebViewBuilder};

const HOME: &str = "http://jumpbot.localhost/";
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Ready,
    Start { backend: Backend, daemon: bool },
    Pause,
    Resume,
    Stop,
    Selftest,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Backend {
    Auto,
    Simd,
    Cpu,
    Cuda,
    Opencl,
}

impl Backend {
    fn argument(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Simd => "simd",
            Self::Cpu => "cpu",
            Self::Cuda => "cuda",
            Self::Opencl => "opencl",
        }
    }
}

enum Message {
    Ui(Action),
    Worker(u64, Value),
}

struct Engine {
    cwd: PathBuf,
    python: PathBuf,
    dll: PathBuf,
}

impl Engine {
    fn locate() -> Result<Self, String> {
        let exe = std::env::current_exe().map_err(|error| error.to_string())?;
        let cwd = exe
            .ancestors()
            .skip(1)
            .find(|dir| dir.join("jumpbot.py").is_file())
            .ok_or("找不到 jumpbot.py。请在项目内保留编译后的桌面程序。")?;
        let python = std::env::var_os("JUMPBOT_PYTHON")
            .map(PathBuf::from)
            .or_else(|| {
                cwd.ancestors()
                    .map(|dir| dir.join(r".venv\Scripts\python.exe"))
                    .find(|path| path.is_file())
            })
            .ok_or("找不到 Python 环境。请设置 JUMPBOT_PYTHON 或创建项目的 .venv。")?;
        if !python.is_absolute() || !python.is_file() {
            return Err("JUMPBOT_PYTHON 必须指向 Python 可执行文件的绝对路径。".into());
        }
        let dll = exe.with_file_name("jumpbot_vision.dll");
        if !dll.is_file() {
            return Err("找不到 jumpbot_vision.dll。请编译完整 Cargo 工作区。".into());
        }
        Ok(Self {
            cwd: cwd.into(),
            python,
            dll,
        })
    }

    fn spawn(&self, backend: Backend, daemon: bool, selftest: bool) -> std::io::Result<Child> {
        let mut command = Command::new(&self.python);
        command
            .current_dir(&self.cwd)
            .arg(self.cwd.join("jumpbot.py"))
            .args(["--backend", backend.argument(), "--no-dashboard"])
            .env("JUMPBOT_DESKTOP_PIPE", "1")
            .env("JUMPBOT_VISION_DLL", &self.dll)
            .env("PYTHONUTF8", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW);
        if selftest {
            command.args(["--selftest", "--no-cuda", "--no-opencl"]);
            command.env("JUMPBOT_NO_INPUT", "1");
        } else {
            command.args(["--max-jumps", "0"]);
            if daemon {
                command.arg("--daemon");
            }
        }
        command.spawn()
    }
}

struct Session {
    child: Child,
    id: u64,
    selftest: bool,
    readers: [thread::JoinHandle<()>; 2],
}

impl Session {
    fn new(mut child: Child, id: u64, selftest: bool, proxy: &EventLoopProxy<Message>) -> Self {
        let readers = [
            read_worker(child.stdout.take().unwrap(), id, proxy.clone()),
            read_worker(child.stderr.take().unwrap(), id, proxy.clone()),
        ];
        Self {
            child,
            id,
            selftest,
            readers,
        }
    }

    fn stop(&mut self) {
        // EOF requests cooperative stop, including on unexpected host exit.
        self.child.stdin.take();
        if self.selftest {
            // Selftest never opens a game window or owns mouse input.
            let _ = self.child.kill();
        }
    }

    fn send(&mut self, command: &[u8]) -> std::io::Result<()> {
        if let Some(pipe) = &mut self.child.stdin {
            pipe.write_all(command)?;
        }
        Ok(())
    }
}

fn read_worker(
    stream: impl std::io::Read + Send + 'static,
    id: u64,
    proxy: EventLoopProxy<Message>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            let event = match line.strip_prefix("@jumpbot ") {
                Some(payload) => match serde_json::from_str::<Value>(payload) {
                    Ok(status) => json!({ "type": "status", "status": status }),
                    Err(_) => continue,
                },
                None => json!({ "type": "log", "line": line }),
            };
            if proxy.send_event(Message::Worker(id, event)).is_err() {
                break;
            }
        }
    })
}

fn emit(view: &WebView, message: Value) {
    let _ = view.evaluate_script(&format!("window.renderJumpBot({message})"));
}

fn asset(uri: &http::Uri) -> http::Response<&'static [u8]> {
    let (kind, bytes): (&str, &[u8]) = match uri.path() {
        "/" => (
            "text/html; charset=utf-8",
            include_bytes!("../ui/index.html"),
        ),
        "/endfield.css" => ("text/css", include_bytes!("../ui/endfield.css")),
        "/console.css" => ("text/css", include_bytes!("../ui/console.css")),
        "/console.js" => ("text/javascript", include_bytes!("../ui/console.js")),
        "/Saira.ttf" => ("font/ttf", include_bytes!("../ui/Saira.ttf")),
        _ => return http::Response::builder().status(404).body(&[][..]).unwrap(),
    };
    http::Response::builder()
        .header("Content-Type", kind)
        .header("Content-Security-Policy", "default-src 'self'; style-src 'self' 'unsafe-inline'; object-src 'none'; base-uri 'none'; frame-src 'none'; form-action 'none'")
        .body(bytes)
        .unwrap()
}

fn run() -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoopBuilder::<Message>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let window = WindowBuilder::new()
        .with_title("跳一跳控制台 / JumpBot")
        .with_inner_size(LogicalSize::new(1200, 800))
        .with_min_inner_size(LogicalSize::new(680, 520))
        .build(&event_loop)?;
    let ipc_proxy = proxy.clone();
    let view = WebViewBuilder::new()
        .with_incognito(true)
        .with_navigation_handler(|url| url == HOME)
        .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
        .with_custom_protocol("jumpbot".into(), |_, request| {
            asset(request.uri()).map(Into::into)
        })
        .with_url("jumpbot://localhost/")
        .with_ipc_handler(move |request| {
            if request.uri().to_string() == HOME {
                if let Ok(action) = serde_json::from_str(request.body()) {
                    let _ = ipc_proxy.send_event(Message::Ui(action));
                }
            }
        })
        .build(&window)?;

    let engine = Engine::locate();
    let mut session: Option<Session> = None;
    let mut sequence = 0;
    let mut closing = false;
    let mut drained_at = None;
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        let _keep_window_alive = &window;
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                closing = true;
                if let Some(run) = &mut session {
                    run.stop();
                    emit(&view, json!({ "type": "stopping" }));
                }
            }
            Event::UserEvent(Message::Ui(action)) if !closing => match action {
                Action::Ready => {
                    emit(&view, json!({ "type": "ready", "error": engine.as_ref().err() }));
                }
                Action::Start { .. } | Action::Selftest if session.is_none() => {
                    let (backend, daemon, selftest) = match action {
                        Action::Start { backend, daemon } => (backend, daemon, false),
                        _ => (Backend::Cpu, false, true),
                    };
                    match engine.as_ref().map_err(Clone::clone).and_then(|engine| {
                        engine.spawn(backend, daemon, selftest).map_err(|err| err.to_string())
                    }) {
                        Ok(child) => {
                            sequence += 1;
                            session = Some(Session::new(child, sequence, selftest, &proxy));
                            emit(&view, json!({ "type": "started", "selftest": selftest }));
                        }
                        Err(error) => emit(&view, json!({ "type": "error", "error": error })),
                    }
                }
                Action::Stop => {
                    if let Some(run) = &mut session {
                        run.stop();
                        emit(&view, json!({ "type": "stopping" }));
                    }
                }
                Action::Pause | Action::Resume => {
                    if let Some(run) = &mut session {
                        let command = if matches!(action, Action::Pause) { b"pause\n".as_slice() } else { b"resume\n" };
                        if !run.selftest && run.send(command).is_err() {
                            run.stop();
                            emit(&view, json!({ "type": "error", "error": "控制连接已断开，正在停止。" }));
                        }
                    }
                }
                _ => {}
            },
            Event::UserEvent(Message::Worker(id, message))
                if session.as_ref().is_some_and(|run| run.id == id) => emit(&view, message),
            Event::MainEventsCleared => {
                if let Some(run) = &mut session {
                    if let Ok(Some(code)) = run.child.try_wait() {
                        if run.readers.iter().all(thread::JoinHandle::is_finished) {
                            // Leave an event-loop turn for the final queued stdout/stderr events.
                            let deadline = *drained_at.get_or_insert_with(|| Instant::now() + Duration::from_millis(100));
                            if Instant::now() >= deadline {
                                emit(&view, json!({ "type": "exited", "ok": code.success(), "code": code.code(), "selftest": run.selftest }));
                                session = None;
                                drained_at = None;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        if session.is_some() {
            *flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(100));
        } else if closing {
            *flow = ControlFlow::Exit;
        }
    });
}

fn main() {
    if let Err(error) = run() {
        show_startup_error(&format!(
            "无法打开跳一跳控制台。请检查 WebView2 Runtime。\n\n{error}"
        ));
    }
}

fn show_startup_error(message: &str) {
    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(hwnd: isize, text: *const u16, title: *const u16, kind: u32) -> i32;
    }
    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }
    unsafe { MessageBoxW(0, wide(message).as_ptr(), wide("JumpBot").as_ptr(), 0x10) };
}
