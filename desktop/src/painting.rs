//! Parent surface and owner-drawn push buttons. All geometry is in logical pixels.
use crate::{
    console::Jump,
    native::{wide, Id},
};
use std::{cell::RefCell, ptr::null_mut, time::Instant};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    UI::{
        Controls::{DRAWITEMSTRUCT, ODS_DISABLED, ODS_FOCUS, ODS_SELECTED},
        WindowsAndMessaging::*,
    },
};

pub fn rect(x: i32, y: i32, w: i32, h: i32) -> RECT {
    RECT {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    }
}
fn rgb(hex: u32) -> u32 {
    ((hex & 255) << 16) | (hex & 0xff00) | (hex >> 16)
}
pub const AMBER: u32 = 0x1fb3ff;
#[derive(Clone, Copy)]
pub struct Colors {
    pub paper: u32,
    pub panel: u32,
    pub ink: u32,
    pub muted: u32,
    pub rule: u32,
    pub chip: u32,
}
impl Colors {
    pub fn new(dark: bool) -> Self {
        let [paper, panel, ink, muted, rule, chip] = if dark {
            [0x1b1a17, 0x24221e, 0xf3f0e7, 0xc0bdb3, 0x46433a, 0x39362e]
        } else {
            [0xf3f1ea, 0xfffefa, 0x181815, 0x56564e, 0xdcd8cc, 0xe7e3d8]
        };
        Self {
            paper: rgb(paper),
            panel: rgb(panel),
            ink: rgb(ink),
            muted: rgb(muted),
            rule: rgb(rule),
            chip: rgb(chip),
        }
    }
}
#[derive(Clone, Copy)]
pub struct Layout {
    pub scale: f32,
    pub w: i32,
    pub h: i32,
    pub left: i32,
    pub right: i32,
    pub top: i32,
    pub split: i32,
}
impl Layout {
    pub fn new(width: i32, height: i32, dpi: u32, notice: bool) -> Self {
        let scale = (dpi as f32 / 96.)
            .min(width as f32 / 860.)
            .min(height as f32 / 605.)
            .max(0.45);
        let w = (width as f32 / scale).round() as i32;
        let h = (height as f32 / scale).round() as i32;
        let left = ((w - 72) as f32 * 0.58) as i32;
        let top = if notice { 178 } else { 144 };
        let split = (h - 204).max(top + 251);
        Self {
            scale,
            w,
            h,
            left,
            right: 40 + left,
            top,
            split,
        }
    }
    pub fn px(self, v: i32) -> i32 {
        (v as f32 * self.scale).round() as i32
    }
    pub fn bounds(self, id: Id) -> RECT {
        use Id::*;
        let Self {
            w,
            h,
            left,
            right,
            top,
            split,
            ..
        } = self;
        let right_width = w - right - 24;
        let m = top + ((split - top) * 39 / 100).max(126);
        let step = ((split - 48 - m) / 4).max(20);
        let footer = split - 45;
        let body = split + 12;
        let b = match id {
            Theme => rect(w - 140, 9, 116, 34),
            Shortcuts => rect(w - 310, 82, 282, 33),
            Notice => rect(32, 136, w - 64, 29),
            Phase => rect(24 + left - 161, top + 14, 143, 30),
            PhaseNote => rect(44, top + 58, left - 40, 25),
            Window => rect(126, footer, left - 118, 27),
            Countdown => rect(45, footer, 76, 27),
            Score => rect(
                right + 18,
                top + 77,
                right_width - 36,
                (m - top - 82).max(40),
            ),
            Jumps | Hit | Land | Elapsed => {
                let i = match id {
                    Jumps => 0,
                    Hit => 1,
                    Land => 2,
                    _ => 3,
                };
                rect(
                    right + right_width / 2,
                    m + i * step + 1,
                    right_width / 2 - 19,
                    step,
                )
            }
            ActualBackend => rect(right + 84, split - 43, (right_width / 2 - 67).max(52), 23),
            Kernel => rect(
                right + right_width / 2 + 79,
                split - 43,
                right_width / 2 - 80,
                23,
            ),
            Backend => rect(42, body + 73, (left / 3).clamp(128, 186), 180),
            Daemon => rect(
                55 + (left / 3).clamp(128, 186),
                body + 70,
                left - (left / 3).clamp(128, 186) - 50,
                34,
            ),
            Primary | Stop | Check => {
                let available = left - 36;
                let secondary = ((available - 16) / 3).min(146);
                let first = available - 16 - 2 * secondary;
                let (x, width) = match id {
                    Primary => (42, first),
                    Stop => (50 + first, secondary),
                    _ => (58 + first + secondary, secondary),
                };
                rect(x, body + 113, width, 43)
            }
            Ledger | Empty => rect(
                right + 16,
                body + 60,
                right_width - 32,
                (h - body - 85).max(56),
            ),
        };
        RECT {
            left: self.px(b.left),
            top: self.px(b.top),
            right: self.px(b.right),
            bottom: self.px(b.bottom),
        }
    }
}
pub struct Fonts {
    pub body: HFONT,
    pub small: HFONT,
    pub strong: HFONT,
    pub latin: HFONT,
    pub score: HFONT,
    pub title: HFONT,
    pub section: HFONT,
}
impl Fonts {
    pub unsafe fn new(l: Layout) -> Self {
        let f = |face: &str, height, weight| {
            CreateFontW(
                -l.px(height),
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                1,
                0,
                0,
                5,
                0,
                wide(face).as_ptr(),
            )
        };
        Self {
            body: f("Microsoft YaHei UI", 15, 400),
            small: f("Microsoft YaHei UI", 13, 400),
            strong: f("Microsoft YaHei UI", 16, 700),
            latin: f("Saira", 15, 700),
            score: f("Saira", 50, 800),
            title: f("Microsoft YaHei UI", 34, 800),
            section: f("Microsoft YaHei UI", 21, 700),
        }
    }
    pub fn control(&self, id: Id) -> HFONT {
        match id {
            Id::Score => self.score,
            Id::Phase | Id::Primary | Id::Check | Id::Stop => self.strong,
            Id::Jumps | Id::Hit | Id::Land | Id::Elapsed | Id::Countdown => self.latin,
            Id::Shortcuts | Id::Window | Id::Kernel | Id::ActualBackend => self.small,
            _ => self.body,
        }
    }
}
impl Drop for Fonts {
    fn drop(&mut self) {
        unsafe {
            for h in [
                self.body,
                self.small,
                self.strong,
                self.latin,
                self.score,
                self.title,
                self.section,
            ] {
                DeleteObject(h);
            }
        }
    }
}
pub struct Scene {
    pub layout: Layout,
    pub colors: Colors,
    pub fonts: Fonts,
    pub motion: bool,
    pub transition: Instant,
    pub landing: Option<Jump>,
    pub notice: bool,
}
thread_local! {pub static SCENE:RefCell<Option<Scene>>=const{RefCell::new(None)};}
fn animation_frame(
    motion: bool,
    now: Instant,
    transition: Instant,
    landing: Option<Jump>,
) -> (f32, Option<(Jump, f32)>) {
    if !motion {
        return (1., None);
    }
    let entry = (now.saturating_duration_since(transition).as_secs_f32() / 0.4).min(1.);
    (entry, landing.map(|jump| (jump, jump.position(now))))
}
impl Scene {
    pub fn moving(&self) -> bool {
        self.motion
            && (self.transition.elapsed().as_secs_f32() < 0.4
                || self
                    .landing
                    .is_some_and(|jump| jump.started.elapsed() < Jump::DURATION))
    }
    pub fn bg(&self, id: Option<Id>) -> u32 {
        match id {
            Some(Id::Shortcuts) => self.colors.paper,
            Some(Id::Phase | Id::Notice) => AMBER,
            _ => self.colors.panel,
        }
    }
    pub unsafe fn draw(&self, dc: HDC) {
        let l = self.layout;
        let c = self.colors;
        let f = &self.fonts;
        let box_at = |x, y, w, h, color| fill(dc, rect(l.px(x), l.px(y), l.px(w), l.px(h)), color);
        let text = |x, y, w, h, font, color, s: &str| {
            label(
                dc,
                rect(l.px(x), l.px(y), l.px(w), l.px(h)),
                font,
                color,
                s,
                DT_LEFT,
            )
        };
        box_at(0, 0, l.w + 2, l.h + 2, c.paper);
        box_at(0, 0, l.w, 53, rgb(0x191815));
        for (i, hex) in [0xe2a4bc, 0xffb31f, 0xa2c7af].into_iter().enumerate() {
            box_at(i as i32 * l.w / 3, 0, l.w / 3 + 1, 3, rgb(hex));
        }
        box_at(24, 13, 25, 25, AMBER);
        text(31, 11, 18, 28, f.latin, rgb(0x171714), "J");
        text(59, 8, 132, 39, f.latin, rgb(0xfcfaf3), "J U M P B O T");
        box_at(192, 12, 91, 30, AMBER);
        text(206, 12, 77, 30, f.strong, rgb(0x171714), "运行控制");
        text(24, 60, 310, 24, f.latin, c.muted, "//  JUMP CONSOLE");
        text(22, 82, 430, 48, f.title, c.ink, "跳一跳控制台");
        box_at(24, 132, l.w - 48, 1, c.ink);
        let (t, jump) = animation_frame(self.motion, Instant::now(), self.transition, self.landing);
        let amount = 1. - (1. - t).powi(3);
        for (x, y, w, h, en, cn) in [
            (
                24,
                l.top,
                l.left,
                l.split - l.top - 10,
                "//  RUN STATUS",
                "当前状态",
            ),
            (
                l.right,
                l.top,
                l.w - l.right - 24,
                l.split - l.top - 10,
                "//  LIVE METRICS",
                "本轮读数",
            ),
            (
                24,
                l.split + 12,
                l.left,
                l.h - l.split - 36,
                "//  CONTROLS",
                "运行设置",
            ),
            (
                l.right,
                l.split + 12,
                l.w - l.right - 24,
                l.h - l.split - 36,
                "//  ACTIVITY",
                "运行日志",
            ),
        ] {
            box_at(x, y, w, h, c.panel);
            box_at(x, y, w, 1, c.rule);
            box_at(x, y, (68. * amount) as i32, 4, c.ink);
            text(x + 17, y + 8, w - 34, 19, f.latin, c.muted, en);
            text(x + 16, y + 26, w - 34, 30, f.section, c.ink, cn);
        }
        let right_width = l.w - l.right - 24;
        text(
            l.right + 18,
            l.top + 59,
            right_width - 30,
            18,
            f.small,
            c.muted,
            "当前分数",
        );
        for (id, s) in [
            (Id::Jumps, "跳跃次数"),
            (Id::Hit, "中心命中"),
            (Id::Land, "落地率"),
            (Id::Elapsed, "有效运行"),
        ] {
            let mut r = l.bounds(id);
            let y = r.top - 1;
            r.left = l.px(l.right + 18);
            r.right = l.px(l.right + right_width / 2);
            fill(dc, rect(r.left, y, l.px(right_width - 36), 1), c.rule);
            label(dc, r, f.small, c.muted, s, DT_LEFT);
        }
        text(
            l.right + 15,
            l.split - 44,
            64,
            23,
            f.small,
            c.muted,
            "识别后端",
        );
        text(
            l.right + right_width / 2 + 12,
            l.split - 44,
            60,
            23,
            f.small,
            c.muted,
            "背景取样",
        );
        text(42, l.split + 64, 130, 20, f.small, c.muted, "识别后端");
        let foot = l.split - 49;
        box_at(42, foot, l.left - 36, 1, c.rule);
        self.diagram(
            dc,
            rect(42, l.top + 86, l.left - 36, foot - l.top - 94),
            amount,
            jump,
        );
    }
    unsafe fn diagram(&self, dc: HDC, bounds: RECT, entry: f32, jump: Option<(Jump, f32)>) {
        let (x, y, w, h) = (
            bounds.left,
            bounds.top,
            bounds.right - bounds.left,
            bounds.bottom - bounds.top,
        );
        if h < 24 {
            return;
        }
        let l = self.layout;
        let c = self.colors;
        let pt = |a: f32, b: f32| POINT {
            x: ((x as f32 + w as f32 * a) * l.scale).round() as i32,
            y: ((y as f32 + h as f32 * b) * l.scale).round() as i32,
        };
        let line = |a: POINT, b: POINT, color| {
            SetDCPenColor(dc, color);
            MoveToEx(dc, a.x, a.y, null_mut());
            LineTo(dc, b.x, b.y);
        };
        SelectObject(dc, GetStockObject(DC_PEN));
        SelectObject(dc, GetStockObject(DC_BRUSH));
        for i in 0..25 {
            let v = i as f32 / 24.;
            line(pt(v, 0.02), pt(v, 0.97), c.rule);
        }
        for i in 0..8 {
            let v = i as f32 / 8.;
            line(pt(0., v), pt(1., v), c.rule);
        }
        let diamond = |cx: f32, cy: f32, rx: f32, ry: f32, ink, fill| {
            let a = [
                pt(cx - rx, cy),
                pt(cx, cy - ry),
                pt(cx + rx, cy),
                pt(cx, cy + ry),
            ];
            SetDCPenColor(dc, ink);
            SetDCBrushColor(dc, fill);
            Polygon(dc, a.as_ptr(), 4);
        };
        let target = jump.map_or(1., |(jump, _)| jump.to);
        for (side, (cx, cy)) in [(0.24, 0.70), (0.76, 0.51)].into_iter().enumerate() {
            let cy = cy + (1. - entry) * 0.14;
            let sides = [
                pt(cx - 0.155, cy),
                pt(cx, cy + 0.115),
                pt(cx + 0.155, cy),
                pt(cx + 0.155, cy + 0.07),
                pt(cx, cy + 0.185),
                pt(cx - 0.155, cy + 0.07),
            ];
            SetDCPenColor(dc, c.ink);
            SetDCBrushColor(dc, c.chip);
            Polygon(dc, sides.as_ptr(), 6);
            for i in 1..14 {
                let t = i as f32 / 14.;
                line(
                    pt(cx - 0.155 * (1. - t), cy + 0.115 * t + 0.01),
                    pt(cx - 0.155 * (1. - t) + 0.013, cy + 0.115 * t + 0.065),
                    c.rule,
                );
            }
            line(pt(cx, cy + 0.115), pt(cx, cy + 0.185), c.ink);
            diamond(cx, cy, 0.155, 0.115, c.ink, c.paper);
            if side as f32 == target {
                diamond(cx, cy, 0.027, 0.021, c.ink, c.panel);
            }
        }
        let offset = (1. - entry) * 0.14;
        let arc = |t: f32| {
            pt(
                0.24 + 0.52 * t,
                0.70 + offset - 0.19 * t - 1.61 * t * (1. - t),
            )
        };
        let (from, position) = jump.map_or((0., 0.), |(jump, p)| (jump.from, p));
        let color = if jump.is_some() { c.ink } else { c.muted };
        for i in 0..40 {
            let mut start = i as f32 / 40.;
            let mut end = (i + 1) as f32 / 40.;
            if i % 2 != 0 {
                start = start.max(from.min(position));
                end = end.min(from.max(position));
            }
            if start < end {
                line(arc(start), arc(end), color);
            }
        }
        let p = arc(position);
        // At the apex the feet are about 19.7% below the top. Reserve room for the full 3k silhouette.
        let k = (l.px(h) / 16).clamp(1, l.px(15));
        SetDCBrushColor(dc, c.ink);
        SetDCPenColor(dc, c.ink);
        Ellipse(dc, p.x - k / 2, p.y - 3 * k, p.x + k / 2 + 1, p.y - 2 * k);
        let body = [
            POINT {
                x: p.x - k / 3,
                y: p.y - 2 * k,
            },
            POINT {
                x: p.x + k / 3,
                y: p.y - 2 * k,
            },
            POINT { x: p.x + k, y: p.y },
            POINT {
                x: p.x,
                y: p.y + k / 4,
            },
            POINT { x: p.x - k, y: p.y },
        ];
        Polygon(dc, body.as_ptr(), 5);
    }
}
pub unsafe fn fill(dc: HDC, r: RECT, color: u32) {
    SetDCBrushColor(dc, color);
    FillRect(dc, &r, GetStockObject(DC_BRUSH));
}
unsafe fn label(dc: HDC, mut r: RECT, font: HFONT, color: u32, s: &str, align: u32) {
    let old = SelectObject(dc, font);
    SetBkMode(dc, TRANSPARENT as i32);
    SetTextColor(dc, color);
    let s = wide(s);
    DrawTextW(
        dc,
        s.as_ptr(),
        (s.len() - 1) as i32,
        &mut r,
        align | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
    );
    SelectObject(dc, old);
}
pub unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let dc = BeginPaint(hwnd, &mut ps);
    SCENE.with_borrow(|s| {
        if let Some(s) = s {
            let mut r = RECT::default();
            GetClientRect(hwnd, &mut r);
            let mem = CreateCompatibleDC(dc);
            let bmp = CreateCompatibleBitmap(dc, r.right.max(1), r.bottom.max(1));
            if !mem.is_null() && !bmp.is_null() {
                let old = SelectObject(mem, bmp);
                s.draw(mem);
                BitBlt(
                    dc,
                    ps.rcPaint.left,
                    ps.rcPaint.top,
                    ps.rcPaint.right - ps.rcPaint.left,
                    ps.rcPaint.bottom - ps.rcPaint.top,
                    mem,
                    ps.rcPaint.left,
                    ps.rcPaint.top,
                    SRCCOPY,
                );
                SelectObject(mem, old);
            } else {
                s.draw(dc);
            }
            DeleteObject(bmp);
            DeleteDC(mem);
        }
    });
    EndPaint(hwnd, &ps);
}
pub unsafe fn button(d: &DRAWITEMSTRUCT) {
    SCENE.with_borrow(|s| {
        if let Some(s) = s {
            let id = Id::from_raw(d.CtlID as usize);
            let c = s.colors;
            let dc = d.hDC;
            let r = d.rcItem;
            let off = d.itemState & ODS_DISABLED != 0;
            let dark = id == Some(Id::Theme) || id == Some(Id::Primary);
            let pressed = d.itemState & ODS_SELECTED != 0;
            let base = if off {
                c.chip
            } else if pressed {
                AMBER
            } else if dark {
                rgb(0x292824)
            } else {
                c.chip
            };
            fill(
                dc,
                r,
                if id == Some(Id::Theme) {
                    rgb(0x191815)
                } else {
                    c.panel
                },
            );
            let cut = s.layout.px(10);
            let pts = [
                POINT {
                    x: r.left,
                    y: r.top,
                },
                POINT {
                    x: r.right - 1,
                    y: r.top,
                },
                POINT {
                    x: r.right - 1,
                    y: r.bottom - cut,
                },
                POINT {
                    x: r.right - cut,
                    y: r.bottom - 1,
                },
                POINT {
                    x: r.left,
                    y: r.bottom - 1,
                },
            ];
            SelectObject(dc, GetStockObject(DC_PEN));
            SelectObject(dc, GetStockObject(DC_BRUSH));
            SetDCBrushColor(dc, base);
            SetDCPenColor(dc, if off { c.rule } else { c.ink });
            Polygon(dc, pts.as_ptr(), 5);
            if id == Some(Id::Primary) && !off {
                fill(dc, rect(1, 1, s.layout.px(5), r.bottom - 2), AMBER);
            }
            let mut buf = [0u16; 128];
            let n = GetWindowTextW(d.hwndItem, buf.as_mut_ptr(), 128);
            label(
                dc,
                r,
                s.fonts.strong,
                if off {
                    c.muted
                } else if pressed {
                    rgb(0x181815)
                } else if dark {
                    rgb(0xfffefa)
                } else {
                    c.ink
                },
                &String::from_utf16_lossy(&buf[..n as usize]),
                DT_CENTER,
            );
            if d.itemState & ODS_FOCUS != 0 {
                let inset = s.layout.px(5);
                DrawFocusRect(
                    dc,
                    &rect(inset, inset, r.right - 2 * inset, r.bottom - 2 * inset),
                );
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reduced_motion_is_static_and_normal_motion_finishes() {
        let start = Instant::now();
        let jump = Jump {
            started: start,
            from: 0.,
            to: 1.,
        };
        let during = start + std::time::Duration::from_millis(100);
        let frame = animation_frame(true, during, start, Some(jump));
        assert!(frame.0 > 0. && frame.0 < 1. && frame.1.is_some());
        assert_eq!(
            animation_frame(false, during, start, Some(jump)),
            (1., None)
        );
        assert_eq!(
            animation_frame(
                true,
                start + std::time::Duration::from_secs(1),
                start,
                Some(jump)
            ),
            (1., Some((jump, 1.)))
        );
    }
}
