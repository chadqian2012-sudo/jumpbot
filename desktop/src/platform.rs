//! Windows live I/O. Replay can construct crate::Frame directly. Keep the
//! single-instance handle and the hotkey listener alive for the whole live run.
use crate::{Config, Frame, Result};
use regex::Regex;
use std::{
    ffi::c_void,
    io,
    marker::PhantomData,
    mem,
    os::windows::io::{FromRawHandle, OwnedHandle},
    ptr::{null, null_mut},
    rc::Rc,
    sync::Mutex,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, POINT, RECT},
    Graphics::{
        Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS},
        Gdi::*,
    },
    Storage::Xps::{PrintWindow, PW_CLIENTONLY},
    System::Threading::{
        CreateMutexW, OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    },
    UI::{HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

fn os_error(operation: &str) -> String {
    format!("{operation}: {}", io::Error::last_os_error())
}

pub fn no_input() -> bool {
    std::env::var_os("JUMPBOT_NO_INPUT").is_some_and(|value| value == "1")
}

#[must_use = "keep this handle until the live worker has finished releasing input"]
pub struct SingleInstance {
    _handle: OwnedHandle,
}

impl SingleInstance {
    pub fn acquire() -> Result<Self> {
        Self::named(r"Local\JumpBot.Desktop.LiveInput")
    }

    fn named(name: &str) -> Result<Self> {
        let wide: Vec<_> = name.encode_utf16().chain(Some(0)).collect();
        // Lifetime of the named object is the lock. No thread owns the mutex.
        let raw = unsafe { CreateMutexW(null(), 0, wide.as_ptr()) };
        if raw.is_null() {
            return Err(os_error("CreateMutexW"));
        }
        let exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        if exists {
            return Err("另一个 JumpBot 实例已取得输入控制权".into());
        }
        Ok(Self { _handle: handle })
    }
}

struct PhysicalPixels(DPI_AWARENESS_CONTEXT);

impl PhysicalPixels {
    fn enter() -> Result<Self> {
        let old =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if old.is_null() {
            Err(os_error("SetThreadDpiAwarenessContext"))
        } else {
            Ok(Self(old))
        }
    }
}

impl Drop for PhysicalPixels {
    fn drop(&mut self) {
        unsafe { SetThreadDpiAwarenessContext(self.0) };
    }
}

#[derive(Clone, Debug)]
pub struct Window {
    pub hwnd: isize,
    pub pid: u32,
    pub title: String,
    pub exe: String,
    /// Physical screen coordinates, right and bottom exclusive.
    pub client_rect: [i32; 4],
}

fn title(hwnd: HWND) -> String {
    unsafe {
        let length = GetWindowTextLengthW(hwnd);
        if length <= 0 {
            return String::new();
        }
        let mut buf = vec![0; length as usize + 1];
        let used = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        String::from_utf16_lossy(&buf[..used as usize])
    }
}

fn pid(hwnd: HWND) -> u32 {
    let mut id = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut id) };
    id
}

fn compiled_regex(pattern: &str) -> Result<Regex> {
    static PATTERN_CACHE: Mutex<Option<(String, Regex)>> = Mutex::new(None);
    let mut guard = PATTERN_CACHE.lock().map_err(|e| e.to_string())?;
    if let Some((cached, re)) = guard.as_ref() {
        if cached == pattern {
            return Ok(re.clone());
        }
    }
    let re = Regex::new(pattern).map_err(|e| format!("窗口标题正则无效: {e}"))?;
    *guard = Some((pattern.to_owned(), re.clone()));
    Ok(re)
}

fn executable(id: u32) -> String {
    unsafe {
        let raw = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, id);
        if raw.is_null() {
            return String::new();
        }
        let _handle = OwnedHandle::from_raw_handle(raw);
        let mut stack = [0u16; 512];
        let mut len = stack.len() as u32;
        if QueryFullProcessImageNameW(raw, 0, stack.as_mut_ptr(), &mut len) != 0 {
            return String::from_utf16_lossy(&stack[..len as usize]);
        }
        const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
        if GetLastError() != ERROR_INSUFFICIENT_BUFFER {
            return String::new();
        }
        let mut buf = vec![0; 32768];
        len = buf.len() as u32;
        if QueryFullProcessImageNameW(raw, 0, buf.as_mut_ptr(), &mut len) == 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..len as usize])
    }
}

fn client_rect(hwnd: HWND) -> Result<Option<[i32; 4]>> {
    let mut rect = RECT::default();
    let mut origin = POINT::default();
    unsafe {
        if GetClientRect(hwnd, &mut rect) == 0 || ClientToScreen(hwnd, &mut origin) == 0 {
            return Err(os_error("读取客户区位置"));
        }
    }
    if rect.right <= 0 || rect.bottom <= 0 {
        return Ok(None);
    }
    Ok(Some([
        origin.x,
        origin.y,
        origin
            .x
            .checked_add(rect.right)
            .ok_or("窗口横坐标超出范围")?,
        origin
            .y
            .checked_add(rect.bottom)
            .ok_or("窗口纵坐标超出范围")?,
    ]))
}

fn point_is_target(hwnd: HWND, point: POINT) -> bool {
    unsafe { GetAncestor(WindowFromPoint(point), GA_ROOT) == hwnd }
}

struct Search<'a> {
    pattern: &'a Regex,
    exe_hint: String,
    min_size: (usize, usize),
    largest: Option<Window>,
}

