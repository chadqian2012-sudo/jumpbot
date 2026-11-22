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

fn executable(id: u32) -> String {
    unsafe {
        let raw = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, id);
        if raw.is_null() {
            return String::new();
        }
        let _handle = OwnedHandle::from_raw_handle(raw);
        let mut buf = vec![0; 32768];
        let mut len = buf.len() as u32;
        if QueryFullProcessImageNameW(raw, 0, buf.as_mut_ptr(), &mut len) == 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..len as usize])
    }
}

fn client_rect(hwnd: HWND) -> Result<[i32; 4]> {
    let mut rect = RECT::default();
    let mut origin = POINT::default();
    unsafe {
        if GetClientRect(hwnd, &mut rect) == 0 || ClientToScreen(hwnd, &mut origin) == 0 {
            return Err(os_error("读取客户区位置"));
        }
    }
    if rect.right <= 0 || rect.bottom <= 0 {
        return Err("窗口没有可截图的客户区".into());
    }
    Ok([
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
    ])
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
    let Ok(rect) = client_rect(hwnd) else {
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
        let pattern = Regex::new(cfg.s("window_title_pattern"))
            .map_err(|e| format!("窗口标题正则无效: {e}"))?;
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

    pub(crate) fn still_current(&self) -> Result<bool> {
        let _dpi = PhysicalPixels::enter()?;
        let hwnd = self.handle();
        if unsafe { IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 }
            || pid(hwnd) != self.pid
            || title(hwnd) != self.title
        {
            return Ok(false);
        }
        Ok(client_rect(hwnd)? == self.client_rect)
    }

    pub fn is_foreground(&self) -> bool {
        unsafe { GetForegroundWindow() == self.handle() && pid(self.handle()) == self.pid }
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
        let [l, t, r, b] = self.client_rect;
        for (x, y) in [(l, t), (r - 1, t), (l, b - 1), (r - 1, b - 1)] {
            if unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONULL) }.is_null() {
                return Err("游戏窗口部分位于屏幕外".into());
            }
        }
        self.check_occlusion()
    }

    fn check_occlusion(&self) -> Result<()> {
        let mut cover = Cover {
            target: self,
            stopped_at: null_mut(),
        };
        unsafe { EnumWindows(Some(check_cover), &mut cover as *mut _ as LPARAM) };
        if cover.stopped_at == self.handle() {
            return Ok(());
        }
        if cover.stopped_at.is_null() {
            return Err("无法在桌面窗口列表中确认游戏窗口".into());
        }
        let id = pid(cover.stopped_at);
        let path = executable(id);
        let name = std::path::Path::new(&path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("未知程序");
        Err(format!("游戏客户区被其他窗口遮挡（{name}，PID {id}）"))
    }

    /// Screen reads visible desktop pixels. PrintWindow requests client pixels
    /// from the target itself and can be used when the desktop is occluded.
    pub fn capture(&self, mode: CaptureMode) -> Result<Option<Frame>> {
        let _dpi = PhysicalPixels::enter()?;
        if !self.still_current()? {
            return Ok(None);
        }
        if mode == CaptureMode::Screen {
            self.check_screen()?;
        }
        let width = self.width() as i32;
        let height = self.height() as i32;
        let frame = match mode {
            CaptureMode::Screen => capture_bgr(width, height, |dc| unsafe {
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
                let print = |flags| {
                    capture_bgr(width, height, |dc| {
                        if unsafe { PrintWindow(self.handle(), dc, flags) } != 0 {
                            Ok(())
                        } else {
                            Err(os_error("PrintWindow"))
                        }
                    })
                };
                // Some Windows desktops report success for full-content rendering
                // while returning a black surface. Request the plain client DC there.
                match print(PW_CLIENTONLY | PW_RENDERFULLCONTENT) {
                    Ok(frame) if frame.bgr.iter().any(|&byte| byte != 0) => Ok(frame),
                    _ => print(PW_CLIENTONLY),
                }
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
                if !self.still_current()? {
                    return Ok(None);
                }
                if !visible {
                    return Err("游戏窗口不在前台或按压点被遮挡".into());
                }
                if background {
                    target.post(WM_MOUSEMOVE, 0)?;
                }
                Ok(Some(target))
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

struct Cover<'a> {
    target: &'a Window,
    stopped_at: HWND,
}

unsafe extern "system" fn check_cover(hwnd: HWND, data: LPARAM) -> i32 {
    let cover = &mut *(data as *mut Cover<'_>);
    if hwnd == cover.target.handle() {
        cover.stopped_at = hwnd;
        return 0;
    }
    let mut rect = RECT::default();
    if IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 || GetWindowRect(hwnd, &mut rect) == 0 {
        return 1;
    }
    let [l, t, r, b] = cover.target.client_rect;
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
    cover.stopped_at = hwnd;
    0
}

/// Draw into a top-down 24-bit DIB and return tightly packed BGR pixels.
pub(crate) fn capture_bgr(
    width: i32,
    height: i32,
    draw: impl FnOnce(HDC) -> Result<()>,
) -> Result<Frame> {
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
        let mut surface = Surface {
            dc,
            bitmap,
            old: null_mut(),
        };
        if bitmap.is_null() || bits.is_null() {
            return Err(os_error("CreateDIBSection"));
        }
        surface.old = SelectObject(dc, bitmap);
        if surface.old.is_null() || surface.old as isize == -1 {
            return Err(os_error("SelectObject"));
        }
        let pixels = bits.cast::<u8>();
        std::ptr::write_bytes(pixels, 0, bytes);
        draw(dc)?;
        if GdiFlush() == 0 {
            return Err(os_error("GdiFlush"));
        }
        let mut bgr = Vec::with_capacity(row * h);
        for line in std::slice::from_raw_parts(pixels, bytes).chunks_exact(stride) {
            bgr.extend_from_slice(&line[..row]);
        }
        Ok(Frame {
            width: w,
            height: h,
            bgr,
        })
    }
}

struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
}

impl Drop for Surface {
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
        sync::Mutex,
    };

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
            let window = Window {
                hwnd: hwnd as isize,
                pid: pid(hwnd),
                title: title(hwnd),
                exe: executable(pid(hwnd)),
                client_rect: client_rect(hwnd).unwrap(),
            };
            assert_eq!(window.title, caption);
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
            window.check_occlusion().unwrap();
            let overlay = make_window(
                "Click-through overlay",
                WS_EX_TOPMOST | WS_EX_TRANSPARENT | WS_EX_LAYERED,
                110,
                75,
            );
            for alpha in [0, 1, 255] {
                assert_ne!(SetLayeredWindowAttributes(overlay, 0, alpha, LWA_ALPHA), 0);
                assert_eq!(window.check_occlusion().is_ok(), alpha == 0);
            }
            // The two bars leave a hole at the only corner overlapping the target.
            // Extending one bar by a single pixel must block capture again.
            for edge in [10, 9] {
                let bottom = CreateRectRgn(0, 15, 90, 60);
                let right = CreateRectRgn(edge, 0, 90, 15);
                assert_eq!(CombineRgn(bottom, bottom, right, RGN_OR), COMPLEXREGION);
                DeleteObject(right);
                assert_ne!(SetWindowRgn(overlay, bottom, 0), 0);
                assert_eq!(window.check_occlusion().is_ok(), edge == 10);
            }
            assert_ne!(DestroyWindow(overlay), 0);
            window.check_occlusion().unwrap();
            assert!(window
                .capture(CaptureMode::Screen)
                .err()
                .unwrap()
                .contains("前台"));

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
}
