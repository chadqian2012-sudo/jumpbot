use crate::{
    console::{Console, Mode},
    painting::{self, Colors, Divider, Fonts, Layout, Scene, SCENE},
    Host, BACKENDS,
};
use std::{
    collections::HashMap,
    io,
    ptr::{null, null_mut},
    time::Instant,
};
use windows::{
    core::PCWSTR,
    Win32::{Foundation::HWND as ComHwnd, System::Com::*, UI::Accessibility::*},
};
use windows_sys::{
    core::w,
    Win32::{
        Foundation::*,
        Graphics::{Dwm::DwmSetWindowAttribute, Gdi::*},
        System::{LibraryLoader::GetModuleHandleW, Registry::*},
        UI::{
            Controls::{BST_CHECKED, DRAWITEMSTRUCT},
            HiDpi::*,
            Input::KeyboardAndMouse::{
                EnableWindow, GetCapture, IsWindowEnabled, ReleaseCapture, SetCapture, VK_ESCAPE,
            },
            WindowsAndMessaging::*,
        },
    },
};

#[repr(usize)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Id {
    Theme = 101,
    Phase,
    PhaseNote,
    Notice,
    Score,
    Jumps,
    Hit,
    Land,
    Elapsed,
    ActualBackend,
    Kernel,
    Window,
    Countdown,
    Shortcuts,
    Backend,
    Daemon,
    Primary,
    Stop,
    Check,
    Empty,
    Ledger,
}
use Id::*;
const IDS: &[(Id, &str)] = &[
    (Theme, "theme"),
    (Phase, "phase"),
    (PhaseNote, "phase-note"),
    (Notice, "notice"),
    (Score, "score"),
    (Jumps, "jumps"),
    (Hit, "hit"),
    (Land, "land"),
    (Elapsed, "elapsed"),
    (ActualBackend, "actual-backend"),
    (Kernel, "kernel"),
    (Window, "window"),
    (Countdown, "countdown"),
    (Shortcuts, "shortcuts"),
    (Backend, "backend"),
    (Daemon, "daemon"),
    (Primary, "primary"),
    (Stop, "stop"),
    (Check, "check"),
    (Empty, "empty"),
    (Ledger, "ledger"),
];
impl Id {
    pub fn from_raw(n: usize) -> Option<Self> {
        IDS.iter().find(|(id, _)| *id as usize == n).map(|x| x.0)
    }
}
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
const ACTION: u32 = WM_APP + 1;
const RESIZE: u32 = WM_APP + 2;
const CLOSE: u32 = WM_APP + 3;
const SETTINGS: u32 = WM_APP + 4;
unsafe fn divider_cursor(d: Divider) {
    let shape = if d == Divider::Columns {
        IDC_SIZEWE
    } else {
        IDC_SIZENS
    };
    SetCursor(LoadCursorW(null_mut(), shape));
}

unsafe fn end_drag(hwnd: HWND) {
    let dragging = SCENE.with_borrow_mut(|s| s.as_mut().and_then(|s| s.drag.take()).is_some());
    // ReleaseCapture synchronously sends WM_CAPTURECHANGED back into this procedure.
    if dragging && GetCapture() == hwnd {
        ReleaseCapture();
    }
}

