//! Set JUMPBOT_TEST_PACKAGE to an unpacked package, then run
//! cargo test --manifest-path desktop/Cargo.toml --test packaged_gui -- --ignored --nocapture
//! All actions target a private copy and use UI Automation without physical input.
#![cfg(windows)]
use std::{
    env, fs, io,
    mem::size_of,
    path::{Path, PathBuf},
    process::{Child, Command},
    ptr::null_mut,
    thread,
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::HWND as ComHwnd,
    System::{Com::*, Variant::VARIANT},
    UI::Accessibility::*,
};
use windows_sys::Win32::{
    Foundation::*,
    System::{Diagnostics::ToolHelp::*, Threading::*},
    UI::WindowsAndMessaging::*,
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn copy_dir(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let path = dst.join(entry.file_name());
        if ty.is_file() {
            fs::copy(entry.path(), path)?;
        } else if ty.is_dir() {
            copy_dir(&entry.path(), &path)?;
        } else {
            return Err(io::Error::other("Unexpected link in package"));
        }
    }
    Ok(())
}
fn until<T>(label: &str, mut check: impl FnMut() -> Option<T>) -> Result<T> {
    let until = Instant::now() + Duration::from_secs(25);
    loop {
        if let Some(v) = check() {
            return Ok(v);
        }
        if Instant::now() >= until {
            return Err(format!("Timed out waiting for {label}").into());
        }
        thread::sleep(Duration::from_millis(65));
    }
}
unsafe fn window(pid: u32) -> Option<HWND> {
    unsafe extern "system" fn visit(w: HWND, l: isize) -> i32 {
        let args = &mut *(l as *mut (u32, HWND));
        let mut pid = 0;
        GetWindowThreadProcessId(w, &mut pid);
        let mut class = [0; 64];
        let n = GetClassNameW(w, class.as_mut_ptr(), 64);
        if pid == args.0
            && String::from_utf16_lossy(&class[..n as usize]) == "JumpBotConsole"
            && IsWindowVisible(w) != 0
        {
            args.1 = w;
            return 0;
        }
        1
    }
    let mut args: (u32, HWND) = (pid, null_mut());
    EnumWindows(Some(visit), (&mut args as *mut (u32, HWND)) as isize);
    (!args.1.is_null()).then_some(args.1)
}
struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
unsafe fn snapshot(flags: u32, pid: u32) -> Result<Handle> {
    let h = CreateToolhelp32Snapshot(flags, pid);
    if h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error().into());
    }
    Ok(Handle(h))
}
fn string(buf: &[u16]) -> String {
    String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())])
}
unsafe fn worker(gui: u32, expected: &Path) -> Result<(u32, Handle)> {
    let list = snapshot(TH32CS_SNAPPROCESS, 0)?;
    let mut p = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut more = Process32FirstW(list.0, &mut p);
    let mut children = vec![];
    while more != 0 {
        if p.th32ParentProcessID == gui {
            children.push((p.th32ProcessID, string(&p.szExeFile)));
        }
        more = Process32NextW(list.0, &mut p);
    }
    assert_eq!(children.len(), 1, "desktop worker children {children:?}");
    let (pid, name) = children.pop().unwrap();
    assert_eq!(name.to_lowercase(), "jumpbot.exe");
    let handle = OpenProcess(
        PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
        0,
        pid,
    );
    if handle.is_null() {
        return Err(io::Error::last_os_error().into());
    }
    let handle = Handle(handle);
    let mut path = vec![0u16; 32768];
    let mut len = path.len() as u32;
    assert_ne!(
        QueryFullProcessImageNameW(handle.0, 0, path.as_mut_ptr(), &mut len),
        0
    );
    assert_eq!(
        Path::new(&string(&path)).canonicalize()?,
        expected.canonicalize()?
    );
    Ok((pid, handle))
}
unsafe fn modules(pid: u32, package: Option<&Path>) -> Result<()> {
    let mut packaged_opencv = false;
    let snap = snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid)?;
    let mut m = MODULEENTRY32W {
        dwSize: size_of::<MODULEENTRY32W>() as u32,
        ..Default::default()
    };
    let mut more = Module32FirstW(snap.0, &mut m);
    assert_ne!(more, 0);
    while more != 0 {
        let name = string(&m.szModule).to_ascii_lowercase();
        assert!(
            !["webview2", "msedge", "python", "pypy"]
                .iter()
                .any(|s| name.contains(s)),
            "Unexpected module {name} in PID {pid}"
        );
        if let Some(root) = package {
            if name.starts_with("opencv_world") && name.ends_with(".dll") {
                assert_eq!(
                    Path::new(&string(&m.szExePath)).canonicalize()?,
                    root.join(&name).canonicalize()?
                );
                packaged_opencv = true;
            }
        }
        more = Module32NextW(snap.0, &mut m);
    }
    assert!(
        package.is_none() || packaged_opencv,
        "worker did not load the packaged OpenCV runtime"
    );
    Ok(())
}
struct Gui {
    child: Child,
    hwnd: HWND,
}
impl Drop for Gui {
    fn drop(&mut self) {
        unsafe {
            if self.child.try_wait().ok().flatten().is_none() {
                if !self.hwnd.is_null() {
                    PostMessageW(self.hwnd, WM_CLOSE, 0, 0);
                }
                for _ in 0..60 {
                    if self.child.try_wait().ok().flatten().is_some() {
                        return;
                    }
                    thread::sleep(Duration::from_millis(50));
                }
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }
}
struct Automation {
    root: IUIAutomationElement,
    api: IUIAutomation,
}
impl Automation {
    unsafe fn find(&self, id: &str) -> Result<IUIAutomationElement> {
        let c = self
            .api
            .CreatePropertyCondition(UIA_AutomationIdPropertyId, &VARIANT::from(id))?;
        Ok(self.root.FindFirst(TreeScope_Descendants, &c)?)
    }
    unsafe fn named(&self, id: &str, name: &str) -> Result<IUIAutomationElement> {
        until(&format!("{id} = {name}"), || {
            let e = self.find(id).ok()?;
            (e.CurrentName().ok()? == name).then_some(e)
        })
    }
    unsafe fn invoke(&self, id: &str, name: &str) -> Result<()> {
        let e = self.named(id, name)?;
        until(&format!("{id} enabled"), || {
            e.CurrentIsEnabled().ok()?.as_bool().then_some(())
        })?;
        assert_eq!(e.CurrentControlType()?, UIA_ButtonControlTypeId);
        e.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)?
            .Invoke()?;
        Ok(())
    }
}
#[test]
#[ignore = "requires an unpacked Windows package and a desktop session"]
fn packaged_gui() -> Result<()> {
    unsafe {
        let original = PathBuf::from(
            env::var_os("JUMPBOT_TEST_PACKAGE")
                .ok_or("Set JUMPBOT_TEST_PACKAGE to the unpacked package directory")?,
        )
        .canonicalize()?;
        let stage = tempfile::tempdir()?;
        let app = stage.path().join("jumpbot");
        copy_dir(&original, &app)?;
        let config = app.join("config.json");
        let text = fs::read_to_string(&config)?;
        let mut cfg: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))?;
        let nonce = stage.path().file_name().unwrap().to_string_lossy();
        cfg["window_title_pattern"] = format!("^JumpBot private QA {nonce}$").into();
        cfg["window_exe_hint"] = format!("never-match-{nonce}.exe").into();
        cfg["window_wait_s"] = 120.into();
        cfg["startup_wait_s"] = 120.into();
        cfg["pause_hotkey"] = "Ctrl+Shift+7".into();
        fs::write(&config, serde_json::to_vec(&cfg)?)?;
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        struct Com;
        impl Drop for Com {
            fn drop(&mut self) {
                unsafe { CoUninitialize() }
            }
        }
        let _com = Com;
        let mut cmd = Command::new(app.join("jumpbot-desktop.exe"));
        for (name, _) in env::vars_os() {
            let key = name.to_string_lossy().to_uppercase();
            if key.starts_with("JUMPBOT_")
                || key.starts_with("WEBVIEW2_")
                || key.starts_with("PYTHON")
            {
                cmd.env_remove(name);
            }
        }
        let child = cmd
            .current_dir(stage.path())
            .env(
                "PATH",
                PathBuf::from(env::var_os("SystemRoot").unwrap()).join("System32"),
            )
            .env("JUMPBOT_NO_INPUT", "1")
            .env("JUMPBOT_DISABLE_CUDA", "1")
            .env("JUMPBOT_DISABLE_OPENCL", "1")
            .spawn()?;
        let mut gui = Gui {
            child,
            hwnd: null_mut(),
        };
        let pid = gui.child.id();
        gui.hwnd = until("packaged JumpBot window", || window(pid))?;
        let api: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
        let ui = Automation {
            root: api.ElementFromHandle(ComHwnd(gui.hwnd))?,
            api,
        };
        ui.named("phase", "待机")?;
        let theme = ui.find("theme")?.CurrentName()?.to_string();
        ui.invoke("theme", &theme)?;
        let other = if theme == "深色模式" {
            "浅色模式"
        } else {
            "深色模式"
        };
        ui.named("theme", other)?;
        ui.invoke("theme", other)?;
        ui.named("theme", &theme)?;
        let combo = ui.find("backend")?;
        assert_eq!(combo.CurrentControlType()?, UIA_ComboBoxControlTypeId);
        // The system hosts the drop-down list in a separate popup. Select the actual
        // combo item on its verified HWND without depending on focus or a popup lifetime.
        let native_combo = combo.CurrentNativeWindowHandle()?.0;
        let mut owner = 0;
        GetWindowThreadProcessId(native_combo, &mut owner);
        assert_eq!(owner, pid);
        let simd = windows_sys::core::w!("CPU SIMD");
        let index = SendMessageW(native_combo, CB_SELECTSTRING, usize::MAX, simd as isize);
        assert_ne!(index, CB_ERR as isize);
        assert_eq!(SendMessageW(native_combo, CB_GETCURSEL, 0, 0), index);
        let repeat = ui
            .find("daemon")?
            .GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)?;
        repeat.Toggle()?;
        until("daemon checkbox checked", || {
            (repeat.CurrentToggleState().ok()? == ToggleState_On).then_some(())
        })?;
        ui.invoke("check", "无输入自检")?;
        ui.named("notice", "CPU 无输入自检通过。")?;
        ui.named("phase", "待机")?;
        eprintln!("PASS theme, backend, daemon and CPU no-input selftest");
        let mut previous = None;
        for close in [false, true] {
            ui.invoke("primary", "启动脚本")?;
            ui.named("phase", "等待游戏窗口")?;
            ui.named("actual-backend", "CPU SIMD")?;
            assert!(!combo.CurrentIsEnabled()?.as_bool());
            let (worker_pid, child) = worker(pid, &app.join("jumpbot.exe"))?;
            assert_ne!(previous, Some(worker_pid));
            previous = Some(worker_pid);
            modules(pid, None)?;
            modules(worker_pid, Some(&app))?;
            if close {
                PostMessageW(gui.hwnd, WM_CLOSE, 0, 0);
                assert!(until("GUI close", || gui.child.try_wait().ok().flatten())?.success());
            } else {
                ui.invoke("primary", "暂停运行")?;
                ui.named("phase", "已暂停")?;
                ui.invoke("primary", "恢复运行")?;
                ui.named("phase", "等待游戏窗口")?;
                assert_eq!(worker(pid, &app.join("jumpbot.exe"))?.0, worker_pid);
                ui.invoke("stop", "停止")?;
                ui.named("phase", "已停止")?;
                ui.named("primary", "启动脚本")?;
            }
            assert_eq!(
                WaitForSingleObject(child.0, 5000),
                WAIT_OBJECT_0,
                "worker {worker_pid} outlived stop/close"
            );
            eprintln!(
                "PASS packaged worker {worker_pid} {} and exit",
                if close {
                    "restart / close"
                } else {
                    "waiting / pause / resume / stop"
                }
            );
        }
        eprintln!("PASS package {}", original.display());
        Ok(())
    }
}