unsafe extern "system" fn candidate(hwnd: HWND, data: LPARAM) -> i32 {
    let search = &mut *(data as *mut Search<'_>);
    if IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 {
        return 1;
    }
    let name = title(hwnd);
    if name.is_empty() || !search.pattern.is_match(&name) {
        return 1;
    }
    let Ok(Some(rect)) = client_rect(hwnd) else {
        return 1;
    };
    let (width, height) = ((rect[2] - rect[0]) as usize, (rect[3] - rect[1]) as usize);
    let area = width as u64 * height as u64;
    if width < search.min_size.0
        || height < search.min_size.1
        || search
            .largest
            .as_ref()
            .is_some_and(|old| old.width() as u64 * old.height() as u64 >= area)
    {
        return 1;
    }
    let id = pid(hwnd);
    let exe = executable(id);
    if id != 0 && exe.to_lowercase().contains(&search.exe_hint) {
        search.largest = Some(Window {
            hwnd: hwnd as isize,
            pid: id,
            title: name,
            exe,
            client_rect: rect,
        });
    }
    1
}

impl Window {
    /// Match title regex, case-insensitive executable path substring and minimum
    /// client size. None means "wait for window"; malformed regex is an error.
    pub fn find(cfg: &Config, min_size: (usize, usize)) -> Result<Option<Self>> {
        let _dpi = PhysicalPixels::enter()?;
        let pattern = compiled_regex(cfg.s("window_title_pattern"))?;
        let mut search = Search {
            pattern: &pattern,
            exe_hint: cfg.s("window_exe_hint").to_lowercase(),
            min_size,
            largest: None,
        };
        if unsafe { EnumWindows(Some(candidate), &mut search as *mut _ as LPARAM) } == 0 {
            return Err(os_error("EnumWindows"));
        }
        Ok(search.largest)
    }

    pub fn width(&self) -> usize {
        (self.client_rect[2] - self.client_rect[0]) as usize
    }
    pub fn height(&self) -> usize {
        (self.client_rect[3] - self.client_rect[1]) as usize
    }

    fn handle(&self) -> HWND {
        self.hwnd as HWND
    }

    pub(crate) fn same_instance_exists(&self) -> bool {
        pid(self.handle()) == self.pid
    }

    pub(crate) fn still_current(&self) -> Result<bool> {
        let _dpi = PhysicalPixels::enter()?;
        let hwnd = self.handle();
        if unsafe { IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 }
            || !self.same_instance_exists()
            || title(hwnd) != self.title
        {
            return Ok(false);
        }
        Ok(client_rect(hwnd)? == Some(self.client_rect))
    }

    pub fn is_foreground(&self) -> bool {
        unsafe { GetForegroundWindow() == self.handle() && self.same_instance_exists() }
    }

    /// One foreground request. The owner handles countdown, retries and pause.
    pub fn activate(&self) -> Result<()> {
        let _dpi = PhysicalPixels::enter()?;
        if !self.still_current()? {
            return Err("目标窗口已改变".into());
        }
        if !self.is_foreground() && !no_input() {
            unsafe { SetForegroundWindow(self.handle()) };
        }
        if self.is_foreground() {
            Ok(())
        } else {
            Err("请将游戏窗口切到前台".into())
        }
    }

    fn check_screen(&self) -> Result<()> {
        if !self.is_foreground() {
            return Err("游戏窗口不在前台".into());
        }
        let [left, top, right, bottom] = self.client_rect;
        let mut y = top;
        while y < bottom {
            let mut x = left;
            let mut next_y = bottom;
            while x < right {
                let point = POINT { x, y };
                let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) };
                let mut info = MONITORINFO {
                    cbSize: mem::size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
                    return Err(os_error("GetMonitorInfoW"));
                }
                if unsafe { PtInRect(&info.rcMonitor, point) } == 0 {
                    return Err("游戏窗口部分位于屏幕外".into());
                }
                x = info.rcMonitor.right;
                // The whole strip is covered down to the first monitor's bottom edge.
                next_y = next_y.min(info.rcMonitor.bottom);
            }
            y = next_y;
        }
        Ok(())
    }

    /// A window rectangle is only a hint for diagnosing a failed visible read.
    /// Transparent pixels in layered windows can occupy this entire rectangle.
    pub(crate) fn overlap(&self, bounds: [i32; 4]) -> Option<Overlay> {
        let _dpi = PhysicalPixels::enter().ok()?;
        let mut cover = Cover {
            target: self.handle(),
            bounds,
            found: None,
        };
        unsafe { EnumWindows(Some(check_cover), &mut cover as *mut _ as LPARAM) };
        cover.found
    }

    pub(crate) fn covering_pid(&self, bounds: [i32; 4]) -> Option<u32> {
        match self.overlap(bounds)? {
            Overlay::Other(id) => Some(id),
            Overlay::Allowed => None,
        }
    }

    /// Screen reads visible desktop pixels. PrintWindow requests client pixels
    /// from the target itself and can be used when the desktop is occluded.
    pub fn capture(&self, mode: CaptureMode) -> Result<Option<Frame>> {
        self.capture_with(mode, |w, h, clear, draw| {
            ReusableSurface::new(w, h)?.capture(clear, draw)
        })
    }

    fn capture_with<F>(&self, mode: CaptureMode, mut render: F) -> Result<Option<Frame>>
    where
        F: FnMut(i32, i32, bool, &mut dyn FnMut(HDC) -> Result<()>) -> Result<Frame>,
    {
        let _dpi = PhysicalPixels::enter()?;
        if !self.still_current()? {
            return Ok(None);
        }
        if mode == CaptureMode::Screen {
            self.check_screen()?;
        }
        let width = self.width() as i32;
        let height = self.height() as i32;
        let mut capture = |clear, draw: &mut dyn FnMut(HDC) -> Result<()>| {
            let frame = render(width, height, clear, draw)?;
            if frame.is_solid_color()
                && (mode != CaptureMode::Screen || self.overlap(self.client_rect).is_none())
            {
                return Err("截图只有单一颜色，等待窗口显示内容".into());
            }
            Ok(frame)
        };
        let frame = match mode {
            CaptureMode::Screen => capture(false, &mut |dc| unsafe {
                let screen = GetDC(null_mut());
                if screen.is_null() {
                    return Err(os_error("GetDC"));
                }
                let ok = BitBlt(
                    dc,
                    0,
                    0,
                    width,
                    height,
                    screen,
                    self.client_rect[0],
                    self.client_rect[1],
                    SRCCOPY | CAPTUREBLT,
                );
                let result = if ok != 0 {
                    Ok(())
                } else {
                    Err(os_error("BitBlt"))
                };
                ReleaseDC(null_mut(), screen);
                result
            }),
            CaptureMode::PrintWindow => {
                const PW_RENDERFULLCONTENT: u32 = 2;
                let mut print = |flags| {
                    capture(true, &mut |dc| {
                        if unsafe { PrintWindow(self.handle(), dc, flags) } != 0 {
                            Ok(())
                        } else {
                            Err(os_error("PrintWindow"))
                        }
                    })
                };
                print(PW_CLIENTONLY | PW_RENDERFULLCONTENT).or_else(|_| print(PW_CLIENTONLY))
            }
        };
        if !self.still_current()? {
            return Ok(None);
        }
        if mode == CaptureMode::Screen {
            self.check_screen()?;
        }
        frame.map(Some)
    }

    fn press_target(&self, point: (f64, f64), background: bool) -> Result<Option<ButtonTarget>> {
        let cx = (point.0 * self.width() as f64) as i32;
        let cy = (point.1 * self.height() as f64) as i32;
        let target = if background {
            if cx > i16::MAX as i32 || cy > i16::MAX as i32 {
                return Err("后台按压点超出 Windows 消息范围".into());
            }
            ButtonTarget::Client(self.handle(), ((cy << 16) | cx) as LPARAM)
        } else {
            let x = self.client_rect[0] + cx;
            let y = self.client_rect[1] + cy;
            let absolute = |p: i32, origin, extent| {
                let start = unsafe { GetSystemMetrics(origin) };
                let size = i64::from(unsafe { GetSystemMetrics(extent) });
                let offset = i64::from(p) - i64::from(start);
                if !(0..size).contains(&offset) {
                    return Err("按压点位于屏幕外");
                }
                Ok(((2 * offset + 1) * 32768 / size) as i32)
            };
            ButtonTarget::Desktop(
                absolute(x, SM_XVIRTUALSCREEN, SM_CXVIRTUALSCREEN)?,
                absolute(y, SM_YVIRTUALSCREEN, SM_CYVIRTUALSCREEN)?,
            )
        };
        let visible = background
            || (self.is_foreground()
                && point_is_target(
                    self.handle(),
                    POINT {
                        x: self.client_rect[0] + cx,
                        y: self.client_rect[1] + cy,
                    },
                ));
        if !visible || !self.still_current()? {
            return Ok(None);
        }
        Ok(Some(target))
    }

    /// The admission callback returns the parent's locked control guard after
    /// checking pause, stop and the generation of the recognized frame. DOWN
    /// executes under that guard. Hold and UP happen after unlocking. The
    /// parent acknowledges pause only once this operation has returned.
    pub fn press<G>(
        &self,
        point: (f64, f64),
        duration: Duration,
        background: bool,
        admit: impl FnOnce() -> Result<G>,
    ) -> Result<PressOutcome> {
        if no_input() {
            return Ok(PressOutcome::Suppressed);
        }
        if ![point.0, point.1].iter().all(|v| (0.0..1.0).contains(v)) {
            return Err("按压点必须位于客户区内，坐标范围为 [0,1)".into());
        }
        let _dpi = PhysicalPixels::enter()?;
        dispatch_press(
            admit,
            || {
                if no_input() {
                    return Err("JUMPBOT_NO_INPUT 已禁止按压".into());
                }
                let target = self.press_target(point, background)?;
                if let Some(client @ ButtonTarget::Client(..)) = &target {
                    client.post(WM_MOUSEMOVE, 0)?;
                }
                Ok(target)
            },
            ButtonTarget::down,
            ButtonTarget::up,
            || hold_precise(duration),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureMode {
    Screen,
    PrintWindow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PressOutcome {
    Sent,
    WindowChanged,
    Suppressed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Overlay {
    Allowed,
    Other(u32),
}

fn allowed_overlay(class: &str, executable: &str) -> bool {
    // Sogou creates its IME windows inside other processes, including WeChat.
    // Match the vendor's window class, never the whole host or a generic IME class.
    if class.starts_with("SoPY_") || class == "Sogou_TSF_UI" {
        return true;
    }
    let path = std::path::Path::new(executable);
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    match name.as_str() {
        "nvidia overlay.exe"
        | "nvidia share.exe"
        | "frameview_x64.exe"
        | "qq.exe"
        | "sogousmartassistant.exe"
        | "sgsmartassistant.exe"
        | "sogoucloud.exe"
        | "sogouexe.exe"
        | "sogoucommgr.exe"
        | "sgmyinput.exe" => true,
        // QQNT installs generic helper names alongside the rest of its remoting runtime.
        "remoting_host.exe" | "qremoting_worker.exe" => path.ancestors().any(|p| {
            p.file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("QQNT") || n.eq_ignore_ascii_case("QQ"))
        }),
        _ => false,
    }
}

struct Cover {
    target: HWND,
    bounds: [i32; 4],
    found: Option<Overlay>,
}

unsafe extern "system" fn check_cover(hwnd: HWND, data: LPARAM) -> i32 {
    let cover = &mut *(data as *mut Cover);
    if hwnd == cover.target {
        return 0;
    }
    let mut rect = RECT::default();
    if IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 || GetWindowRect(hwnd, &mut rect) == 0 {
        return 1;
    }
    let [l, t, r, b] = cover.bounds;
    let (l, t, r, b) = (
        l.max(rect.left),
        t.max(rect.top),
        r.min(rect.right),
        b.min(rect.bottom),
    );
    if l >= r || t >= b {
        return 1;
    }
    // Cloaked windows still satisfy IsWindowVisible. A click-through overlay,
    // on the other hand, can corrupt pixels without winning WindowFromPoint.
    let mut cloaked: u32 = 0;
    if DwmGetWindowAttribute(
        hwnd,
        DWMWA_CLOAKED as u32,
        (&mut cloaked as *mut u32).cast(),
        mem::size_of::<u32>() as u32,
    ) >= 0
        && cloaked != 0
    {
        return 1;
    }
    let mut visible = rect;
    let mut bounds = RECT::default();
    if DwmGetWindowAttribute(
        hwnd,
        DWMWA_EXTENDED_FRAME_BOUNDS as u32,
        (&mut bounds as *mut RECT).cast(),
        mem::size_of::<RECT>() as u32,
    ) >= 0
    {
        visible = bounds;
    }
    let (l, t, r, b) = (
        l.max(visible.left),
        t.max(visible.top),
        r.min(visible.right),
        b.min(visible.bottom),
    );
    if l >= r || t >= b {
        return 1;
    }
    let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    if ex_style & WS_EX_LAYERED != 0 {
        let (mut alpha, mut flags) = (0, 0);
        if GetLayeredWindowAttributes(hwnd, null_mut(), &mut alpha, &mut flags) != 0
            && flags & LWA_ALPHA != 0
            && alpha == 0
        {
            return 1;
        }
    }
    let region = CreateRectRgn(0, 0, 0, 0);
    if !region.is_null() {
        let kind = GetWindowRgn(hwnd, region);
        let overlap = RECT {
            left: l - rect.left,
            top: t - rect.top,
            right: r - rect.left,
            bottom: b - rect.top,
        };
        // Unknown regions and mirrored layouts retain the rectangular check.
        let outside = kind == NULLREGION
            || (matches!(kind, SIMPLEREGION | COMPLEXREGION)
                && ex_style & WS_EX_LAYOUTRTL == 0
                && RectInRegion(region, &overlap) == 0);
        DeleteObject(region);
        if outside {
            return 1;
        }
    }
    let id = pid(hwnd);
    let mut class = [0u16; 256];
    let n = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32);
    let class = String::from_utf16_lossy(&class[..n as usize]);
    if allowed_overlay(&class, &executable(id)) {
        // Keep searching: a transparent sharing border can sit above a real blocker.
        cover.found = Some(Overlay::Allowed);
        return 1;
    }
    cover.found = Some(Overlay::Other(id));
    0
}

/// Draw into a top-down 24-bit DIB and return tightly packed BGR pixels.
pub(crate) fn capture_bgr(
    width: i32,
    height: i32,
    draw: impl FnOnce(HDC) -> Result<()>,
) -> Result<Frame> {
    ReusableSurface::new(width, height)?.capture(true, draw)
}

struct ReusableSurface {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    width: i32,
    height: i32,
    row: usize,
    stride: usize,
    bytes: usize,
}

impl ReusableSurface {
    fn new(width: i32, height: i32) -> Result<Self> {
        let (w, h) = (
            usize::try_from(width).map_err(|_| "截图宽度无效")?,
            usize::try_from(height).map_err(|_| "截图高度无效")?,
        );
        let row = w.checked_mul(3).ok_or("截图宽度过大")?;
        let stride = row.checked_add(3).ok_or("截图宽度过大")? & !3;
        let bytes = stride
            .checked_mul(h)
            .filter(|n| *n > 0 && *n <= isize::MAX as usize)
            .ok_or("截图尺寸无效")?;
        unsafe {
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 24,
                    biCompression: BI_RGB,
                    ..Default::default()
                },
                ..Default::default()
            };
            let dc = CreateCompatibleDC(null_mut());
            if dc.is_null() {
                return Err(os_error("CreateCompatibleDC"));
            }
            let mut bits: *mut c_void = null_mut();
            let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            let mut surface = Self {
                dc,
                bitmap,
                old: null_mut(),
                bits: bits.cast::<u8>(),
                width,
                height,
                row,
                stride,
                bytes,
            };
            if bitmap.is_null() || bits.is_null() {
                return Err(os_error("CreateDIBSection"));
            }
            surface.old = SelectObject(dc, bitmap);
            if surface.old.is_null() || surface.old as isize == -1 {
                return Err(os_error("SelectObject"));
            }
            Ok(surface)
        }
    }

    fn capture(&self, clear: bool, draw: impl FnOnce(HDC) -> Result<()>) -> Result<Frame> {
        unsafe {
            if clear {
                std::ptr::write_bytes(self.bits, 0, self.bytes);
            }
            draw(self.dc)?;
            if GdiFlush() == 0 {
                return Err(os_error("GdiFlush"));
            }
            let w = self.width as usize;
            let h = self.height as usize;
            let raw = std::slice::from_raw_parts(self.bits, self.bytes);
            let bgr = if self.stride == self.row {
                raw.to_vec()
            } else {
                let mut bgr = Vec::with_capacity(self.row * h);
                for line in raw.chunks_exact(self.stride) {
                    bgr.extend_from_slice(&line[..self.row]);
                }
                bgr
            };
            Ok(Frame {
                width: w,
                height: h,
                bgr,
            })
        }
    }
}

