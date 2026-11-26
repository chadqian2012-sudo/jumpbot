use crate::{
    console::{Console, Mode},
    motion::{spring_curve, Roller, Spring},
    painting::{
        self, Colors, Divider, Fonts, Layout, PhaseTone, Scene, DEFAULT_RATIOS, SCENE,
    },
    Host, BACKENDS,
};
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    collections::HashMap,
    fs, io,
    path::PathBuf,
    ptr::{null, null_mut},
    time::{Duration, Instant},
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
        Storage::Xps::PrintWindow,
        System::{LibraryLoader::GetModuleHandleW, Registry::*},
        UI::{
            Controls::{
                SetWindowTheme, BST_CHECKED, DRAWITEMSTRUCT, MEASUREITEMSTRUCT, ODT_LISTBOX,
                ODT_STATIC, WM_MOUSELEAVE,
            },
            HiDpi::*,
            Input::KeyboardAndMouse::{
                EnableWindow, GetCapture, GetFocus, IsWindowEnabled, ReleaseCapture, SetCapture,
                SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE, VK_SPACE,
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
    LogSmaller,
    LogLarger,
    LogCopy,
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
    (LogCopy, "log-copy"),
    (LogSmaller, "log-font-smaller"),
    (LogLarger, "log-font-larger"),
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
const SETTLE: u32 = WM_APP + 5;
const MOVED: u32 = WM_APP + 6;
const WAKE: u32 = WM_APP + 7;
const RESET_DIVIDER: u32 = WM_APP + 8;

#[link(name = "user32")]
extern "system" {
    fn OpenClipboard(hwnd: HWND) -> i32;
    fn EmptyClipboard() -> i32;
    fn SetClipboardData(format: u32, mem: HANDLE) -> HANDLE;
    fn CloseClipboard() -> i32;
}

#[link(name = "winmm")]
extern "system" {
    fn timeBeginPeriod(period: u32) -> u32;
    fn timeEndPeriod(period: u32) -> u32;
}

/// Raises the system timer resolution only while animations run, so the
/// message-pumping frame wait can hit high refresh rates without busy-waiting.
struct HiResTimer(bool);
impl HiResTimer {
    fn set(&mut self, on: bool) {
        if self.0 != on {
            unsafe {
                if on {
                    timeBeginPeriod(1);
                } else {
                    timeEndPeriod(1);
                }
            }
            self.0 = on;
        }
    }
}
impl Drop for HiResTimer {
    fn drop(&mut self) {
        self.set(false);
    }
}

unsafe fn frame_interval(hwnd: HWND) -> Duration {
    let dc = GetDC(hwnd);
    let hz = if dc.is_null() {
        60
    } else {
        let hz = GetDeviceCaps(dc, VREFRESH as _);
        ReleaseDC(hwnd, dc);
        hz
    };
    let hz = if hz > 1 { hz.clamp(50, 240) } else { 60 };
    Duration::from_secs_f64(1.0 / hz as f64)
}

#[link(name = "kernel32")]
extern "system" {
    fn GlobalAlloc(flags: u32, bytes: usize) -> HANDLE;
    fn GlobalLock(mem: HANDLE) -> *mut std::ffi::c_void;
    fn GlobalUnlock(mem: HANDLE) -> i32;
    fn GlobalFree(mem: HANDLE) -> HANDLE;
}

unsafe fn copy_to_clipboard(hwnd: HWND, text: &str) -> bool {
    if text.is_empty() || OpenClipboard(hwnd) == 0 {
        return false;
    }
    let mut ok = false;
    if EmptyClipboard() != 0 {
        let utf16: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        let bytes = utf16.len() * 2;
        let mem = GlobalAlloc(0x0002 /* GMEM_MOVEABLE */, bytes);
        if !mem.is_null() {
            let ptr = GlobalLock(mem) as *mut u16;
            if !ptr.is_null() {
                std::ptr::copy_nonoverlapping(utf16.as_ptr(), ptr, utf16.len());
                GlobalUnlock(mem);
                if !SetClipboardData(13 /* CF_UNICODETEXT */, mem).is_null() {
                    ok = true;
                } else {
                    GlobalFree(mem);
                }
            } else {
                GlobalFree(mem);
            }
        }
    }
    CloseClipboard();
    ok
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct UiPrefs {
    dark: Option<bool>,
    manual_theme: bool,
    ratios: [f32; 3],
    log_size: i32,
    width: i32,
    height: i32,
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self {
            dark: None,
            manual_theme: false,
            ratios: DEFAULT_RATIOS,
            log_size: 13,
            width: 1200,
            height: 830,
        }
    }
}

impl UiPrefs {
    fn sanitized(mut self) -> Self {
        for r in &mut self.ratios {
            if !r.is_finite() {
                *r = 0.62;
            }
            *r = r.clamp(0.15, 0.85);
        }
        self.log_size = self.log_size.clamp(10, 20);
        self.width = self.width.clamp(700, 3840);
        self.height = self.height.clamp(550, 2160);
        self
    }
    fn path() -> Option<PathBuf> {
        if std::env::var_os("JUMPBOT_NO_INPUT").is_some() {
            return None;
        }
        std::env::current_exe()
            .ok()
            .map(|p| p.with_file_name("ui-state.json"))
    }
    fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<Self>(&s).ok())
            .map(Self::sanitized)
            .unwrap_or_default()
    }
    fn save(&self) {
        let Some(path) = Self::path() else {
            return;
        };
        if let Ok(json) = serde_json::to_vec_pretty(&self.clone().sanitized()) {
            let _ = fs::write(path, json);
        }
    }
}

struct FadeSurface {
    overlay: HWND,
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
}

thread_local! {
    static FADE_SURFACE: RefCell<Option<FadeSurface>> = const { RefCell::new(None) };
    static CONTROL_PROCS: RefCell<HashMap<isize, WNDPROC>> = RefCell::new(HashMap::new());
    static HOVERED_CONTROLS: RefCell<HashMap<isize, bool>> = RefCell::new(HashMap::new());
}

unsafe fn dismiss_fade_surface() {
    let surf = FADE_SURFACE.with_borrow_mut(Option::take);
    if let Some(surf) = surf {
        if !surf.overlay.is_null() {
            ShowWindow(surf.overlay, SW_HIDE);
            SetWindowRgn(surf.overlay, null_mut(), 0);
        }
        SelectObject(surf.dc, surf.old);
        DeleteObject(surf.bmp);
        DeleteDC(surf.dc);
    }
}