unsafe fn place_controls(
    hwnd: HWND,
    layout: Layout,
    previous: Option<Layout>,
    fonts_changed: bool,
) {
    for &(id, _) in IDS {
        let child = GetDlgItem(hwnd, id as i32);
        if fonts_changed {
            let font = SCENE.with_borrow(|s| s.as_ref().unwrap().fonts.control(id));
            SendMessageW(child, WM_SETFONT, font as usize, 0);
        }
        let r = layout.bounds(id);
        if previous.is_none_or(|old| EqualRect(&old.bounds(id), &r) == 0) {
            MoveWindow(child, r.left, r.top, r.right - r.left, r.bottom - r.top, 0);
        }
    }
    RedrawWindow(hwnd, null(), null_mut(), RDW_INVALIDATE | RDW_ALLCHILDREN);
}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, wp: usize, lp: isize) -> isize {
    match msg {
        WM_SETCURSOR if wp == h as usize && lp as u16 == HTCLIENT as u16 => {
            let mut pt = POINT::default();
            let in_client = GetCursorPos(&mut pt) != 0 && ScreenToClient(h, &mut pt) != 0;
            let d = SCENE.with_borrow(|s| {
                let s = s.as_ref()?;
                s.drag.map(|(d, _)| d).or_else(|| {
                    if in_client {
                        s.layout.hit(pt.x, pt.y)
                    } else {
                        None
                    }
                })
            });
            if let Some(d) = d {
                divider_cursor(d);
                1
            } else {
                DefWindowProcW(h, msg, wp, lp)
            }
        }
        WM_LBUTTONDOWN | WM_MOUSEMOVE | WM_LBUTTONUP => {
            let x = lp as i16 as i32;
            let y = (lp >> 16) as i16 as i32;
            if msg == WM_LBUTTONDOWN {
                let hit = SCENE.with_borrow(|s| {
                    let l = s.as_ref()?.layout;
                    let d = l.hit(x, y)?;
                    Some((d, l.logical_axis(d, x, y) - l.position(d)))
                });
                if let Some((d, offset)) = hit {
                    SetCapture(h);
                    if GetCapture() == h {
                        SCENE.with_borrow_mut(|s| s.as_mut().unwrap().drag = Some((d, offset)));
                        divider_cursor(d);
                    }
                    return 0;
                }
            } else if GetCapture() == h {
                let changed = SCENE.with_borrow_mut(|s| {
                    let s = s.as_mut()?;
                    let (d, offset) = s.drag?;
                    let old = s.layout;
                    s.layout
                        .move_to(d, old.logical_axis(d, x, y) - offset)
                        .then_some((old, s.layout))
                });
                if let Some((old, layout)) = changed {
                    place_controls(h, layout, Some(old), false);
                }
                if msg == WM_LBUTTONUP {
                    end_drag(h);
                }
                return 0;
            }
            DefWindowProcW(h, msg, wp, lp)
        }
        WM_CANCELMODE => {
            end_drag(h);
            DefWindowProcW(h, msg, wp, lp)
        }
        WM_CAPTURECHANGED => {
            SCENE.with_borrow_mut(|s| {
                if let Some(s) = s {
                    s.drag = None;
                }
            });
            0
        }
        WM_PAINT => {
            painting::paint(h);
            0
        }
        WM_ERASEBKGND => 1,
        WM_DRAWITEM if lp != 0 => {
            painting::button(&*(lp as *const DRAWITEMSTRUCT));
            1
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLORLISTBOX | WM_CTLCOLOREDIT => {
            let dc = wp as HDC;
            let id = Id::from_raw(GetDlgCtrlID(lp as HWND) as usize);
            SCENE.with_borrow(|s| {
                if let Some(s) = s {
                    let bg = s.bg(id);
                    SetBkColor(dc, bg);
                    SetDCBrushColor(dc, bg);
                    SetTextColor(
                        dc,
                        if matches!(id, Some(Phase | Notice)) {
                            0x151818
                        } else {
                            s.colors.ink
                        },
                    );
                    GetStockObject(DC_BRUSH) as isize
                } else {
                    DefWindowProcW(h, msg, wp, lp)
                }
            })
        }
        WM_COMMAND => {
            let id = wp & 0xffff;
            if wp >> 16 == BN_CLICKED as usize
                && lp != 0
                && GetDlgItem(h, id as i32) == lp as HWND
                && IsWindowEnabled(lp as HWND) != 0
            {
                PostMessageW(h, ACTION, id, lp);
            }
            0
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED => {
            PostMessageW(h, SETTINGS, 0, 0);
            0
        }
        WM_SIZE => {
            end_drag(h);
            if wp != SIZE_MINIMIZED as usize {
                PostMessageW(h, RESIZE, 0, 0);
            }
            0
        }
        WM_DPICHANGED => {
            let r = *(lp as *const RECT);
            SetWindowPos(
                h,
                null_mut(),
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            PostMessageW(h, RESIZE, 0, 0);
            0
        }
        WM_GETMINMAXINFO => {
            let m = &mut *(lp as *mut MINMAXINFO);
            let dpi = GetDpiForWindow(h).max(96) as i32;
            m.ptMinTrackSize = POINT {
                x: 700 * dpi / 96,
                y: 550 * dpi / 96,
            };
            0
        }
        WM_CLOSE => {
            PostMessageW(h, CLOSE, 0, 0);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}
fn system_dark() -> bool {
    unsafe {
        let mut value = 1u32;
        let mut size = 4;
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&mut value as *mut u32).cast(),
            &mut size,
        ) == 0
            && value == 0
    }
}
fn system_motion() -> bool {
    unsafe {
        let mut enabled = 0i32;
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            (&mut enabled as *mut i32).cast(),
            0,
        ) != 0
            && enabled != 0
    }
}
struct View {
    hwnd: HWND,
    annotations: IAccPropServices,
    text: HashMap<Id, String>,
    log_sequence: u64,
    dark: bool,
    manual: bool,
    motion: bool,
    font_resource: HANDLE,
    timer: u32,
    transition: Instant,
    reveals: HashMap<Id, Instant>,
}
impl View {
    unsafe fn new(hwnd: HWND) -> Result<Self, Box<dyn std::error::Error>> {
        let annotations = CoCreateInstance(&CAccPropServices, None, CLSCTX_INPROC_SERVER)?;
        let font = include_bytes!("../ui/Saira.ttf");
        let mut count = 0;
        let font_resource = AddFontMemResourceEx(
            font.as_ptr().cast(),
            font.len() as u32,
            null(),
            &raw mut count,
        );
        let now = Instant::now();
        let v = Self {
            hwnd,
            annotations,
            text: HashMap::new(),
            log_sequence: 0,
            dark: system_dark(),
            manual: false,
            motion: system_motion(),
            font_resource,
            timer: 0,
            transition: now,
            reveals: HashMap::new(),
        };
        for &(id, name) in IDS {
            let (class, style, caption) = match id {
                Theme | Primary | Stop | Check => (
                    w!("BUTTON"),
                    WS_TABSTOP | BS_OWNERDRAW as u32,
                    match id {
                        Stop => "停止",
                        Check => "无输入自检",
                        Primary => "启动脚本",
                        _ => "深色模式",
                    },
                ),
                Daemon => (
                    w!("BUTTON"),
                    WS_TABSTOP | BS_AUTOCHECKBOX as u32,
                    "窗口重新打开后继续",
                ),
                Backend => (
                    w!("COMBOBOX"),
                    WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32 | CBS_HASSTRINGS as u32,
                    "",
                ),
                Ledger => (
                    w!("LISTBOX"),
                    WS_TABSTOP | WS_VSCROLL | WS_HSCROLL | LBS_NOINTEGRALHEIGHT as u32,
                    "",
                ),
                Empty => (w!("STATIC"), 0, "启动或自检后，这里会显示运行日志。"),
                _ => (
                    w!("STATIC"),
                    0x200
                        | 0x4000
                        | 0x80
                        | if matches!(id, Jumps | Hit | Land | Elapsed | Shortcuts) {
                            2
                        } else if id == Phase {
                            1
                        } else {
                            0
                        },
                    "",
                ),
            };
            let child = CreateWindowExW(
                0,
                class,
                wide(caption).as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                0,
                0,
                1,
                1,
                hwnd,
                id as usize as HMENU,
                GetModuleHandleW(null()),
                null(),
            );
            if child.is_null() {
                return Err(io::Error::last_os_error().into());
            }
            let annotate = |prop, s: &str| {
                v.annotations.SetHwndPropStr(
                    ComHwnd(child),
                    OBJID_CLIENT as u32,
                    0,
                    prop,
                    PCWSTR(wide(s).as_ptr()),
                )
            };
            annotate(AutomationId_Property_GUID, name)?;
            if id == Backend || id == Ledger {
                annotate(
                    PROPID_ACC_NAME,
                    if id == Backend {
                        "识别后端"
                    } else {
                        "最近运行日志"
                    },
                )?;
            }
        }
        for (label, _) in BACKENDS {
            v.send(Backend, CB_ADDSTRING, 0, wide(label).as_ptr() as isize);
        }
        v.send(Backend, CB_SETCURSEL, 0, 0);
        v.send(Daemon, BM_SETCHECK, BST_CHECKED as usize, 0);
        Ok(v)
    }
    unsafe fn child(&self, id: Id) -> HWND {
        GetDlgItem(self.hwnd, id as i32)
    }
    unsafe fn send(&self, id: Id, msg: u32, wp: usize, lp: isize) -> isize {
        SendMessageW(self.child(id), msg, wp, lp)
    }
    unsafe fn text(&mut self, id: Id, s: &str) {
        if self.text.get(&id).is_none_or(|old| old != s) {
            if self.motion
                && !s.is_empty()
                && matches!(id, Phase | Score | Notice)
                && self.text.contains_key(&id)
            {
                self.reveals.insert(id, Instant::now());
                // Restarting a reveal uncovers the background behind the old, wider text.
                SCENE.with_borrow(|scene| {
                    if let Some(scene) = scene {
                        InvalidateRect(self.hwnd, &scene.layout.bounds(id), 0);
                    }
                });
            }
            SetWindowTextW(self.child(id), wide(s).as_ptr());
            self.text.insert(id, s.into());
        }
    }
    unsafe fn visible(&self, id: Id, show: bool) {
        let h = self.child(id);
        if (GetWindowLongW(h, GWL_STYLE) as u32 & WS_VISIBLE != 0) != show {
            ShowWindow(h, if show { SW_SHOWNA } else { SW_HIDE });
        }
    }
    unsafe fn enabled(&self, id: Id, on: bool) {
        let h = self.child(id);
        if (IsWindowEnabled(h) != 0) != on {
            EnableWindow(h, on as i32);
        }
    }
    unsafe fn layout(&mut self, notice: bool) {
        let mut r = RECT::default();
        GetClientRect(self.hwnd, &mut r);
        if r.right <= 0 || r.bottom <= 0 {
            return;
        }
        let dpi = GetDpiForWindow(self.hwnd);
        let (layout, previous, old_fonts) = SCENE.with_borrow_mut(|slot| {
            let previous = slot.as_ref().map(|s| s.layout);
            let ratios = previous.map_or([0.58, 0.66, 0.66], |l| l.ratios);
            let layout = Layout::new(r.right, r.bottom, dpi, notice, ratios);
            let colors = Colors::new(self.dark);
            let old_fonts = if let Some(s) = slot {
                let changed = s.layout.scale != layout.scale;
                s.layout = layout;
                s.colors = colors;
                s.notice = notice;
                s.motion = self.motion;
                s.transition = self.transition;
                changed.then(|| std::mem::replace(&mut s.fonts, Fonts::new(layout)))
            } else {
                *slot = Some(Scene {
                    layout,
                    colors,
                    fonts: Fonts::new(layout),
                    motion: self.motion,
                    transition: self.transition,
                    frame_time: Instant::now(),
                    landing: None,
                    notice,
                    drag: None,
                });
                None
            };
            (layout, previous, old_fonts)
        });
        place_controls(
            self.hwnd,
            layout,
            previous,
            previous.is_none() || old_fonts.is_some(),
        );
        // Child windows may select the previous fonts until WM_SETFONT returns.
        drop(old_fonts);
    }
    unsafe fn title_theme(&self) {
        let dark = self.dark as i32;
        DwmSetWindowAttribute(self.hwnd, 20, (&dark as *const i32).cast(), 4);
    }
    unsafe fn sync(&mut self, s: &Console) {
        let notice = !s.notice.is_empty();
        if !SCENE.with_borrow(|scene| scene.as_ref().is_some_and(|v| v.notice == notice)) {
            self.layout(notice);
        }
        let scene_moving = SCENE.with_borrow_mut(|v| {
            let scene = v.as_mut().unwrap();
            let previous = scene.frame_time;
            scene.frame_time = Instant::now();
            let changed = scene.landing != s.landed;
            scene.landing = s.landed;
            let entry_end = scene.transition + painting::ENTRY_DURATION;
            let end = scene
                .landing
                .map_or(entry_end, |j| entry_end.max(j.started + j.duration()));
            // Include the first tick past the deadline so the resting frame is painted.
            if scene.motion && previous < entry_end {
                InvalidateRect(self.hwnd, null(), 0);
            } else if changed || (scene.motion && previous < end) {
                InvalidateRect(self.hwnd, &scene.layout.diagram_pixels(), 0);
            }
            scene.motion && scene.frame_time < end
        });

        let (phase, note) = s.labels();
        self.text(Phase, phase);
        self.text(PhaseNote, note);
        self.text(Notice, &s.notice);
        self.visible(Notice, notice);
        self.text(
            Theme,
            if self.dark {
                "浅色模式"
            } else {
                "深色模式"
            },
        );
        self.text(Primary, s.primary_label());
        self.enabled(Primary, s.primary_enabled());
        self.enabled(Stop, s.mode == Mode::Live);
        self.enabled(Check, s.ready && s.mode == Mode::Idle);
        self.enabled(Backend, s.mode == Mode::Idle);
        self.enabled(Daemon, s.mode == Mode::Idle);
        let st = &s.status;
        for (id, key) in [
            (Score, "score"),
            (Jumps, "jumps"),
            (Hit, "hit_rate"),
            (Land, "landing_rate"),
        ] {
            let text = st[key].as_f64().map(|v| {
                if matches!(id, Hit | Land) {
                    format!("{v:.1}%")
                } else {
                    format!("{v:.0}")
                }
            });
            self.text(id, text.as_deref().unwrap_or("—"));
        }
        self.text(
            Elapsed,
            &s.elapsed(Instant::now())
                .map(|v| {
                    let n = v as u64;
                    format!("{:02}:{:02}:{:02}", n / 3600, n / 60 % 60, n % 60)
                })
                .unwrap_or_else(|| "—".into()),
        );
        self.text(
            ActualBackend,
            match st["mask_backend"].as_str() {
                Some("pending") => "等待首帧",
                Some("cuda" | "gpu") => "CUDA",
                Some("simd") => "CPU SIMD",
                Some("cpu") => "CPU",
                Some("opencl") => "OpenCL",
                _ => "—",
            },
        );
        self.text(
            Kernel,
            if st["background_kernel"] == "rust" {
                "Rust"
            } else {
                "—"
            },
        );
        self.text(
            Shortcuts,
            &format!(
                "{} 暂停  /  F8 停止",
                st["pause_hotkey"].as_str().unwrap_or("Alt+P")
            ),
        );
        self.text(Window, &s.window());
        self.text(
            Countdown,
            &st["countdown_s"]
                .as_f64()
                .filter(|_| matches!(s.phase.as_str(), "countdown" | "resuming"))
                .map(|v| format!("{v:.1} s"))
                .unwrap_or_else(|| "—".into()),
        );
        if self.log_sequence > s.log_sequence {
            self.send(Ledger, LB_RESETCONTENT, 0, 0);
            self.log_sequence = 0;
        }
        if self.log_sequence < s.log_sequence {
            let count = self.send(Ledger, LB_GETCOUNT, 0, 0);
            let top = self.send(Ledger, LB_GETTOPINDEX, 0, 0);
            let mut r = RECT::default();
            GetClientRect(self.child(Ledger), &mut r);
            let tail =
                top + (r.bottom as isize / self.send(Ledger, LB_GETITEMHEIGHT, 0, 0).max(1)) + 1
                    >= count;
            let delta = (s.log_sequence - self.log_sequence).min(s.logs.len() as u64) as usize;
            for line in s.logs.iter().skip(s.logs.len() - delta) {
                if self.send(Ledger, LB_GETCOUNT, 0, 0) >= 60 {
                    self.send(Ledger, LB_DELETESTRING, 0, 0);
                }
                self.send(Ledger, LB_ADDSTRING, 0, wide(line).as_ptr() as isize);
            }
            let dc = GetDC(self.child(Ledger));
            let font = self.send(Ledger, WM_GETFONT, 0, 0);
            let old = SelectObject(dc, font as HGDIOBJ);
            let extent = s
                .logs
                .iter()
                .map(|line| {
                    let v = wide(line);
                    let mut size = SIZE::default();
                    GetTextExtentPoint32W(dc, v.as_ptr(), (v.len() - 1) as i32, &mut size);
                    size.cx
                })
                .max()
                .unwrap_or(0);
            SelectObject(dc, old);
            ReleaseDC(self.child(Ledger), dc);
            self.send(Ledger, LB_SETHORIZONTALEXTENT, extent as usize + 5, 0);
            if tail {
                self.send(Ledger, LB_SETTOPINDEX, s.logs.len().saturating_sub(1), 0);
            }
            self.log_sequence = s.log_sequence;
        }
        self.visible(Ledger, !s.logs.is_empty());
        self.visible(Empty, s.logs.is_empty());
        self.reveals.retain(|id, t| {
            let h = GetDlgItem(self.hwnd, *id as i32);
            let p = t.elapsed().as_secs_f32() / 0.26;
            if !self.motion || p >= 1. || self.text[id].is_empty() {
                SetWindowRgn(h, null_mut(), 1);
                return false;
            }
            let mut r = RECT::default();
            GetClientRect(h, &mut r);
            let extent = (r.right as f32 * (p * p * (3. - 2. * p))) as i32;
            let region = CreateRectRgn(0, 0, extent.max(1), r.bottom);
            if SetWindowRgn(h, region, 1) == 0 {
                DeleteObject(region);
            }
            true
        });
        let animate = scene_moving || !self.reveals.is_empty();
        let interval = if animate {
            USER_TIMER_MINIMUM
        } else if s.mode != Mode::Idle {
            80
        } else {
            0
        };
        if self.timer != interval {
            KillTimer(self.hwnd, 1);
            if interval != 0 {
                SetTimer(self.hwnd, 1, interval, None);
            }
            self.timer = interval;
        }
    }
}
impl Drop for View {
    fn drop(&mut self) {
        unsafe {
            for &(id, _) in IDS {
                let h = self.child(id);
                if !h.is_null() {
                    let _ = self.annotations.ClearHwndProps(
                        ComHwnd(h),
                        OBJID_CLIENT as u32,
                        0,
                        &[AutomationId_Property_GUID, PROPID_ACC_NAME],
                    );
                }
            }
            DestroyWindow(self.hwnd);
            SCENE.with_borrow_mut(|s| *s = None);
            if !self.font_resource.is_null() {
                RemoveFontMemResourceEx(self.font_resource);
            }
        }
    }
}
pub fn run(mut host: Host) -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        struct Com;
        impl Drop for Com {
            fn drop(&mut self) {
                unsafe { CoUninitialize() }
            }
        }
        let _com = Com;
        let instance = GetModuleHandleW(null());
        let class = w!("JumpBotConsole");
        // Icon group 1 also supplies the executable icon used by the Windows shell.
        let icon = LoadIconW(instance, std::ptr::without_provenance(1));
        if icon.is_null() {
            return Err(io::Error::last_os_error().into());
        }
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            hIcon: icon,
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            lpszClassName: class,
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            return Err(io::Error::last_os_error().into());
        }
        let dpi = GetDpiForSystem() as i32;
        let hwnd = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            class,
            w!("跳一跳控制台 / JumpBot"),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            (1200 * dpi / 96).min(GetSystemMetrics(SM_CXSCREEN)),
            (830 * dpi / 96).min(GetSystemMetrics(SM_CYSCREEN) - 50),
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            return Err(io::Error::last_os_error().into());
        }
        let mut v = View::new(hwnd)?;
        v.title_theme();
        v.sync(&host.ui);
        ShowWindow(hwnd, SW_SHOWNORMAL);
        let mut msg = MSG::default();
        loop {
            let got = GetMessageW(&mut msg, null_mut(), 0, 0);
            if got < 0 {
                return Err(io::Error::last_os_error().into());
            }
            if got == 0 {
                break;
            }
            if msg.message == WM_KEYDOWN && msg.wParam == VK_ESCAPE as usize && GetCapture() == hwnd
            {
                end_drag(hwnd);
                continue;
            }
            match (msg.hwnd == hwnd, msg.message) {
                (true, ACTION) if !host.closing => {
                    let Some(id) = Id::from_raw(msg.wParam) else {
                        continue;
                    };
                    if v.child(id) != msg.lParam as HWND || IsWindowEnabled(v.child(id)) == 0 {
                        continue;
                    }
                    match id {
                        Primary => {
                            let index = v.send(Backend, CB_GETCURSEL, 0, 0) as usize;
                            if index < BACKENDS.len() {
                                host.primary(
                                    index,
                                    v.send(Daemon, BM_GETCHECK, 0, 0) == BST_CHECKED as isize,
                                );
                            }
                        }
                        Stop => host.stop(),
                        Check => host.start(2, false, true),
                        Theme => {
                            v.dark = !v.dark;
                            v.title_theme();
                            v.manual = true;
                            v.transition = Instant::now();
                            v.layout(!host.ui.notice.is_empty());
                        }
                        _ => {}
                    }
                }
                (true, CLOSE) => {
                    host.closing = true;
                    host.stop();
                }
                (true, RESIZE) => v.layout(!host.ui.notice.is_empty()),
                (true, SETTINGS) => {
                    if !v.manual {
                        let dark = system_dark();
                        if dark != v.dark {
                            v.dark = dark;
                            v.title_theme();
                        }
                    }
                    v.motion = system_motion();
                    v.layout(!host.ui.notice.is_empty());
                }
                (true, WM_TIMER) => host.poll(),
                _ => {
                    if IsDialogMessageW(hwnd, &msg) == 0 {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                    continue;
                }
            }
            if host.closing && host.session.is_none() {
                break;
            }
            v.sync(&host.ui);
        }
        Ok(())
    }
}
pub fn startup_error(s: &str) {
    unsafe {
        MessageBoxW(null_mut(), wide(s).as_ptr(), w!("JumpBot"), MB_ICONERROR);
    }
}