impl Drop for ReusableSurface {
    fn drop(&mut self) {
        unsafe {
            if !self.old.is_null() && self.old as isize != -1 {
                SelectObject(self.dc, self.old);
            }
            DeleteDC(self.dc);
            if !self.bitmap.is_null() {
                DeleteObject(self.bitmap);
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct Capturer {
    surface: Option<ReusableSurface>,
    reference_task: Option<std::thread::JoinHandle<()>>,
}

impl Capturer {
    /// PrintWindow runs synchronously inside the window owner. An unresponsive
    /// foreign window must not stall the foreground controls or pile up requests.
    pub(crate) fn reference_frame(&mut self, window: &Window) -> Option<Frame> {
        if self
            .reference_task
            .as_ref()
            .is_some_and(|task| !task.is_finished())
        {
            return None;
        }
        let window = window.clone();
        let (send, receive) = std::sync::mpsc::channel();
        self.reference_task = Some(
            std::thread::Builder::new()
                .spawn(move || {
                    let frame = window.capture(CaptureMode::PrintWindow).ok().flatten();
                    let _ = send.send(frame);
                })
                .ok()?,
        );
        receive
            .recv_timeout(Duration::from_millis(250))
            .ok()
            .flatten()
    }

    fn draw(
        &mut self,
        width: i32,
        height: i32,
        clear: bool,
        draw: impl FnOnce(HDC) -> Result<()>,
    ) -> Result<Frame> {
        let reuse = self
            .surface
            .as_ref()
            .is_some_and(|s| s.width == width && s.height == height);
        if !reuse {
            self.surface = None;
            self.surface = Some(ReusableSurface::new(width, height)?);
        }
        self.surface.as_ref().unwrap().capture(clear, draw)
    }

    pub(crate) fn capture(&mut self, window: &Window, mode: CaptureMode) -> Result<Option<Frame>> {
        window.capture_with(mode, |w, h, clear, draw| self.draw(w, h, clear, draw))
    }
}

enum ButtonTarget {
    Desktop(i32, i32),
    Client(HWND, LPARAM),
}

impl ButtonTarget {
    fn post(&self, message: u32, buttons: usize) -> Result<()> {
        let Self::Client(hwnd, point) = *self else {
            unreachable!()
        };
        if unsafe { PostMessageW(hwnd, message, buttons, point) } == 0 {
            Err(os_error("PostMessageW"))
        } else {
            Ok(())
        }
    }

    fn down(&self) -> Result<()> {
        match *self {
            Self::Client(..) => self.post(WM_LBUTTONDOWN, 1),
            Self::Desktop(x, y) => send_mouse(
                x,
                y,
                MOUSEEVENTF_MOVE
                    | MOUSEEVENTF_ABSOLUTE
                    | MOUSEEVENTF_VIRTUALDESK
                    | MOUSEEVENTF_LEFTDOWN,
            ),
        }
    }

    fn up(&self) -> Result<()> {
        match *self {
            Self::Client(..) => self.post(WM_LBUTTONUP, 0),
            Self::Desktop(..) => send_mouse(0, 0, MOUSEEVENTF_LEFTUP),
        }
    }
}

fn send_mouse(x: i32, y: i32, flags: MOUSE_EVENT_FLAGS) -> Result<()> {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: x,
                dy: y,
                dwFlags: flags,
                ..Default::default()
            },
        },
    };
    if unsafe { SendInput(1, &input, mem::size_of::<INPUT>() as i32) } != 1 {
        Err(os_error("SendInput"))
    } else {
        Ok(())
    }
}

struct Release<F: FnOnce() -> Result<()>>(Option<F>);

impl<F: FnOnce() -> Result<()>> Drop for Release<F> {
    fn drop(&mut self) {
        if let Some(up) = self.0.take() {
            let _ = up();
        }
    }
}

fn dispatch_press<G, T>(
    admit: impl FnOnce() -> Result<G>,
    prepare: impl FnOnce() -> Result<Option<T>>,
    down: impl FnOnce(&T) -> Result<()>,
    up: impl FnOnce(&T) -> Result<()>,
    hold: impl FnOnce(),
) -> Result<PressOutcome> {
    let guard = admit()?;
    let Some(target) = prepare()? else {
        return Ok(PressOutcome::WindowChanged);
    };
    let mut release = Release(Some(|| up(&target)));
    let started = down(&target);
    drop(guard);
    if started.is_ok() {
        hold();
    }
    release.0.take().unwrap()()?;
    started.map(|_| PressOutcome::Sent)
}

fn hold_precise(duration: Duration) {
    let start = Instant::now();
    std::thread::sleep(duration.saturating_sub(Duration::from_millis(3)));
    while start.elapsed() < duration {
        std::hint::spin_loop();
    }
}

pub fn parse_hotkey(text: &str) -> Result<(u32, u32)> {
    let compact = text.replace(' ', "").to_ascii_uppercase();
    let (prefix, key) = compact
        .rsplit_once('+')
        .ok_or("暂停热键需要修饰键和字母或数字，例如 Alt+P")?;
    let mut mask = MOD_NOREPEAT;
    for part in prefix.split('+') {
        mask |= match part {
            "ALT" => MOD_ALT,
            "CTRL" | "CONTROL" => MOD_CONTROL,
            "SHIFT" => MOD_SHIFT,
            _ => return Err(format!("未知热键修饰键: {part}")),
        };
    }
    let [key] = *key.as_bytes() else {
        return Err("热键末键必须是一个英文字母或数字".into());
    };
    if !key.is_ascii_alphanumeric() || key == b'Q' || (key == b'C' && mask & MOD_CONTROL != 0) {
        return Err("暂停热键末键无效或与停止键冲突".into());
    }
    Ok((mask, u32::from(key)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hotkey {
    TogglePause,
    Stop,
}

/// Register and poll on the same dedicated thread. Failure to register must
/// keep the parent paused. Poll independently of capture, recognition and hold.
pub struct Hotkeys {
    _same_thread: PhantomData<Rc<()>>,
}

impl Hotkeys {
    const PAUSE: i32 = 0x4a50;
    const STOP: i32 = 0x4a51;

    pub fn register(pause: &str) -> Result<Self> {
        let (mask, key) = parse_hotkey(pause)?;
        unsafe {
            if RegisterHotKey(null_mut(), Self::PAUSE, mask, key) == 0 {
                return Err(os_error("注册暂停热键"));
            }
            if RegisterHotKey(null_mut(), Self::STOP, MOD_NOREPEAT, VK_F8.into()) == 0 {
                let error = os_error("注册 F8 停止键");
                UnregisterHotKey(null_mut(), Self::PAUSE);
                return Err(error);
            }
        }
        Ok(Self {
            _same_thread: PhantomData,
        })
    }

    pub fn poll(&mut self, mut event: impl FnMut(Hotkey)) {
        let mut message = MSG::default();
        unsafe {
            // Also stop when F8 is already held or pressed with a modifier.
            if GetAsyncKeyState(i32::from(VK_F8)) < 0 {
                event(Hotkey::Stop);
                return;
            }
            while PeekMessageW(
                &mut message,
                -1isize as HWND,
                WM_HOTKEY,
                WM_HOTKEY,
                PM_REMOVE,
            ) != 0
            {
                if message.message == WM_HOTKEY {
                    match message.wParam as i32 {
                        Self::STOP => event(Hotkey::Stop),
                        Self::PAUSE => event(Hotkey::TogglePause),
                        _ => (),
                    }
                }
            }
        }
    }
}

impl Drop for Hotkeys {
    fn drop(&mut self) {
        unsafe {
            UnregisterHotKey(null_mut(), Self::STOP);
            UnregisterHotKey(null_mut(), Self::PAUSE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::Cell,
        panic::{catch_unwind, AssertUnwindSafe},
    };

    fn fixture_window(hwnd: HWND) -> Window {
        Window {
            hwnd: hwnd as isize,
            pid: pid(hwnd),
            title: title(hwnd),
            exe: executable(pid(hwnd)),
            client_rect: client_rect(hwnd)
                .unwrap()
                .expect("fixture has a client area"),
        }
    }

    #[test]
    fn overlay_allowlist_is_specific_to_the_app_or_ime_window() {
        for (class, exe) in [
            ("", r"C:\NVIDIA App\CEF\NVIDIA Overlay.exe"),
            ("", "FrameView_x64.exe"),
            ("", r"D:\Apps\QQNT\QQ.exe"),
            ("", r"D:\QQNT\versions\42\remoting_host.exe"),
            ("", "SOGOUSmartAssistant.exe"),
            ("SoPY_Comp", "explorer.exe"),
            ("Sogou_TSF_UI", "WeChatAppEx.exe"),
        ] {
            assert!(allowed_overlay(class, exe), "{class} {exe}");
        }
        for (class, exe) in [
            ("Chrome_WidgetWin_1", "chrome.exe"),
            ("IME", "ctfmon.exe"),
            ("", "QQMusic.exe"),
            ("", "QQ.exe.bak"),
            ("", r"D:\Other\remoting_host.exe"),
            ("", "nvcontainer.exe"),
            ("", ""),
            ("WeChatAppEx", "WeChatAppEx.exe"),
        ] {
            assert!(!allowed_overlay(class, exe), "{class} {exe}");
        }
    }

    #[test]
    #[ignore = "requires an exclusive Windows foreground desktop"]
    fn foreground_overlay_capture() {
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
        assert!(
            std::env::var("JUMPBOT_TEST_FOREGROUND").as_deref() == Ok("1") && no_input(),
            "requires JUMPBOT_TEST_FOREGROUND=1 and JUMPBOT_NO_INPUT=1 on an exclusive desktop"
        );
        let _dpi = PhysicalPixels::enter().unwrap();
        let cfg = Config::default();
        let point = (cfg.at("press_point", 0), cfg.at("press_point", 1));
        let x = (160. * point.0) as i32;
        let y = (120. * point.1) as i32;
        unsafe {
            // This read requires access to the input desktop and never moves the cursor.
            assert_ne!(
                GetCursorPos(&mut POINT::default()),
                0,
                "active input desktop unavailable: {}",
                os_error("GetCursorPos")
            );
            let instance = GetModuleHandleW(null());
            let pane = |name: &str, color, ex, style, parent, [left, top, w, h]: [i32; 4]| {
                let name: Vec<_> = name.encode_utf16().chain(Some(0)).collect();
                let class = WNDCLASSW {
                    lpfnWndProc: Some(DefWindowProcW),
                    hInstance: instance,
                    hbrBackground: CreateSolidBrush(color),
                    lpszClassName: name.as_ptr(),
                    ..Default::default()
                };
                assert!(
                    !class.hbrBackground.is_null(),
                    "{}",
                    os_error("CreateSolidBrush")
                );
                assert_ne!(RegisterClassW(&class), 0, "{}", os_error("RegisterClassW"));
                let hwnd = CreateWindowExW(
                    ex,
                    name.as_ptr(),
                    name.as_ptr(),
                    style,
                    left,
                    top,
                    w,
                    h,
                    parent,
                    null_mut(),
                    instance,
                    null(),
                );
                assert!(!hwnd.is_null(), "{}", os_error("CreateWindowExW"));
                hwnd
            };
            let board = pane(
                "JumpBotCiBoard",
                0xffffff,
                0,
                WS_POPUP | WS_CLIPCHILDREN,
                null_mut(),
                [64, 64, 160, 120],
            );
            pane(
                "JumpBotCiPawn",
                0,
                0,
                WS_CHILD | WS_VISIBLE,
                board,
                [x - 8, y - 10, 16, 20],
            );
            let style = WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_LAYERED;
            let overlay = pane(
                "JumpBotCiCover",
                0x0000ff,
                style,
                WS_POPUP,
                null_mut(),
                [0, 0, 24, 32],
            );
            assert_ne!(SetLayeredWindowAttributes(overlay, 0, 255, LWA_ALPHA), 0);
            ShowWindow(board, SW_SHOW);
            let activated = SetForegroundWindow(board);
            let window = fixture_window(board);
            let [left, top, _, _] = window.client_rect;
            let press = POINT {
                x: left + x,
                y: top + y,
            };
            const WHITE: [u8; 3] = [255; 3];
            const BLACK: [u8; 3] = [0; 3];
            const RED: [u8; 3] = [0, 0, 255]; // Captured BGR.
            for (stage, at, edge, pawn, through, route) in [
                ("normal", None, WHITE, BLACK, false, board),
                ("edge", Some((154, 6)), RED, BLACK, false, board),
                (
                    "covered",
                    Some((x - 12, y - 16)),
                    WHITE,
                    RED,
                    false,
                    overlay,
                ),
                (
                    "click-through",
                    Some((x - 12, y - 16)),
                    WHITE,
                    RED,
                    true,
                    board,
                ),
                ("restored", Some((168, 6)), WHITE, BLACK, false, board),
            ] {
                if let Some((dx, dy)) = at {
                    let ex = if through {
                        style | WS_EX_TRANSPARENT
                    } else {
                        style
                    };
                    assert_ne!(SetWindowLongW(overlay, GWL_EXSTYLE, ex as i32), 0);
                    assert_ne!(
                        SetWindowPos(
                            overlay,
                            HWND_TOPMOST,
                            left + dx,
                            top + dy,
                            0,
                            0,
                            SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_NOSIZE
                        ),
                        0
                    );
                    UpdateWindow(overlay);
                }
                let expected = [edge, pawn, pawn, pawn, WHITE];
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    let mut msg = MSG::default();
                    while PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
                        DispatchMessageW(&msg);
                    }
                    let pixels = window.capture(CaptureMode::Screen).map(|frame| {
                        frame.map(|f| {
                            [(156, 12), (x - 7, y - 9), (x, y), (x + 7, y + 9), (12, 100)].map(
                                |(sx, sy)| {
                                    f.bgr.as_chunks::<3>().0[sy as usize * f.width + sx as usize]
                                },
                            )
                        })
                    });
                    let hit = GetAncestor(WindowFromPoint(press), GA_ROOT);
                    if pixels == Ok(Some(expected))
                        && hit == route
                        && point_is_target(board, press) == (route == board)
                    {
                        println!(
                            "{stage}: captured BGR {pixels:?}, press routed to {}",
                            if hit == board { "board" } else { "overlay" }
                        );
                        break;
                    }
                    assert!(Instant::now() < deadline,
                        "{stage}: expected {expected:?}, got {pixels:?}; hit={hit:p}, expected={route:p}; foreground={:p}, board={board:p}, SetForegroundWindow={activated}",
                        GetForegroundWindow());
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
            let pawn_bounds = [press.x - 8, press.y - 10, press.x + 8, press.y + 10];
            let ime = pane(
                "SoPY_JumpBotCi",
                0x00ff00,
                WS_EX_NOACTIVATE | WS_EX_TOPMOST,
                WS_POPUP,
                null_mut(),
                [press.x - 12, press.y - 16, 24, 32],
            );
            ShowWindow(ime, SW_SHOWNOACTIVATE);
            UpdateWindow(ime);
            assert_eq!(window.overlap(pawn_bounds), Some(Overlay::Allowed));
            assert_eq!(window.covering_pid(pawn_bounds), None);
            assert_eq!(WindowFromPoint(press), ime);
            assert!(window.is_foreground());
            assert!(window.press_target(point, false).unwrap().is_none());

            // Insert the ordinary overlay immediately below the IME at the same point.
            assert_ne!(
                SetWindowPos(
                    overlay,
                    ime,
                    press.x - 12,
                    press.y - 16,
                    0,
                    0,
                    SWP_NOACTIVATE | SWP_NOSIZE
                ),
                0
            );
            let blocker = pid(overlay);
            assert_eq!(window.overlap(pawn_bounds), Some(Overlay::Other(blocker)));
            assert_eq!(window.covering_pid(pawn_bounds), Some(blocker));
            assert_eq!(WindowFromPoint(press), ime);
            assert!(window.press_target(point, false).unwrap().is_none());
            assert_ne!(DestroyWindow(ime), 0);
            assert_eq!(WindowFromPoint(press), overlay);
            assert_ne!(DestroyWindow(overlay), 0);
            assert!(window.press_target(point, false).unwrap().is_some());
            DestroyWindow(board);
        }
    }

    #[test]
    fn ctrl_hotkey_and_no_input_fixture() {
        use std::{os::windows::process::CommandExt, process::Command};
        use windows_sys::Win32::System::Threading::GetCurrentThreadId;
        const CHILD: &str = "JUMPBOT_PLATFORM_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            // Isolate the no-input environment from tests running in parallel.
            let output = Command::new(std::env::current_exe().unwrap())
                .creation_flags(0x0800_0000)
                .args([
                    "--exact",
                    "platform::tests::ctrl_hotkey_and_no_input_fixture",
                ])
                .env(CHILD, "1")
                .env("JUMPBOT_NO_INPUT", "1")
                .output()
                .unwrap();
            let report = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && report.contains("1 passed"),
                "{report}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        assert!(no_input());

        // Never switch the interactive desktop. The short-lived child owns
        // this private desktop and all of its windows and registrations.
        #[link(name = "user32")]
        extern "system" {
            fn CreateDesktopW(
                name: *const u16,
                device: *const u16,
                mode: *const DEVMODEW,
                flags: u32,
                access: u32,
                security: *const windows_sys::Win32::Security::SECURITY_ATTRIBUTES,
            ) -> *mut c_void;
            fn SetThreadDesktop(desktop: *mut c_void) -> i32;
        }
        let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
        let name = wide(&format!("JumpBotNoInputFixture{}", std::process::id()));
        unsafe {
            // DESKTOP_READOBJECTS | CREATEWINDOW | ENUMERATE | WRITEOBJECTS.
            let desk = CreateDesktopW(name.as_ptr(), null(), null(), 0, 0xC3, null());
            assert!(!desk.is_null(), "{}", os_error("CreateDesktopW"));
            assert_ne!(
                SetThreadDesktop(desk),
                0,
                "{}",
                os_error("SetThreadDesktop")
            );
            let caption = "JumpBot Ctrl fixture";
            let make_window = |caption: &str, style, x, y| {
                let hwnd = CreateWindowExW(
                    WS_EX_NOACTIVATE | style,
                    wide("STATIC").as_ptr(),
                    wide(caption).as_ptr(),
                    WS_POPUP,
                    x,
                    y,
                    90,
                    60,
                    null_mut(),
                    null_mut(),
                    null_mut(),
                    null(),
                );
                assert!(!hwnd.is_null(), "{}", os_error("CreateWindowExW"));
                ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                hwnd
            };
            let _dpi = PhysicalPixels::enter().unwrap();
            let hwnd = make_window(caption, 0, 30, 30);
            let window = fixture_window(hwnd);
            assert_eq!(window.title, caption);
            ShowWindow(hwnd, SW_HIDE);
            assert!(window.same_instance_exists());
            assert_eq!(window.still_current(), Ok(false));
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            // A visible HWND may temporarily have no drawable client area during a resize.
            assert_ne!(
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    30,
                    30,
                    0,
                    0,
                    SWP_NOACTIVATE | SWP_NOZORDER
                ),
                0
            );
            assert_eq!(window.still_current(), Ok(false));
            assert!(window.capture(CaptureMode::PrintWindow).unwrap().is_none());
            // Only this private fixture window moves. Its old capture must be discarded
            // even when the HWND and PID are unchanged.
            assert_ne!(
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    35,
                    32,
                    110,
                    70,
                    SWP_NOACTIVATE | SWP_NOZORDER
                ),
                0
            );
            assert!(window.capture(CaptureMode::PrintWindow).unwrap().is_none());
            assert_ne!(
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    30,
                    30,
                    90,
                    60,
                    SWP_NOACTIVATE | SWP_NOZORDER
                ),
                0
            );
            assert_eq!(window.still_current(), Ok(true));
            // A non-black solid first pass must retry. One changed channel in the
            // final pixel of the fallback is enough to preserve a low-contrast frame.
            for detail in [false, true] {
                let mut attempts = 0;
                let frame = window.capture_with(CaptureMode::PrintWindow, |w, h, _, _| {
                    attempts += 1;
                    let mut bgr = [8, 5, 3].repeat((w * h) as usize);
                    if detail && attempts == 2 {
                        *bgr.last_mut().unwrap() += 1;
                    }
                    Ok(Frame {
                        width: w as usize,
                        height: h as usize,
                        bgr,
                    })
                });
                assert_eq!(attempts, 2);
                if detail {
                    assert!(frame.unwrap().is_some());
                } else {
                    assert!(frame.err().unwrap().contains("单一颜色"));
                }
            }
            assert!(window.covering_pid(window.client_rect).is_none());
            let overlay = make_window(
                "Click-through overlay",
                WS_EX_TOPMOST | WS_EX_TRANSPARENT | WS_EX_LAYERED,
                110,
                75,
            );
            // Plain STATIC text passes hits through even without a layered overlay.
            const SS_NOTIFY: i32 = 0x0100;
            for control in [hwnd, overlay] {
                let style = GetWindowLongW(control, GWL_STYLE);
                assert_ne!(SetWindowLongW(control, GWL_STYLE, style | SS_NOTIFY), 0);
            }
            let cfg = Config::default();
            let [left, top, _, _] = window.client_rect;
            let press = POINT {
                x: left + (cfg.at("press_point", 0) * window.width() as f64) as i32,
                y: top + (cfg.at("press_point", 1) * window.height() as f64) as i32,
            };
            let press_pixel = [press.x, press.y, press.x + 1, press.y + 1];
            // The golden-piece fixture's box mapped into this 90 by 60 client.
            let piece = [54, 60, 62, 67];
            let owner = pid(overlay);
            assert!(point_is_target(hwnd, press));
            for alpha in [0, 1, 255] {
                assert_ne!(SetLayeredWindowAttributes(overlay, 0, alpha, LWA_ALPHA), 0);
                let expected = (alpha != 0).then_some(owner);
                assert_eq!(window.covering_pid(window.client_rect), expected);
                assert_eq!(window.covering_pid([110, 75, 111, 76]), expected);
                assert_eq!(window.covering_pid(piece), None);
                assert_eq!(window.covering_pid(press_pixel), None);
            }
            // The two bars leave a hole at the only corner overlapping the target.
            // Extending one bar by a single pixel reports a possible intersection only.
            for edge in [10, 9] {
                let bottom = CreateRectRgn(0, 15, 90, 60);
                let right = CreateRectRgn(edge, 0, 90, 15);
                assert_eq!(CombineRgn(bottom, bottom, right, RGN_OR), COMPLEXREGION);
                DeleteObject(right);
                assert_ne!(SetWindowRgn(overlay, bottom, 0), 0);
                assert_eq!(
                    window.covering_pid(window.client_rect).is_none(),
                    edge == 10
                );
            }
            assert_ne!(SetWindowRgn(overlay, null_mut(), 0), 0);
            for bounds in [piece, press_pixel] {
                let [x, y, r, b] = bounds;
                assert_ne!(
                    SetWindowPos(
                        overlay,
                        null_mut(),
                        x,
                        y,
                        r - x,
                        b - y,
                        SWP_NOACTIVATE | SWP_NOZORDER
                    ),
                    0
                );
                assert_eq!(
                    window.covering_pid(piece),
                    (bounds == piece).then_some(owner)
                );
                assert_eq!(
                    window.covering_pid(press_pixel),
                    (bounds == press_pixel).then_some(owner)
                );
                assert_eq!(WindowFromPoint(press), hwnd);
            }
            // Same pixel and same HWND. Let Windows route hits by alpha alone.
            let style = GetWindowLongW(overlay, GWL_EXSTYLE);
            assert_ne!(
                SetWindowLongW(overlay, GWL_EXSTYLE, style & !(WS_EX_TRANSPARENT as i32)),
                0
            );
            for (alpha, hit) in [(0, hwnd), (255, overlay)] {
                assert_ne!(SetLayeredWindowAttributes(overlay, 0, alpha, LWA_ALPHA), 0);
                assert_eq!(
                    window.covering_pid(press_pixel),
                    (alpha != 0).then_some(owner)
                );
                assert_eq!(WindowFromPoint(press), hit);
                assert_eq!(point_is_target(hwnd, press), hit == hwnd);
            }
            assert_ne!(DestroyWindow(overlay), 0);
            assert!(point_is_target(hwnd, press));
            assert!(window.covering_pid(window.client_rect).is_none());
            assert!(window
                .capture(CaptureMode::Screen)
                .err()
                .unwrap()
                .contains("前台"));

            // Losing foreground after capture cancels this plan before any input is sent.
            assert!(window.press_target((0.5, 0.72), true).unwrap().is_some());
            let skipped = dispatch_press(
                || Ok(()),
                || window.press_target((0.5, 0.72), false),
                |_| panic!("unexpected DOWN"),
                |_| panic!("unexpected UP"),
                || panic!("unexpected hold"),
            );
            assert_eq!(skipped, Ok(PressOutcome::WindowChanged));

            let mut keys = Hotkeys::register("Ctrl+Shift+7").unwrap();
            assert_eq!(
                parse_hotkey("Control+Shift+7").unwrap(),
                (MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT, u32::from(b'7'))
            );
            // Check the actual chord reserved in Windows, then feed the
            // resulting message format to the listener without typing a key.
            let desktop = desk as isize;
            let collision = std::thread::spawn(move || {
                assert_ne!(SetThreadDesktop(desktop as *mut c_void), 0);
                let ok = RegisterHotKey(null_mut(), 1, MOD_CONTROL | MOD_SHIFT, u32::from(b'7'));
                let err = GetLastError();
                if ok != 0 {
                    UnregisterHotKey(null_mut(), 1);
                }
                (ok, err)
            })
            .join()
            .unwrap();
            assert_eq!(
                collision,
                (
                    0,
                    windows_sys::Win32::Foundation::ERROR_HOTKEY_ALREADY_REGISTERED
                )
            );
            for (id, chord) in [
                (
                    Hotkeys::PAUSE,
                    ((b'7' as isize) << 16) | (MOD_CONTROL | MOD_SHIFT) as isize,
                ),
                (Hotkeys::STOP, (VK_F8 as isize) << 16),
            ] {
                assert_ne!(
                    PostThreadMessageW(GetCurrentThreadId(), WM_HOTKEY, id as usize, chord),
                    0
                );
            }
            let mut events = Vec::new();
            keys.poll(|e| events.push(e));
            keys.poll(|e| events.push(e));
            assert_eq!(events, [Hotkey::TogglePause, Hotkey::Stop]);

            for background in [false, true] {
                assert_eq!(
                    window.press((0.5, 0.72), Duration::ZERO, background, || -> Result<()> {
                        panic!("replay entered input admission")
                    }),
                    Ok(PressOutcome::Suppressed)
                );
            }
            assert_eq!(
                PeekMessageW(
                    &mut MSG::default(),
                    hwnd,
                    WM_MOUSEFIRST,
                    WM_MOUSELAST,
                    PM_REMOVE
                ),
                0
            );
            drop(keys);
            drop(Hotkeys::register("Ctrl+Shift+7").unwrap());
            assert_ne!(DestroyWindow(hwnd), 0);
            assert!(!window.same_instance_exists());
        }
    }

    #[test]
    fn admission_covers_down_and_unlocks_before_hold_and_up() {
        let lock = Mutex::new(());
        let phase = Cell::new(0);
        dispatch_press(
            || Ok(lock.lock().unwrap()),
            || Ok(Some(())),
            |_| {
                assert!(lock.try_lock().is_err());
                phase.set(1);
                Ok(())
            },
            |_| {
                assert!(lock.try_lock().is_ok());
                assert_eq!(phase.replace(3), 2);
                Ok(())
            },
            || {
                assert!(lock.try_lock().is_ok());
                assert_eq!(phase.replace(2), 1);
            },
        )
        .unwrap();
        assert_eq!(phase.get(), 3);
        let moved = dispatch_press(
            || Ok(()),
            || Ok(None::<()>),
            |_| panic!("stale down"),
            |_| panic!("unowned release"),
            || panic!("stale hold"),
        );
        assert_eq!(moved, Ok(PressOutcome::WindowChanged));
        let denied = dispatch_press(
            || Err::<(), _>("paused".into()),
            || -> Result<Option<()>> { panic!() },
            |_| panic!(),
            |_| panic!(),
            || panic!(),
        );
        assert_eq!(denied.unwrap_err(), "paused");
    }

    #[test]
    fn down_failure_and_interrupted_hold_each_release_once() {
        let releases = Cell::new(0);
        let up = |_: &()| {
            releases.set(releases.get() + 1);
            Ok(())
        };
        let failed = dispatch_press(
            || Ok(()),
            || Ok(Some(())),
            |_| Err("down failed".into()),
            up,
            || panic!(),
        );
        assert_eq!(failed.unwrap_err(), "down failed");
        assert_eq!(releases.get(), 1);
        assert!(catch_unwind(AssertUnwindSafe(|| dispatch_press(
            || Ok(()),
            || Ok(Some(())),
            |_| Ok(()),
            up,
            || panic!("interrupted hold")
        )))
        .is_err());
        assert_eq!(releases.get(), 2);
    }

    #[test]
    fn gdi_pixels_are_top_down_bgr_without_row_padding() {
        // 3 pixels use 9 bytes in Frame and 12 bytes in the native scanline.
        let frame = capture_bgr(3, 2, |dc| {
            for (x, y, color) in [(2, 0, 0x00332211), (0, 1, 0x00665544)] {
                if unsafe { SetPixelV(dc, x, y, color) } == 0 {
                    return Err(os_error("SetPixelV"));
                }
            }
            Ok(())
        })
        .unwrap();
        assert_eq!((frame.width, frame.height), (3, 2));
        assert_eq!(
            frame.bgr,
            [0, 0, 0, 0, 0, 0, 0x33, 0x22, 0x11, 0x66, 0x55, 0x44, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn capturer_reuses_surface_and_handles_resize_and_clear() {
        let mut cap = Capturer::default();
        let f1 = cap
            .draw(3, 2, true, |dc| {
                if unsafe { SetPixelV(dc, 2, 0, 0x00112233) } == 0 {
                    return Err(os_error("SetPixelV"));
                }
                Ok(())
            })
            .unwrap();
        let ptr1 = cap.surface.as_ref().unwrap().bits;
        assert_eq!((f1.width, f1.height), (3, 2));
        assert_eq!(&f1.bgr[6..9], &[0x11, 0x22, 0x33]);

        // Second same-size capture without clear reuses the exact same DIB allocation.
        let f2 = cap
            .draw(3, 2, false, |dc| {
                if unsafe { SetPixelV(dc, 0, 1, 0x00445566) } == 0 {
                    return Err(os_error("SetPixelV"));
                }
                Ok(())
            })
            .unwrap();
        let ptr2 = cap.surface.as_ref().unwrap().bits;
        assert_eq!(ptr1, ptr2);
        assert_eq!(&f2.bgr[6..12], &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);

        // Size change allocates a fresh matching DIB and zero-fills when clear=true.
        let f3 = cap.draw(4, 2, true, |_| Ok(())).unwrap();
        assert_eq!((f3.width, f3.height), (4, 2));
        assert!(f3.bgr.iter().all(|&b| b == 0));
    }
}