unsafe extern "system" fn control_wndproc(h: HWND, msg: u32, wp: usize, lp: isize) -> isize {
    let key = h as isize;
    let orig = CONTROL_PROCS.with_borrow(|m| m.get(&key).copied().flatten());
    if msg == WM_ERASEBKGND {
        return 1;
    }
    let id = Id::from_raw(GetDlgCtrlID(h) as usize);
    if msg == WM_PAINT && matches!(id, Some(Phase | Notice | Daemon | Backend)) {
        let mut ps = PAINTSTRUCT::default();
        let dc = BeginPaint(h, &mut ps);
        let focused = GetFocus() == h;
        let disabled = IsWindowEnabled(h) == 0;
        match id {
            Some(Phase) => painting::phase_badge(h, dc),
            Some(Notice) => painting::notice_banner(h, dc),
            Some(Daemon) => painting::daemon_checkbox(h, dc, focused, disabled),
            Some(Backend) => painting::backend_combo(h, dc, focused, disabled),
            _ => {}
        }
        EndPaint(h, &ps);
        return 0;
    }
    let mut wake = false;
    if let Some(id) = id.filter(|id| !matches!(id, Phase | Notice)) {
        match msg {
            WM_MOUSEMOVE => {
                let first = HOVERED_CONTROLS.with_borrow_mut(|m| {
                    let entry = m.entry(key).or_insert(false);
                    if !*entry {
                        *entry = true;
                        true
                    } else {
                        false
                    }
                });
                if first {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: h,
                        dwHoverTime: 0,
                    };
                    TrackMouseEvent(&mut tme);
                    SCENE.with_borrow_mut(|slot| {
                        if let Some(s) = slot {
                            let anim = s.buttons.entry(id).or_default();
                            if s.motion {
                                anim.hover.set_target(1.0);
                            } else {
                                anim.hover.snap(1.0);
                            }
                        }
                    });
                    InvalidateRect(h, null(), 0);
                    wake = true;
                }
            }
            WM_MOUSELEAVE => {
                HOVERED_CONTROLS.with_borrow_mut(|m| {
                    m.insert(key, false);
                });
                SCENE.with_borrow_mut(|slot| {
                    if let Some(s) = slot {
                        let anim = s.buttons.entry(id).or_default();
                        if s.motion {
                            anim.hover.set_target(0.0);
                            anim.press.set_target(0.0);
                        } else {
                            anim.hover.snap(0.0);
                            anim.press.snap(0.0);
                        }
                    }
                });
                InvalidateRect(h, null(), 0);
                wake = true;
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                SCENE.with_borrow_mut(|slot| {
                    if let Some(s) = slot {
                        let anim = s.buttons.entry(id).or_default();
                        if s.motion {
                            anim.press.set_target(1.0);
                        } else {
                            anim.press.snap(1.0);
                        }
                    }
                });
                InvalidateRect(h, null(), 0);
                wake = true;
            }
            WM_LBUTTONUP | WM_CAPTURECHANGED | WM_KILLFOCUS => {
                SCENE.with_borrow_mut(|slot| {
                    if let Some(s) = slot {
                        let anim = s.buttons.entry(id).or_default();
                        if s.motion {
                            anim.press.set_target(0.0);
                        } else {
                            anim.press.snap(0.0);
                        }
                    }
                });
                InvalidateRect(h, null(), 0);
                wake = true;
            }
            WM_SETFOCUS => {
                InvalidateRect(h, null(), 0);
            }
            WM_KEYDOWN if wp == VK_SPACE as usize => {
                SCENE.with_borrow_mut(|slot| {
                    if let Some(s) = slot {
                        let anim = s.buttons.entry(id).or_default();
                        if s.motion {
                            anim.press.set_target(1.0);
                        } else {
                            anim.press.snap(1.0);
                        }
                    }
                });
                InvalidateRect(h, null(), 0);
                wake = true;
            }
            WM_KEYUP if wp == VK_SPACE as usize => {
                SCENE.with_borrow_mut(|slot| {
                    if let Some(s) = slot {
                        let anim = s.buttons.entry(id).or_default();
                        if s.motion {
                            anim.press.set_target(0.0);
                        } else {
                            anim.press.snap(0.0);
                        }
                    }
                });
                InvalidateRect(h, null(), 0);
                wake = true;
            }
            _ => {}
        }
    }
    let res = if let Some(orig) = orig {
        CallWindowProcW(Some(orig), h, msg, wp, lp)
    } else {
        DefWindowProcW(h, msg, wp, lp)
    };
    if id == Some(Daemon) && matches!(msg, BM_SETCHECK | WM_LBUTTONUP | WM_KEYUP) {
        let checked = SendMessageW(h, BM_GETCHECK, 0, 0) == BST_CHECKED as isize;
        let target = if checked { 1.0 } else { 0.0 };
        SCENE.with_borrow_mut(|slot| {
            if let Some(s) = slot {
                if s.motion {
                    s.daemon_check.set_target(target);
                } else {
                    s.daemon_check.snap(target);
                }
            }
        });
        InvalidateRect(h, null(), 0);
        wake = true;
    }
    if wake {
        let parent = GetParent(h);
        if !parent.is_null() {
            PostMessageW(parent, WAKE, 0, 0);
        }
    }
    res
}

unsafe extern "system" fn fade_wndproc(h: HWND, msg: u32, wp: usize, lp: isize) -> isize {
    match msg {
        WM_NCHITTEST => HTTRANSPARENT as isize,
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(h, &mut ps);
            FADE_SURFACE.with_borrow(|slot| {
                if let Some(surf) = slot {
                    let w = ps.rcPaint.right - ps.rcPaint.left;
                    let h = ps.rcPaint.bottom - ps.rcPaint.top;
                    if w > 0 && h > 0 {
                        BitBlt(
                            dc,
                            ps.rcPaint.left,
                            ps.rcPaint.top,
                            w,
                            h,
                            surf.dc,
                            ps.rcPaint.left,
                            ps.rcPaint.top,
                            SRCCOPY,
                        );
                    }
                }
            });
            EndPaint(h, &ps);
            0
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}

fn is_roller(id: Id) -> bool {
    matches!(id, Score | Jumps | Hit | Land | Elapsed | Countdown)
}
unsafe fn divider_cursor(d: Divider) {
    let shape = if d == Divider::Columns {
        IDC_SIZEWE
    } else {
        IDC_SIZENS
    };
    SetCursor(LoadCursorW(null_mut(), shape));
}

unsafe fn end_drag(hwnd: HWND) {
    let band = SCENE.with_borrow_mut(|s| {
        let s = s.as_mut()?;
        let (d, _) = s.drag.take()?;
        Some(s.layout.pixels(s.layout.band(d)))
    });
    if let Some(rect) = band {
        InvalidateRect(hwnd, &rect, 0);
        // ReleaseCapture synchronously sends WM_CAPTURECHANGED back into this procedure.
        if GetCapture() == hwnd {
            ReleaseCapture();
        }
        PostMessageW(hwnd, SETTLE, 0, 0);
    }
}

unsafe fn log_position(list: HWND) -> (isize, bool) {
    let top = SendMessageW(list, LB_GETTOPINDEX, 0, 0);
    let mut r = RECT::default();
    GetClientRect(list, &mut r);
    let page = (r.bottom as isize / SendMessageW(list, LB_GETITEMHEIGHT, 0, 0).max(1)).max(1);
    (top, top + page >= SendMessageW(list, LB_GETCOUNT, 0, 0))
}
unsafe fn restore_log_position(list: HWND, top: isize, tail: bool) {
    let n = SendMessageW(list, LB_GETCOUNT, 0, 0);
    if n > 0 {
        SendMessageW(
            list,
            LB_SETTOPINDEX,
            if tail { n - 1 } else { top } as usize,
            0,
        );
    }
}

unsafe fn place_controls(
    hwnd: HWND,
    layout: Layout,
    previous: Option<Layout>,
    fonts_changed: bool,
) {
    let mut moved: Vec<(Id, HWND, RECT)> = Vec::with_capacity(IDS.len());
    for &(id, _) in IDS {
        let child = GetDlgItem(hwnd, id as i32);
        if fonts_changed {
            let font = SCENE.with_borrow(|s| s.as_ref().unwrap().fonts.control(id));
            SendMessageW(child, WM_SETFONT, font as usize, 0);
            if id == Backend {
                SendMessageW(child, CB_SETITEMHEIGHT, usize::MAX, layout.px(28) as isize);
                SendMessageW(
                    child,
                    CB_SETITEMHEIGHT,
                    0,
                    layout.px(26).max(18) as isize,
                );
            }
        }
        let r = layout.bounds(id);
        if previous.is_none_or(|old| EqualRect(&old.bounds(id), &r) == 0) {
            moved.push((id, child, r));
        }
    }
    // Keep the same content in view when a divider or the notice changes its height.
    let ledger_pos = (!fonts_changed)
        .then(|| {
            moved
                .iter()
                .find(|(id, _, _)| *id == Ledger)
                .map(|&(_, child, _)| (child, log_position(child)))
        })
        .flatten();
    if !moved.is_empty() {
        let mut batched = false;
        let mut hdwp = BeginDeferWindowPos(moved.len() as i32);
        if !hdwp.is_null() {
            let mut ok = true;
            for &(_, child, r) in &moved {
                hdwp = DeferWindowPos(
                    hdwp,
                    child,
                    null_mut(),
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOCOPYBITS | SWP_NOREDRAW,
                );
                if hdwp.is_null() {
                    ok = false;
                    break;
                }
            }
            if ok && EndDeferWindowPos(hdwp) != 0 {
                batched = true;
            }
        }
        if !batched {
            for &(_, child, r) in &moved {
                MoveWindow(child, r.left, r.top, r.right - r.left, r.bottom - r.top, 0);
            }
        }
    }
    if let Some((child, (top, tail))) = ledger_pos {
        restore_log_position(child, top, tail);
    }
    if previous.is_none() || fonts_changed {
        RedrawWindow(
            hwnd,
            null(),
            null_mut(),
            RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN,
        );
        return;
    }
    if moved.is_empty() {
        // Same geometry: nothing to repaint (avoids a second full frame after WM_SIZE).
        return;
    }
    // Children that kept their rectangle stay valid thanks to WS_CLIPCHILDREN, so only
    // the parent surface and the moved controls need a new frame.
    RedrawWindow(hwnd, null(), null_mut(), RDW_INVALIDATE | RDW_NOCHILDREN);
    for &(_, child, _) in &moved {
        RedrawWindow(
            child,
            null(),
            null_mut(),
            RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN,
        );
    }
}

unsafe fn set_divider_hover(hwnd: HWND, hovered: Option<Divider>) {
    let mut dirty = [None; 3];
    let mut changed = false;
    SCENE.with_borrow_mut(|slot| {
        let Some(s) = slot else { return };
        if s.hovered_divider == hovered {
            return;
        }
        let prev = s.hovered_divider;
        s.hovered_divider = hovered;
        changed = true;
        for (i, d) in [Divider::Columns, Divider::LeftRow, Divider::RightRow]
            .into_iter()
            .enumerate()
        {
            let target = if hovered == Some(d) { 1.0 } else { 0.0 };
            if s.motion {
                s.divider_hover[i].set_target(target);
            } else {
                s.divider_hover[i].snap(target);
            }
            if prev == Some(d) || hovered == Some(d) {
                dirty[i] = Some(s.layout.pixels(s.layout.band(d)));
            }
        }
    });
    for rect in dirty.into_iter().flatten() {
        InvalidateRect(hwnd, &rect, 0);
    }
    if changed {
        PostMessageW(hwnd, WAKE, 0, 0);
    }
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
        WM_LBUTTONDBLCLK => {
            let x = lp as i16 as i32;
            let y = (lp >> 16) as i16 as i32;
            let hit = SCENE.with_borrow(|s| s.as_ref()?.layout.hit(x, y));
            if let Some(d) = hit {
                PostMessageW(h, RESET_DIVIDER, d as usize, 0);
                return 0;
            }
            DefWindowProcW(h, msg, wp, lp)
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
                        let rect = SCENE.with_borrow_mut(|s| {
                            let s = s.as_mut().unwrap();
                            s.drag = Some((d, offset));
                            s.layout.pixels(s.layout.band(d))
                        });
                        InvalidateRect(h, &rect, 0);
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
                        .move_to(d, old.logical_axis(d, x, y) - offset, s.motion)
                        .then_some((d, old, s.layout))
                });
                if let Some((d, old, layout)) = changed {
                    if old.position(d) != layout.position(d) {
                        place_controls(h, layout, Some(old), false);
                    } else if old.stretch != layout.stretch {
                        InvalidateRect(h, &layout.pixels(layout.band(d)), 0);
                    }
                }
                if msg == WM_LBUTTONUP {
                    end_drag(h);
                }
                return 0;
            } else if msg == WM_MOUSEMOVE {
                let hit = SCENE.with_borrow(|s| s.as_ref()?.layout.hit(x, y));
                if hit.is_some() {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: h,
                        dwHoverTime: 0,
                    };
                    TrackMouseEvent(&mut tme);
                }
                set_divider_hover(h, hit);
            }
            DefWindowProcW(h, msg, wp, lp)
        }
        WM_MOUSELEAVE => {
            set_divider_hover(h, None);
            0
        }
        WM_CANCELMODE => {
            end_drag(h);
            DefWindowProcW(h, msg, wp, lp)
        }
        WM_CAPTURECHANGED => {
            let band = SCENE.with_borrow_mut(|s| {
                let s = s.as_mut()?;
                let (d, _) = s.drag.take()?;
                Some(s.layout.pixels(s.layout.band(d)))
            });
            if let Some(rect) = band {
                InvalidateRect(h, &rect, 0);
                PostMessageW(h, SETTLE, 0, 0);
            }
            0
        }
        WM_PAINT => {
            painting::paint(h);
            0
        }
        WM_ERASEBKGND => 1,
        WM_MEASUREITEM if lp != 0 => {
            let m = &mut *(lp as *mut MEASUREITEMSTRUCT);
            if m.CtlID == Ledger as u32 {
                m.itemHeight = 20;
                1
            } else {
                0
            }
        }
        WM_DRAWITEM if lp != 0 => {
            let d = &*(lp as *const DRAWITEMSTRUCT);
            match d.CtlType {
                ODT_STATIC => painting::roller(d),
                ODT_LISTBOX => painting::ledger_item(d),
                _ => painting::button(d),
            }
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
                        } else if id == Some(Empty) {
                            s.colors.muted
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
        WM_MOVE => {
            dismiss_fade_surface();
            PostMessageW(h, MOVED, 0, 0);
            DefWindowProcW(h, msg, wp, lp)
        }
        WM_SIZE => {
            end_drag(h);
            dismiss_fade_surface();
            if wp != SIZE_MINIMIZED as usize {
                let mut r = RECT::default();
                GetClientRect(h, &mut r);
                if r.right > 0 && r.bottom > 0 {
                    let dpi = GetDpiForWindow(h);
                    let mut live = None;
                    let mut old_fonts = None;
                    SCENE.with_borrow_mut(|slot| {
                        if let Some(s) = slot {
                            let old = s.layout;
                            let next = Layout::new(
                                r.right,
                                r.bottom,
                                dpi,
                                old.top,
                                old.ratios,
                                [0; 3],
                            );
                            if next.scale != old.scale {
                                old_fonts = Some(std::mem::replace(
                                    &mut s.fonts,
                                    Fonts::new(next, s.log_size),
                                ));
                            }
                            s.layout = next;
                            live = Some((old, next));
                        }
                    });
                    if let Some((old, next)) = live {
                        place_controls(h, next, Some(old), old_fonts.is_some());
                        UpdateWindow(h);
                    }
                    drop(old_fonts);
                }
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
struct Fade {
    overlay: HWND,
    start: Instant,
    center: POINT,
    max_r: f32,
    w: i32,
    h: i32,
    last_r: i32,
    last_alpha: u8,
}

struct View {
    hwnd: HWND,
    fade_overlay: HWND,
    annotations: IAccPropServices,
    text: HashMap<Id, String>,
    log_sequence: u64,
    log_size: i32,
    log_extent: i32,
    last_scale: f32,
    wide_scratch: Vec<u16>,
    dark: bool,
    manual: bool,
    motion: bool,
    font_resource: HANDLE,
    timer: u32,
    transition: Instant,
    top: Spring,
    divider_springs: [Option<Spring>; 3],
    ratio_springs: [Option<Spring>; 3],
    initial_ratios: [f32; 3],
    countdown_max: Option<f32>,
    copied_until: Option<Instant>,
    fade: Option<Fade>,
    last_frame: Instant,
}
impl View {
    unsafe fn new(hwnd: HWND, prefs: &UiPrefs) -> Result<Self, Box<dyn std::error::Error>> {
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
        let dark = if prefs.manual_theme {
            prefs.dark.unwrap_or_else(system_dark)
        } else {
            system_dark()
        };
        let v = Self {
            hwnd,
            fade_overlay: null_mut(),
            annotations,
            text: HashMap::new(),
            log_sequence: 0,
            log_size: prefs.log_size,
            log_extent: 0,
            last_scale: 0.0,
            wide_scratch: Vec::new(),
            dark,
            manual: prefs.manual_theme,
            motion: system_motion(),
            font_resource,
            timer: 0,
            transition: now,
            top: Spring::smooth(144.0),
            divider_springs: [None; 3],
            ratio_springs: [None; 3],
            initial_ratios: prefs.ratios,
            countdown_max: None,
            copied_until: None,
            fade: None,
            last_frame: now,
        };
        for &(id, name) in IDS {
            let (class, style, caption) = match id {
                LogCopy | LogSmaller | LogLarger | Theme | Primary | Stop | Check => (
                    w!("BUTTON"),
                    WS_TABSTOP | BS_OWNERDRAW as u32,
                    match id {
                        LogCopy => "复制",
                        LogSmaller => "A−",
                        LogLarger => "A+",
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
                    WS_TABSTOP
                        | WS_VSCROLL
                        | WS_HSCROLL
                        | LBS_NOINTEGRALHEIGHT as u32
                        | LBS_OWNERDRAWFIXED as u32
                        | LBS_HASSTRINGS as u32,
                    "",
                ),
                Empty => (
                    w!("STATIC"),
                    0x1 | 0x200, // SS_CENTER | SS_CENTERIMAGE: centered empty-state hint
                    "启动或自检后，这里会显示运行日志。",
                ),
                _ if is_roller(id) => (w!("STATIC"), 0x80 | 0x0D, ""),
                _ => (
                    w!("STATIC"),
                    0x200
                        | 0x4000
                        | 0x80
                        | if matches!(id, Shortcuts) {
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
            if matches!(
                id,
                LogCopy
                    | LogSmaller
                    | LogLarger
                    | Theme
                    | Primary
                    | Stop
                    | Check
                    | Daemon
                    | Backend
                    | Phase
                    | Notice
            ) {
                let prev = SetWindowLongPtrW(
                    child,
                    GWLP_WNDPROC,
                    control_wndproc as *const () as isize,
                );
                if prev != 0 {
                    let proc: WNDPROC = std::mem::transmute::<isize, WNDPROC>(prev);
                    CONTROL_PROCS.with_borrow_mut(|m| {
                        m.insert(child as isize, proc);
                    });
                }
            }
            let annotate = |prop, s: &str| {
                let w = wide(s);
                v.annotations.SetHwndPropStr(
                    ComHwnd(child),
                    OBJID_CLIENT as u32,
                    0,
                    prop,
                    PCWSTR(w.as_ptr()),
                )
            };
            annotate(AutomationId_Property_GUID, name)?;
            match id {
                Backend => annotate(PROPID_ACC_NAME, "识别后端")?,
                Ledger => annotate(PROPID_ACC_NAME, "最近运行日志")?,
                LogCopy => annotate(PROPID_ACC_NAME, "复制运行日志")?,
                LogSmaller => annotate(PROPID_ACC_NAME, "减小日志字号")?,
                LogLarger => annotate(PROPID_ACC_NAME, "增大日志字号")?,
                _ => {}
            }
        }
        for (label, _) in BACKENDS {
            v.send(Backend, CB_ADDSTRING, 0, wide(label).as_ptr() as isize);
        }
        v.send(Backend, CB_SETCURSEL, 0, 0);
        v.send(Daemon, BM_SETCHECK, BST_CHECKED as usize, 0);
        Ok(v)
    }
    unsafe fn save_prefs(&self) {
        let mut r = RECT::default();
        if GetWindowRect(self.hwnd, &mut r) == 0 {
            return;
        }
        let dpi = GetDpiForWindow(self.hwnd).max(96) as i32;
        let width = (r.right - r.left) * 96 / dpi;
        let height = (r.bottom - r.top) * 96 / dpi;
        let ratios = SCENE
            .with_borrow(|s| s.as_ref().map(|sc| sc.layout.ratios))
            .unwrap_or(self.initial_ratios);
        UiPrefs {
            dark: Some(self.dark),
            manual_theme: self.manual,
            ratios,
            log_size: self.log_size,
            width,
            height,
        }
        .save();
    }
    unsafe fn child(&self, id: Id) -> HWND {
        GetDlgItem(self.hwnd, id as i32)
    }
    unsafe fn send(&self, id: Id, msg: u32, wp: usize, lp: isize) -> isize {
        SendMessageW(self.child(id), msg, wp, lp)
    }
    unsafe fn text(&mut self, id: Id, s: &str) {
        if self.text.get(&id).is_none_or(|old| old != s) {
            if is_roller(id) {
                let animate = self.motion && self.text.contains_key(&id);
                SCENE.with_borrow_mut(|scene| {
                    if let Some(scene) = scene {
                        scene.rollers.entry(id).or_default().set(s, animate);
                    }
                });
                SetWindowTextW(self.child(id), wide(s).as_ptr());
                self.text.insert(id, s.into());
                InvalidateRect(self.child(id), null(), 0);
                return;
            }
            if id == Phase && self.text.contains_key(&Phase) {
                SCENE.with_borrow_mut(|scene| {
                    if let Some(scene) = scene {
                        if self.motion {
                            scene.phase_slide.snap(0.0);
                            scene.phase_slide.set_target(1.0);
                        } else {
                            scene.phase_slide.snap(1.0);
                        }
                    }
                });
            }
            SetWindowTextW(self.child(id), wide(s).as_ptr());
            self.text.insert(id, s.into());
            if matches!(id, Phase | Notice) {
                InvalidateRect(self.child(id), null(), 0);
            }
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
            if !on {
                SCENE.with_borrow_mut(|slot| {
                    if let Some(s) = slot {
                        if let Some(anim) = s.buttons.get_mut(&id) {
                            anim.hover.snap(0.0);
                            anim.press.snap(0.0);
                        }
                    }
                });
            }
            EnableWindow(h, on as i32);
        }
    }
    unsafe fn end_fade(&mut self) {
        self.fade = None;
        dismiss_fade_surface();
    }
    unsafe fn start_fade(&mut self) {
        self.end_fade();
        if !self.motion {
            return;
        }
        let mut r = RECT::default();
        GetClientRect(self.hwnd, &mut r);
        if r.right <= 0 || r.bottom <= 0 {
            return;
        }
        let Some(center) = SCENE.with_borrow(|s| s.as_ref().map(Scene::theme_thumb_center)) else {
            return;
        };
        let win_dc = GetDC(self.hwnd);
        let mem_dc = CreateCompatibleDC(win_dc);
        let bmp = CreateCompatibleBitmap(win_dc, r.right, r.bottom);
        ReleaseDC(self.hwnd, win_dc);
        if mem_dc.is_null() || bmp.is_null() {
            if !bmp.is_null() {
                DeleteObject(bmp);
            }
            if !mem_dc.is_null() {
                DeleteDC(mem_dc);
            }
            return;
        }
        let old = SelectObject(mem_dc, bmp);
        if PrintWindow(self.hwnd, mem_dc, 3) == 0 {
            SelectObject(mem_dc, old);
            DeleteObject(bmp);
            DeleteDC(mem_dc);
            return;
        }
        let mut pt = POINT { x: 0, y: 0 };
        ClientToScreen(self.hwnd, &mut pt);
        if self.fade_overlay.is_null() {
            self.fade_overlay = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                w!("JumpBotFade"),
                w!(""),
                WS_POPUP,
                pt.x,
                pt.y,
                r.right,
                r.bottom,
                self.hwnd,
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            );
        }
        let overlay = self.fade_overlay;
        if overlay.is_null() {
            SelectObject(mem_dc, old);
            DeleteObject(bmp);
            DeleteDC(mem_dc);
            return;
        }
        SetWindowRgn(overlay, null_mut(), 0);
        SetWindowPos(
            overlay,
            null_mut(),
            pt.x,
            pt.y,
            r.right,
            r.bottom,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
        FADE_SURFACE.with_borrow_mut(|slot| {
            *slot = Some(FadeSurface {
                overlay,
                dc: mem_dc,
                bmp,
                old,
            });
        });
        SetLayeredWindowAttributes(overlay, 0, 255, LWA_ALPHA);
        ShowWindow(overlay, SW_SHOWNOACTIVATE);
        InvalidateRect(overlay, null(), 0);
        UpdateWindow(overlay);

        let max_r = [(0, 0), (r.right, 0), (0, r.bottom), (r.right, r.bottom)]
            .into_iter()
            .map(|(cx, cy)| {
                let dx = (cx - center.x) as f32;
                let dy = (cy - center.y) as f32;
                (dx * dx + dy * dy).sqrt()
            })
            .fold(1.0_f32, f32::max);
        self.fade = Some(Fade {
            overlay,
            start: Instant::now(),
            center,
            max_r,
            w: r.right,
            h: r.bottom,
            last_r: -1,
            last_alpha: 255,
        });
    }
    unsafe fn settle(&mut self) {
        let Some(stretch) = SCENE.with_borrow(|s| s.as_ref().map(|s| s.layout.stretch)) else {
            return;
        };
        if !self.motion {
            self.divider_springs = [None; 3];
            let changed = SCENE.with_borrow_mut(|slot| {
                let s = slot.as_mut()?;
                if s.layout.stretch == [0; 3] {
                    return None;
                }
                let old = s.layout;
                s.layout.stretch = [0; 3];
                Some((old, s.layout))
            });
            if let Some((old, layout)) = changed {
                place_controls(self.hwnd, layout, Some(old), false);
            }
            return;
        }
        for (i, &offset) in stretch.iter().enumerate() {
            if offset != 0 {
                let mut sp = Spring::snappy(offset as f32);
                sp.set_target(0.0);
                self.divider_springs[i] = Some(sp);
            } else {
                self.divider_springs[i] = None;
            }
        }
    }
    unsafe fn reset_divider(&mut self, d: Divider) {
        let target = DEFAULT_RATIOS[d as usize];
        if !self.motion {
            self.ratio_springs[d as usize] = None;
            let changed = SCENE.with_borrow_mut(|slot| {
                let s = slot.as_mut()?;
                let old = s.layout;
                s.layout.ratios[d as usize] = target;
                s.layout.stretch[d as usize] = 0;
                Some((old, s.layout))
            });
            if let Some((old, layout)) = changed {
                place_controls(self.hwnd, layout, Some(old), false);
            }
            return;
        }
        let cur = SCENE.with_borrow(|s| s.as_ref().map_or(target, |sc| sc.layout.ratios[d as usize]));
        if (cur - target).abs() > 0.002 {
            let mut sp = Spring::smooth(cur);
            sp.set_target(target);
            self.ratio_springs[d as usize] = Some(sp);
        }
    }
    unsafe fn layout(&mut self, ui: &Console) {
        let notice = !ui.notice.is_empty();
        let mut r = RECT::default();
        GetClientRect(self.hwnd, &mut r);
        if r.right <= 0 || r.bottom <= 0 {
            return;
        }
        let target_top = if notice { 178.0 } else { 144.0 };
        let first = SCENE.with_borrow(|s| s.is_none());
        if first || !self.motion {
            self.top.snap(target_top);
            if !self.motion {
                self.divider_springs = [None; 3];
                self.ratio_springs = [None; 3];
            }
        } else {
            self.top.set_target(target_top);
        }
        let top_px = self.top.value.round() as i32;
        let (top, tail) = log_position(self.child(Ledger));
        let dpi = GetDpiForWindow(self.hwnd);
        let thumb_target = if self.dark { 1.0 } else { 0.0 };
        let (layout, previous, old_fonts) = SCENE.with_borrow_mut(|slot| {
            let previous = slot.as_ref().map(|s| s.layout);
            let ratios = previous.map_or(self.initial_ratios, |l| l.ratios);
            let stretch = if self.motion {
                previous.map_or([0; 3], |l| l.stretch)
            } else {
                [0; 3]
            };
            let layout = Layout::new(r.right, r.bottom, dpi, top_px, ratios, stretch);
            let colors = Colors::new(self.dark);
            let old_fonts = if let Some(s) = slot {
                let changed = s.layout.scale != layout.scale;
                s.layout = layout;
                s.colors = colors;
                s.log_size = self.log_size;
                s.notice = notice;
                s.motion = self.motion;
                s.transition = self.transition;
                let tone_cols = s.phase_tone.colors(colors);
                s.phase_prev_colors = tone_cols;
                s.phase_target_colors = tone_cols;
                s.phase_blend.snap(1.0);
                if self.motion {
                    s.theme_thumb.set_target(thumb_target);
                } else {
                    s.theme_thumb.snap(thumb_target);
                    s.phase_slide.snap(1.0);
                    s.charging.snap(s.charging.target);
                    s.daemon_check.snap(s.daemon_check.target);
                    for h in &mut s.divider_hover {
                        h.snap(h.target);
                    }
                    for rl in s.rollers.values_mut() {
                        rl.snap();
                    }
                    for btn in s.buttons.values_mut() {
                        btn.snap();
                    }
                }
                changed.then(|| std::mem::replace(&mut s.fonts, Fonts::new(layout, self.log_size)))
            } else {
                let tone = PhaseTone::for_phase(&ui.phase, notice);
                let tone_cols = tone.colors(colors);
                let checked = SendMessageW(self.child(Daemon), BM_GETCHECK, 0, 0) == BST_CHECKED as isize;
                *slot = Some(Scene {
                    layout,
                    colors,
                    fonts: Fonts::new(layout, self.log_size),
                    log_size: self.log_size,
                    motion: self.motion,
                    transition: self.transition,
                    frame_time: Instant::now(),
                    landing: None,
                    notice,
                    drag: None,
                    hovered_divider: None,
                    divider_hover: [Spring::hover(0.0); 3],
                    rollers: HashMap::new(),
                    buttons: HashMap::new(),
                    theme_thumb: Spring::bouncy(thumb_target),
                    charging: Spring::smooth(0.0),
                    daemon_check: Spring::snappy(if checked { 1.0 } else { 0.0 }),
                    phase_tone: tone,
                    phase_prev_colors: tone_cols,
                    phase_target_colors: tone_cols,
                    phase_blend: Spring::snappy(1.0),
                    phase_slide: Spring::snappy(1.0),
                    countdown_ratio: None,
                    log_items: std::collections::VecDeque::new(),
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
        if previous.is_none() || old_fonts.is_some() || layout.scale != self.last_scale {
            self.last_scale = layout.scale;
            self.log_metrics(ui);
            restore_log_position(self.child(Ledger), top, tail);
        }
        // Child windows may select the previous fonts until WM_SETFONT returns.
        drop(old_fonts);
    }
    unsafe fn sync_caption_colors(hwnd: HWND, mix: f32) -> bool {
        let light = Colors::new(false);
        let dark = Colors::new(true);
        let caption_bg = painting::lerp_color(light.paper, dark.header, mix);
        let caption_fg = painting::lerp_color(light.ink, dark.ink, mix);
        let bg_res = DwmSetWindowAttribute(hwnd, 35, (&caption_bg as *const u32).cast(), 4);
        let _ = DwmSetWindowAttribute(hwnd, 36, (&caption_fg as *const u32).cast(), 4);
        bg_res == 0
    }
    unsafe fn apply_theme(&self) {
        let theme = if self.dark {
            w!("DarkMode_Explorer")
        } else {
            w!("Explorer")
        };
        SetWindowTheme(self.child(Ledger), theme, null());
        let combo_theme = if self.dark {
            w!("DarkMode_CFD")
        } else {
            w!("CFD")
        };
        SetWindowTheme(self.child(Backend), combo_theme, null());
        let dark = self.dark as i32;
        if DwmSetWindowAttribute(self.hwnd, 20, (&dark as *const i32).cast(), 4) != 0 {
            let _ = DwmSetWindowAttribute(self.hwnd, 19, (&dark as *const i32).cast(), 4);
        }
        let mix = if self.motion {
            SCENE
                .with_borrow(|s| s.as_ref().map(|sc| sc.theme_thumb.value))
                .unwrap_or(dark as f32)
        } else {
            dark as f32
        };
        if !Self::sync_caption_colors(self.hwnd, mix) {
            SetWindowPos(
                self.hwnd,
                null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
    }
    unsafe fn measure_line_width(dc: HDC, line: &str, buf: &mut Vec<u16>) -> i32 {
        buf.clear();
        buf.extend(line.encode_utf16());
        if buf.is_empty() {
            return 0;
        }
        let mut size = SIZE::default();
        GetTextExtentPoint32W(dc, buf.as_ptr(), buf.len() as i32, &mut size);
        size.cx
    }
    unsafe fn log_metrics(&mut self, s: &Console) {
        let list = self.child(Ledger);
        let dc = GetDC(list);
        let font = SendMessageW(list, WM_GETFONT, 0, 0);
        let old = SelectObject(dc, font as HGDIOBJ);
        let mut tm = TEXTMETRICW::default();
        if GetTextMetricsW(dc, &mut tm) != 0 {
            let height = (tm.tmHeight + 1).clamp(1, 255);
            if SendMessageW(list, LB_GETITEMHEIGHT, 0, 0) != height as isize {
                SendMessageW(list, LB_SETITEMHEIGHT, 0, height as isize);
            }
        }
        let mut extent = 0;
        for line in &s.logs {
            extent = extent.max(Self::measure_line_width(dc, line, &mut self.wide_scratch));
        }
        SelectObject(dc, old);
        ReleaseDC(list, dc);
        self.log_extent = extent;
        let pad_x = (22.0 * self.last_scale.max(1.0)).round() as usize;
        SendMessageW(list, LB_SETHORIZONTALEXTENT, extent as usize + pad_x, 0);
    }
    unsafe fn extend_log_metrics(&mut self, s: &Console, delta: usize) {
        if delta == 0 {
            return;
        }
        let list = self.child(Ledger);
        let dc = GetDC(list);
        let font = SendMessageW(list, WM_GETFONT, 0, 0);
        let old = SelectObject(dc, font as HGDIOBJ);
        let mut added_max = 0;
        for line in s.logs.iter().skip(s.logs.len() - delta) {
            added_max = added_max.max(Self::measure_line_width(dc, line, &mut self.wide_scratch));
        }
        SelectObject(dc, old);
        ReleaseDC(list, dc);
        if added_max > self.log_extent {
            self.log_extent = added_max;
            let pad_x = (22.0 * self.last_scale.max(1.0)).round() as usize;
            SendMessageW(list, LB_SETHORIZONTALEXTENT, added_max as usize + pad_x, 0);
        }
    }
    unsafe fn zoom_log(&mut self, delta: i32, ui: &Console) {
        let (top, tail) = log_position(self.child(Ledger));
        self.log_size = (self.log_size + delta).clamp(10, 20);
        let (old, font) = SCENE.with_borrow_mut(|scene| {
            let scene = scene.as_mut().unwrap();
            scene.log_size = self.log_size;
            let font = Fonts::log(scene.layout, self.log_size);
            (std::mem::replace(&mut scene.fonts.log, font), font)
        });
        for id in [Ledger, Empty] {
            self.send(id, WM_SETFONT, font as usize, 1);
        }
        DeleteObject(old);
        self.log_metrics(ui);
        restore_log_position(self.child(Ledger), top, tail);
        let allow_smaller = self.log_size > 10;
        let allow_larger = self.log_size < 20;
        if allow_smaller {
            self.enabled(LogSmaller, true);
        }
        if allow_larger {
            self.enabled(LogLarger, true);
        }
        if !allow_smaller {
            if GetFocus() == self.child(LogSmaller) {
                SetFocus(self.child(LogLarger));
            }
            self.enabled(LogSmaller, false);
        }
        if !allow_larger {
            if GetFocus() == self.child(LogLarger) {
                SetFocus(self.child(LogSmaller));
            }
            self.enabled(LogLarger, false);
        }
    }
    unsafe fn sync(&mut self, s: &Console) -> bool {
        let notice = !s.notice.is_empty();
        if !SCENE.with_borrow(|scene| scene.as_ref().is_some_and(|v| v.notice == notice)) {
            self.layout(s);
        }
        let now = Instant::now();
        let dt = now
            .saturating_duration_since(self.last_frame)
            .as_secs_f32()
            .min(1.0 / 30.0);
        self.last_frame = now;

        let active_drag = SCENE.with_borrow(|v| v.as_ref().and_then(|sc| sc.drag.map(|(d, _)| d)));
        if let Some(d) = active_drag {
            self.divider_springs[d as usize] = None;
            self.ratio_springs[d as usize] = None;
        }
        if !self.top.settled() {
            if self.motion {
                self.top.step(dt);
            } else {
                self.top.snap(self.top.target);
            }
        }
        let mut next_stretch = [0i32; 3];
        for (i, slot) in self.divider_springs.iter_mut().enumerate() {
            if let Some(sp) = slot {
                if !self.motion {
                    *slot = None;
                    next_stretch[i] = 0;
                } else {
                    sp.step(dt);
                    next_stretch[i] = sp.value.round() as i32;
                    if sp.settled() {
                        *slot = None;
                        next_stretch[i] = 0;
                    }
                }
            } else if let Some(d) = active_drag.filter(|&d| d as usize == i) {
                next_stretch[i] =
                    SCENE.with_borrow(|v| v.as_ref().map_or(0, |sc| sc.layout.stretch[d as usize]));
            }
        }
        let mut next_ratios = SCENE.with_borrow(|v| v.as_ref().map_or(self.initial_ratios, |sc| sc.layout.ratios));
        let mut ratio_changed = false;
        for (i, slot) in self.ratio_springs.iter_mut().enumerate() {
            if let Some(sp) = slot {
                if !self.motion {
                    *slot = None;
                } else {
                    sp.step(dt);
                    next_ratios[i] = sp.value;
                    ratio_changed = true;
                    if sp.settled() {
                        next_ratios[i] = sp.target;
                        *slot = None;
                    }
                }
            }
        }
        let next_top = self.top.value.round() as i32;
        let mut dirty_stretch = [false; 3];
        let mut top_changed = false;
        let mut stretch_changed = false;
        SCENE.with_borrow_mut(|v| {
            if let Some(sc) = v {
                top_changed = sc.layout.top != next_top;
                if sc.layout.stretch != next_stretch {
                    for (i, slot) in dirty_stretch.iter_mut().enumerate() {
                        *slot = sc.layout.stretch[i] != next_stretch[i];
                    }
                    stretch_changed = true;
                    if !top_changed && !ratio_changed {
                        sc.layout.stretch = next_stretch;
                    }
                }
            }
        });
        if top_changed || ratio_changed {
            let mut r = RECT::default();
            GetClientRect(self.hwnd, &mut r);
            if r.right > 0 && r.bottom > 0 {
                let dpi = GetDpiForWindow(self.hwnd);
                let mut layout_pair = None;
                let mut old_fonts = None;
                SCENE.with_borrow_mut(|v| {
                    if let Some(sc) = v {
                        let old = sc.layout;
                        let next = Layout::new(
                            r.right,
                            r.bottom,
                            dpi,
                            next_top,
                            next_ratios,
                            next_stretch,
                        );
                        if next.scale != old.scale {
                            old_fonts = Some(std::mem::replace(
                                &mut sc.fonts,
                                Fonts::new(next, self.log_size),
                            ));
                        }
                        sc.layout = next;
                        layout_pair = Some((old, next));
                    }
                });
                if let Some((old, next)) = layout_pair {
                    place_controls(self.hwnd, next, Some(old), old_fonts.is_some());
                }
                drop(old_fonts);
            }
        } else if stretch_changed {
            for (d, changed) in [Divider::Columns, Divider::LeftRow, Divider::RightRow]
                .into_iter()
                .zip(dirty_stretch)
            {
                if changed {
                    let rect = SCENE.with_borrow(|v| {
                        v.as_ref().map(|sc| sc.layout.pixels(sc.layout.band(d)))
                    });
                    if let Some(rect) = rect {
                        InvalidateRect(self.hwnd, &rect, 0);
                    }
                }
            }
        }

        let daemon_checked =
            SendMessageW(self.child(Daemon), BM_GETCHECK, 0, 0) == BST_CHECKED as isize;
        let next_cd_ratio = if matches!(s.phase.as_str(), "countdown" | "resuming") {
            s.status["countdown_s"].as_f64().map(|v| {
                let v = v as f32;
                let max = self.countdown_max.get_or_insert(v.max(0.5));
                if v > *max {
                    *max = v;
                }
                (v / *max).clamp(0.0, 1.0)
            })
        } else {
            self.countdown_max = None;
            None
        };
        let charging_target = if s.is_charging() { 1.0 } else { 0.0 };
        let target_tone = PhaseTone::for_phase(&s.phase, notice);

        let mut thumb_dirty = false;
        let mut thumb_moving = false;
        let mut caption_mix = None;
        let mut buttons_moving = false;
        let mut dividers_moving = false;
        let mut daemon_dirty = false;
        let mut daemon_moving = false;
        let mut phase_dirty = false;
        let mut phase_moving = false;
        let mut charging_moving = false;
        let mut dirty_dividers = [None; 3];
        let mut dirty_rollers = Vec::new();
        let mut dirty_buttons = Vec::new();
        let mut dirty_entry_bars = None;
        let mut dirty_diagram = None;
        let scene_moving = SCENE.with_borrow_mut(|v| {
            let scene = v.as_mut().unwrap();
            let previous = scene.frame_time;
            scene.frame_time = now;
            let changed = scene.landing != s.landed;
            scene.landing = s.landed;
            let entry_end = scene.transition + painting::ENTRY_DURATION;
            let end = scene
                .landing
                .map_or(entry_end, |j| entry_end.max(j.started + j.duration()));

            if (scene.charging.target - charging_target).abs() > 0.01 {
                if scene.motion {
                    scene.charging.set_target(charging_target);
                } else {
                    scene.charging.snap(charging_target);
                }
            }
            let charge_active = !scene.charging.settled() || (scene.motion && s.is_charging());
            if !scene.charging.settled() {
                if scene.motion {
                    scene.charging.step(dt);
                } else {
                    scene.charging.snap(charging_target);
                }
            }
            charging_moving = scene.motion && (!scene.charging.settled() || s.is_charging());

            // Include the first tick past the deadline so the resting frame is painted.
            if scene.motion && previous < entry_end {
                dirty_entry_bars = Some(scene.layout.entry_bar_pixels());
                dirty_diagram = Some(scene.layout.diagram_pixels());
            } else if changed || charge_active || (scene.motion && previous < end) {
                dirty_diagram = Some(scene.layout.diagram_pixels());
            }

            let d_target = if daemon_checked { 1.0 } else { 0.0 };
            if (scene.daemon_check.target - d_target).abs() > 0.01 {
                if scene.motion {
                    scene.daemon_check.set_target(d_target);
                } else {
                    scene.daemon_check.snap(d_target);
                    daemon_dirty = true;
                }
            }
            if !scene.daemon_check.settled() {
                if scene.motion {
                    scene.daemon_check.step(dt);
                } else {
                    scene.daemon_check.snap(d_target);
                }
                daemon_dirty = true;
                daemon_moving = !scene.daemon_check.settled();
            }

            if scene.phase_tone != target_tone {
                scene.phase_prev_colors = scene.current_phase_colors();
                scene.phase_tone = target_tone;
                scene.phase_target_colors = target_tone.colors(scene.colors);
                if scene.motion {
                    scene.phase_blend.snap(0.0);
                    scene.phase_blend.set_target(1.0);
                } else {
                    scene.phase_blend.snap(1.0);
                }
                phase_dirty = true;
            }
            if !scene.phase_blend.settled() {
                if scene.motion {
                    scene.phase_blend.step(dt);
                } else {
                    scene.phase_blend.snap(1.0);
                }
                phase_dirty = true;
            }
            if !scene.phase_slide.settled() {
                if scene.motion {
                    scene.phase_slide.step(dt);
                } else {
                    scene.phase_slide.snap(1.0);
                }
                phase_dirty = true;
            }
            if scene.countdown_ratio != next_cd_ratio {
                scene.countdown_ratio = next_cd_ratio;
                phase_dirty = true;
            }
            phase_moving = !scene.phase_blend.settled() || !scene.phase_slide.settled();

            for (i, d) in [Divider::Columns, Divider::LeftRow, Divider::RightRow]
                .into_iter()
                .enumerate()
            {
                if !scene.divider_hover[i].settled() {
                    if scene.motion {
                        scene.divider_hover[i].step(dt);
                    } else {
                        scene.divider_hover[i].snap(scene.divider_hover[i].target);
                    }
                    dirty_dividers[i] = Some(scene.layout.pixels(scene.layout.band(d)));
                    if !scene.divider_hover[i].settled() {
                        dividers_moving = true;
                    }
                }
            }

            if !scene.theme_thumb.settled() {
                if scene.motion {
                    scene.theme_thumb.step(dt);
                } else {
                    scene.theme_thumb.snap(scene.theme_thumb.target);
                }
                thumb_dirty = true;
                thumb_moving = !scene.theme_thumb.settled();
                caption_mix = Some(scene.theme_thumb.value.clamp(0.0, 1.0));
            }
            for (&id, btn) in &mut scene.buttons {
                if btn.active() {
                    if scene.motion {
                        btn.step(dt);
                    } else {
                        btn.snap();
                    }
                    dirty_buttons.push(id);
                    if btn.active() {
                        buttons_moving = true;
                    }
                }
            }
            for (&id, roller) in &mut scene.rollers {
                if roller.active() {
                    if scene.motion {
                        roller.step(dt);
                    } else {
                        roller.snap();
                    }
                    dirty_rollers.push(id);
                }
            }
            scene.motion && scene.frame_time < end
        });
        for rect in dirty_dividers.into_iter().flatten() {
            InvalidateRect(self.hwnd, &rect, 0);
        }
        if let Some(bars) = dirty_entry_bars {
            for bar in bars {
                InvalidateRect(self.hwnd, &bar, 0);
            }
        }
        if let Some(board) = dirty_diagram {
            InvalidateRect(self.hwnd, &board, 0);
        }
        if thumb_dirty {
            InvalidateRect(self.child(Theme), null(), 0);
        }
        if let Some(mix) = caption_mix {
            Self::sync_caption_colors(self.hwnd, mix);
        }
        if daemon_dirty {
            InvalidateRect(self.child(Daemon), null(), 0);
        }
        if phase_dirty {
            InvalidateRect(self.child(Phase), null(), 0);
        }
        for id in dirty_buttons {
            if id != Theme && id != Daemon {
                InvalidateRect(self.child(id), null(), 0);
            } else if id == Daemon && !daemon_dirty {
                InvalidateRect(self.child(Daemon), null(), 0);
            }
        }
        for id in dirty_rollers {
            InvalidateRect(self.child(id), null(), 0);
        }

        let mut end_fade = false;
        if let Some(fade) = &mut self.fade {
            let alive = FADE_SURFACE.with_borrow(Option::is_some);
            let elapsed = fade.start.elapsed().as_secs_f32();
            let p = elapsed / 0.35;
            if !alive || !self.motion || p >= 1.0 {
                end_fade = true;
            } else {
                let r = (spring_curve(elapsed, 0.30, 1.0) * fade.max_r).round() as i32;
                if r != fade.last_r {
                    fade.last_r = r;
                    let outer = CreateRectRgn(0, 0, fade.w, fade.h);
                    if !outer.is_null() {
                        if r > 0 {
                            let hole = CreateEllipticRgn(
                                fade.center.x - r,
                                fade.center.y - r,
                                fade.center.x + r,
                                fade.center.y + r,
                            );
                            if !hole.is_null() {
                                CombineRgn(outer, outer, hole, RGN_DIFF);
                                DeleteObject(hole);
                            }
                        }
                        if SetWindowRgn(fade.overlay, outer, 0) == 0 {
                            DeleteObject(outer);
                        }
                    }
                }
                let t = ((p - 0.42) / (1.0 - 0.42)).clamp(0.0, 1.0);
                let smooth = t * t * (3.0 - 2.0 * t);
                let alpha = (255.0 * (1.0 - smooth)).round().clamp(0.0, 255.0) as u8;
                if alpha != fade.last_alpha {
                    fade.last_alpha = alpha;
                    SetLayeredWindowAttributes(fade.overlay, 0, alpha, LWA_ALPHA);
                }
            }
        }
        if end_fade {
            self.end_fade();
        }

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
        self.enabled(LogCopy, !s.logs.is_empty());
        let copy_label = if self.copied_until.is_some_and(|until| now < until) {
            "已复制"
        } else {
            self.copied_until = None;
            "复制"
        };
        self.text(LogCopy, copy_label);
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
            self.log_extent = 0;
            self.send(Ledger, LB_SETHORIZONTALEXTENT, 5, 0);
            SCENE.with_borrow_mut(|v| {
                if let Some(sc) = v {
                    sc.log_items.clear();
                }
            });
        }
        if self.log_sequence < s.log_sequence {
            let (top, tail) = log_position(self.child(Ledger));
            let delta = (s.log_sequence - self.log_sequence).min(s.logs.len() as u64) as usize;
            let mut removed = 0;
            let start_seq = s.log_sequence.saturating_sub(delta as u64);
            for (idx, line) in s.logs.iter().skip(s.logs.len() - delta).enumerate() {
                if self.send(Ledger, LB_GETCOUNT, 0, 0) >= 60 {
                    self.send(Ledger, LB_DELETESTRING, 0, 0);
                    removed += 1;
                }
                self.wide_scratch.clear();
                self.wide_scratch.extend(line.encode_utf16());
                self.wide_scratch.push(0);
                self.send(
                    Ledger,
                    LB_ADDSTRING,
                    0,
                    self.wide_scratch.as_ptr() as isize,
                );
                let seq = start_seq + idx as u64 + 1;
                SCENE.with_borrow_mut(|v| {
                    if let Some(sc) = v {
                        if sc.log_items.len() == 60 {
                            sc.log_items.pop_front();
                        }
                        sc.log_items.push_back((seq, now, line.clone()));
                    }
                });
            }
            if removed > 0 {
                self.log_metrics(s);
            } else {
                self.extend_log_metrics(s, delta);
            }
            restore_log_position(self.child(Ledger), (top - removed).max(0), tail);
            self.log_sequence = s.log_sequence;
        }
        self.visible(Ledger, !s.logs.is_empty());
        self.visible(Empty, s.logs.is_empty());
        let logs_fading = SCENE.with_borrow(|sc| {
            sc.as_ref().is_some_and(|sc| {
                sc.motion
                    && !s.logs.is_empty()
                    && sc.log_items.iter().any(|(_, added_at, _)| {
                        now.saturating_duration_since(*added_at) < Duration::from_millis(220)
                    })
            })
        });
        if logs_fading {
            InvalidateRect(self.child(Ledger), null(), 0);
        }

        let rollers_active = SCENE.with_borrow(|sc| {
            sc.as_ref()
                .is_some_and(|sc| sc.rollers.values().any(Roller::active))
        });
        let buttons_active = buttons_moving
            || SCENE.with_borrow(|sc| {
                sc.as_ref()
                    .is_some_and(|sc| sc.buttons.values().any(|b| b.active()))
            });
        let animate = scene_moving
            || charging_moving
            || daemon_moving
            || phase_moving
            || dividers_moving
            || logs_fading
            || rollers_active
            || thumb_moving
            || buttons_active
            || !self.top.settled()
            || self.divider_springs.iter().any(Option::is_some)
            || self.ratio_springs.iter().any(Option::is_some)
            || self.fade.is_some();
        let interval = if animate {
            0
        } else if s.mode != Mode::Idle {
            80
        } else if self.copied_until.is_some() {
            120
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
        animate
    }
}
impl Drop for View {
    fn drop(&mut self) {
        unsafe {
            self.end_fade();
            if !self.fade_overlay.is_null() {
                DestroyWindow(self.fade_overlay);
                self.fade_overlay = null_mut();
            }
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
            painting::release_buffers();
            CONTROL_PROCS.with_borrow_mut(HashMap::clear);
            HOVERED_CONTROLS.with_borrow_mut(HashMap::clear);
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
        let prefs = UiPrefs::load();
        let instance = GetModuleHandleW(null());
        let class = w!("JumpBotConsole");
        // Icon group 1 also supplies the executable icon used by the Windows shell.
        let icon = LoadIconW(instance, std::ptr::without_provenance(1));
        if icon.is_null() {
            return Err(io::Error::last_os_error().into());
        }
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
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
        let fade_wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(fade_wndproc),
            hInstance: instance,
            lpszClassName: w!("JumpBotFade"),
            ..Default::default()
        };
        if RegisterClassExW(&fade_wc) == 0 {
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
            (prefs.width * dpi / 96).min(GetSystemMetrics(SM_CXSCREEN)),
            (prefs.height * dpi / 96).min(GetSystemMetrics(SM_CYSCREEN) - 50),
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            return Err(io::Error::last_os_error().into());
        }
        let mut v = View::new(hwnd, &prefs)?;
        v.apply_theme();
        let mut animating = v.sync(&host.ui);
        // Keep the window cloaked (DWMWA_CLOAK = 13) until the first complete frame is
        // painted, so it never flashes an unpainted (white) client area on open.
        let cloak = 1i32;
        let cloaked =
            DwmSetWindowAttribute(hwnd, 13, (&cloak as *const i32).cast(), 4) == 0;
        ShowWindow(hwnd, SW_SHOWNORMAL);
        RedrawWindow(
            hwnd,
            null(),
            null_mut(),
            RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN | RDW_UPDATENOW,
        );
        if cloaked {
            let uncloak = 0i32;
            DwmSetWindowAttribute(hwnd, 13, (&uncloak as *const i32).cast(), 4);
        }
        let mut last_poll = Instant::now();
        let mut msg = MSG::default();
        let handle_msg = |msg: &MSG,
                          v: &mut View,
                          host: &mut Host,
                          last_poll: &mut Instant|
         -> bool {
            if msg.message == WM_KEYDOWN && msg.wParam == VK_ESCAPE as usize && GetCapture() == hwnd
            {
                end_drag(hwnd);
                return false;
            }
            match (msg.hwnd == hwnd, msg.message) {
                (true, ACTION) if !host.closing => {
                    let Some(id) = Id::from_raw(msg.wParam) else {
                        return false;
                    };
                    if v.child(id) != msg.lParam as HWND || IsWindowEnabled(v.child(id)) == 0 {
                        return false;
                    }
                    match id {
                        Primary => {
                            let index = v.send(Backend, CB_GETCURSEL, 0, 0) as usize;
                            if index < BACKENDS.len() {
                                host.primary(
                                    index,
                                    v.send(Daemon, BM_GETCHECK, 0, 0) == BST_CHECKED as isize,
                                );
                                *last_poll = Instant::now();
                            }
                        }
                        LogCopy => {
                            let text = host
                                .ui
                                .logs
                                .iter()
                                .cloned()
                                .collect::<Vec<_>>()
                                .join("\r\n");
                            if copy_to_clipboard(hwnd, &text) {
                                v.copied_until =
                                    Some(Instant::now() + Duration::from_millis(1200));
                            }
                        }
                        LogSmaller => v.zoom_log(-1, &host.ui),
                        LogLarger => v.zoom_log(1, &host.ui),
                        Stop => host.stop(),
                        Check => {
                            host.start(2, false, true);
                            *last_poll = Instant::now();
                        }
                        Theme => {
                            v.start_fade();
                            v.dark = !v.dark;
                            v.apply_theme();
                            v.manual = true;
                            v.transition = Instant::now();
                            v.layout(&host.ui);
                            RedrawWindow(
                                hwnd,
                                null(),
                                null_mut(),
                                RDW_INVALIDATE
                                    | RDW_ERASE
                                    | RDW_FRAME
                                    | RDW_UPDATENOW
                                    | RDW_ALLCHILDREN,
                            );
                        }
                        _ => {}
                    }
                    true
                }
                (true, CLOSE) => {
                    v.save_prefs();
                    v.end_fade();
                    host.closing = true;
                    host.stop();
                    true
                }
                (true, RESIZE) => {
                    v.end_fade();
                    v.layout(&host.ui);
                    true
                }
                (true, MOVED) => {
                    v.end_fade();
                    true
                }
                (true, SETTLE) => {
                    v.settle();
                    true
                }
                (true, RESET_DIVIDER) => {
                    let d = match msg.wParam {
                        0 => Divider::Columns,
                        1 => Divider::LeftRow,
                        _ => Divider::RightRow,
                    };
                    v.reset_divider(d);
                    true
                }
                (true, WAKE) => true,
                (true, SETTINGS) => {
                    if !v.manual {
                        let dark = system_dark();
                        if dark != v.dark {
                            v.end_fade();
                            v.dark = dark;
                            v.apply_theme();
                        }
                    }
                    v.motion = system_motion();
                    if !v.motion {
                        v.end_fade();
                        View::sync_caption_colors(hwnd, if v.dark { 1.0 } else { 0.0 });
                    }
                    v.layout(&host.ui);
                    RedrawWindow(
                        hwnd,
                        null(),
                        null_mut(),
                        RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN | RDW_UPDATENOW,
                    );
                    true
                }
                (true, WM_TIMER) => {
                    if last_poll.elapsed() >= Duration::from_millis(80) {
                        host.poll();
                        *last_poll = Instant::now();
                    }
                    true
                }
                _ => {
                    if IsDialogMessageW(hwnd, msg) == 0 {
                        TranslateMessage(msg);
                        DispatchMessageW(msg);
                    }
                    false
                }
            }
        };
        let frame = frame_interval(hwnd);
        let mut hires = HiResTimer(false);
        let mut next_frame = Instant::now() + frame;
        loop {
            hires.set(animating);
            if animating {
                let mut quit = false;
                while PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
                    if msg.message == WM_QUIT {
                        quit = true;
                        break;
                    }
                    handle_msg(&msg, &mut v, &mut host, &mut last_poll);
                }
                if quit || (host.closing && host.session.is_none()) {
                    break;
                }
                // Wait for the next frame while waking instantly for input, sent
                // messages and cross-apartment COM calls (UI Automation).
                let now = Instant::now();
                if now < next_frame {
                    let rem = next_frame - now;
                    if rem >= Duration::from_millis(1) {
                        MsgWaitForMultipleObjectsEx(
                            0,
                            null(),
                            rem.as_millis() as u32,
                            QS_ALLINPUT,
                            MWMO_INPUTAVAILABLE,
                        );
                        if Instant::now() + Duration::from_millis(1) < next_frame {
                            continue;
                        }
                    }
                }
                let step_now = Instant::now();
                next_frame = (next_frame + frame).max(step_now + frame / 2);
                if last_poll.elapsed() >= Duration::from_millis(80) {
                    host.poll();
                    last_poll = Instant::now();
                }
                if host.closing && host.session.is_none() {
                    break;
                }
                animating = v.sync(&host.ui);
                // Present the parent and every invalidated child in the same frame.
                RedrawWindow(hwnd, null(), null_mut(), RDW_UPDATENOW | RDW_ALLCHILDREN);
            } else {
                let got = GetMessageW(&mut msg, null_mut(), 0, 0);
                if got < 0 {
                    return Err(io::Error::last_os_error().into());
                }
                if got == 0 {
                    break;
                }
                let need_sync = handle_msg(&msg, &mut v, &mut host, &mut last_poll);
                if host.closing && host.session.is_none() {
                    break;
                }
                if need_sync {
                    animating = v.sync(&host.ui);
                    if animating {
                        next_frame = Instant::now() + frame;
                    }
                }
            }
        }
        v.save_prefs();
        Ok(())
    }
}
pub fn startup_error(s: &str) {
    unsafe {
        MessageBoxW(null_mut(), wide(s).as_ptr(), w!("JumpBot"), MB_ICONERROR);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_prefs_sanitizes_corrupt_values() {
        let dirty = UiPrefs {
            dark: Some(true),
            manual_theme: true,
            ratios: [f32::NAN, 0.01, 0.99],
            log_size: 42,
            width: 300,
            height: 9999,
        }
        .sanitized();
        assert_eq!(dirty.log_size, 20);
        assert_eq!(dirty.width, 700);
        assert_eq!(dirty.height, 2160);
        assert!(dirty.ratios[0].is_finite() && dirty.ratios[0] >= 0.15);
        assert_eq!(dirty.ratios[1], 0.15);
        assert_eq!(dirty.ratios[2], 0.85);
    }
}
