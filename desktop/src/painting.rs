//! Parent surface and owner-drawn push buttons. All geometry is in logical pixels.
use crate::{
    console::{Jump, Pose},
    native::{wide, Id},
};
use std::{
    cell::RefCell,
    ptr::null_mut,
    time::{Duration, Instant},
};
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
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Divider {
    Columns,
    LeftRow,
    RightRow,
}

impl Divider {
    const ALL: [Self; 3] = [Self::Columns, Self::LeftRow, Self::RightRow];

    pub fn axis(self, x: i32, y: i32) -> i32 {
        if self == Self::Columns {
            x
        } else {
            y
        }
    }
}

#[derive(Clone, Copy)]
pub struct Layout {
    pub scale: f32,
    pub w: i32,
    pub h: i32,
    pub top: i32,
    pub ratios: [f32; 3],
}

impl Layout {
    pub fn new(width: i32, height: i32, dpi: u32, notice: bool, ratios: [f32; 3]) -> Self {
        // Keep a full logical canvas even on a small display or with the notice visible.
        let scale = (dpi as f32 / 96.)
            .min(width as f32 / 860.)
            .min(height as f32 / 640.);
        Self {
            scale,
            w: (width as f32 / scale).round() as i32,
            h: (height as f32 / scale).round() as i32,
            top: if notice { 178 } else { 144 },
            ratios,
        }
    }
    // Origin, usable span, minimum and maximum divider center in logical pixels.
    fn track(self, d: Divider) -> (i32, i32, i32, i32) {
        match d {
            Divider::Columns => (32, self.w - 64, 32 + 380, self.w - 32 - 304),
            Divider::LeftRow => (
                self.top,
                self.h - 24 - self.top,
                self.top + 210,
                self.h - 194,
            ),
            Divider::RightRow => (
                self.top,
                self.h - 24 - self.top,
                self.top + 272,
                self.h - 154,
            ),
        }
    }
    pub fn position(self, d: Divider) -> i32 {
        let (start, span, lo, hi) = self.track(d);
        (start + (span as f32 * self.ratios[d as usize]).round() as i32).clamp(lo, hi)
    }
    pub fn move_to(&mut self, d: Divider, pos: i32) -> bool {
        let (start, span, lo, hi) = self.track(d);
        let pos = pos.clamp(lo, hi);
        if pos == self.position(d) {
            return false;
        }
        self.ratios[d as usize] = (pos - start) as f32 / span as f32;
        true
    }
    fn left(self) -> i32 {
        self.position(Divider::Columns) - 32
    }
    fn right(self) -> i32 {
        self.position(Divider::Columns) + 8
    }
    fn band(self, d: Divider) -> RECT {
        let pos = self.position(d);
        match d {
            Divider::Columns => rect(pos - 8, self.top, 16, self.h - 24 - self.top),
            Divider::LeftRow => rect(24, pos - 8, self.left(), 16),
            Divider::RightRow => rect(self.right(), pos - 8, self.w - 24 - self.right(), 16),
        }
    }
    pub fn hit(self, x: i32, y: i32) -> Option<Divider> {
        Divider::ALL.into_iter().find(|&d| {
            let r = self.pixels(self.band(d));
            x >= r.left && x < r.right && y >= r.top && y < r.bottom
        })
    }
    pub fn logical_axis(self, d: Divider, x: i32, y: i32) -> i32 {
        (d.axis(x, y) as f32 / self.scale).round() as i32
    }
    pub fn px(self, v: i32) -> i32 {
        (v as f32 * self.scale).round() as i32
    }
    fn diagram(self) -> RECT {
        let top = self.top + 86;
        let bottom = self.position(Divider::LeftRow) - 49 - 8; // Leave a gap above the footer rule.
        rect(42, top, self.left() - 36, bottom - top)
    }
    pub fn diagram_pixels(self) -> RECT {
        self.pixels(self.diagram())
    }
    fn pixels(self, b: RECT) -> RECT {
        RECT {
            left: self.px(b.left),
            top: self.px(b.top),
            right: self.px(b.right),
            bottom: self.px(b.bottom),
        }
    }
    pub fn bounds(self, id: Id) -> RECT {
        use Id::*;
        let Self { w, h, top, .. } = self;
        let left = self.left();
        let right = self.right();
        let split = self.position(Divider::RightRow);
        let left_split = self.position(Divider::LeftRow);
        let right_width = w - right - 24;
        let m = top + ((split - top) * 39 / 100).max(138);
        let step = ((split - 48 - m) / 4).max(20);
        let footer = left_split - 45;
        let body = left_split + 12;
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
            Ledger | Empty => rect(right + 16, split + 72, right_width - 32, h - split - 97),
        };
        self.pixels(b)
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
    pub frame_time: Instant,
    pub landing: Option<Jump>,
    pub notice: bool,
    pub drag: Option<(Divider, i32)>,
}
thread_local! {pub static SCENE:RefCell<Option<Scene>>=const{RefCell::new(None)};}
pub const ENTRY_DURATION: Duration = Duration::from_millis(400);
fn animation_frame(
    motion: bool,
    now: Instant,
    transition: Instant,
    landing: Option<Jump>,
) -> (f32, Pose) {
    if !motion {
        return (1., landing.map_or_else(Pose::default, Jump::rest));
    }
    let entry = (now.saturating_duration_since(transition).as_secs_f32()
        / ENTRY_DURATION.as_secs_f32())
    .min(1.);
    (
        entry,
        landing.map_or_else(Pose::default, |jump| jump.pose(now)),
    )
}
impl Scene {
    pub fn bg(&self, id: Option<Id>) -> u32 {
        match id {
            Some(Id::Shortcuts) => self.colors.paper,
            Some(Id::Phase | Id::Notice) => AMBER,
            _ => self.colors.panel,
        }
    }
    unsafe fn draw(&self, dc: HDC, dirty: RECT) {
        let l = self.layout;
        let board = l.diagram_pixels();
        let (t, pose) =
            animation_frame(self.motion, self.frame_time, self.transition, self.landing);
        let amount = 1. - (1. - t).powi(3);
        if dirty.left >= board.left
            && dirty.top >= board.top
            && dirty.right <= board.right
            && dirty.bottom <= board.bottom
        {
            fill(dc, dirty, self.colors.panel);
        } else {
            self.draw_panels(dc, amount);
        }
        if IntersectRect(&mut RECT::default(), &board, &dirty) != 0 {
            self.diagram(dc, l.diagram(), amount, pose, self.frame_time);
        }
    }
    unsafe fn draw_panels(&self, dc: HDC, amount: f32) {
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
        for d in Divider::ALL {
            let mut line = l.pixels(l.band(d));
            let center = l.px(l.position(d));
            if d == Divider::Columns {
                line.left = center;
                line.right = center + 1;
            } else {
                line.top = center;
                line.bottom = center + 1;
            }
            fill(dc, line, c.muted);
        }
        let left_split = l.position(Divider::LeftRow);
        let right_split = l.position(Divider::RightRow);
        for (x, y, w, h, en, cn) in [
            (
                24,
                l.top,
                l.left(),
                left_split - l.top - 10,
                "//  RUN STATUS",
                "当前状态",
            ),
            (
                l.right(),
                l.top,
                l.w - l.right() - 24,
                right_split - l.top - 10,
                "//  LIVE METRICS",
                "本轮读数",
            ),
            (
                24,
                left_split + 12,
                l.left(),
                l.h - left_split - 36,
                "//  CONTROLS",
                "运行设置",
            ),
            (
                l.right(),
                right_split + 12,
                l.w - l.right() - 24,
                l.h - right_split - 36,
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
        let right_width = l.w - l.right() - 24;
        text(
            l.right() + 18,
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
            r.left = l.px(l.right() + 18);
            r.right = l.px(l.right() + right_width / 2);
            fill(dc, rect(r.left, y, l.px(right_width - 36), 1), c.rule);
            label(dc, r, f.small, c.muted, s, DT_LEFT);
        }
        text(
            l.right() + 15,
            right_split - 44,
            64,
            23,
            f.small,
            c.muted,
            "识别后端",
        );
        text(
            l.right() + right_width / 2 + 12,
            right_split - 44,
            60,
            23,
            f.small,
            c.muted,
            "背景取样",
        );
        text(42, left_split + 64, 130, 20, f.small, c.muted, "识别后端");
        let foot = left_split - 49;
        box_at(42, foot, l.left() - 36, 1, c.rule);
    }
    unsafe fn diagram(&self, dc: HDC, bounds: RECT, entry: f32, pose: Pose, now: Instant) {
        let w = (bounds.right - bounds.left) as f32;
        let h = (bounds.bottom - bounds.top) as f32;
        if h < 24. {
            return;
        }
        let scale = self.layout.scale;
        let c = self.colors;
        let pt = |x: f32, y: f32| POINT {
            x: ((bounds.left as f32 + x) * scale).round() as i32,
            y: ((bounds.top as f32 + y) * scale).round() as i32,
        };
        let ground = |along: f32| {
            (
                w * (0.23 + 0.54 * along),
                h * (0.72 - 0.17 * along + (1. - entry) * 0.025),
            )
        };
        let feet = |pose: Pose| {
            let (x, y) = ground(pose.along);
            (x, y - pose.lift * h)
        };
        SelectObject(dc, GetStockObject(DC_PEN));
        SelectObject(dc, GetStockObject(DC_BRUSH));
        let face = |points: &[POINT], color| {
            SetDCPenColor(dc, color);
            SetDCBrushColor(dc, color);
            Polygon(dc, points.as_ptr(), points.len() as i32);
        };
        let oval = |a: POINT, b: POINT, color| {
            SetDCPenColor(dc, color);
            SetDCBrushColor(dc, color);
            Ellipse(dc, a.x, a.y, b.x + 1, b.y + 1);
        };

        // Keep the tiles and the pawn in proportion even in a shallow window.
        let tile = (w * 0.14).min(h * 0.40);
        let rim = tile * 0.36;
        let depth = tile * 0.22;
        let target = self.landing.map_or(0., |jump| jump.to);
        for side in [0., 1.] {
            let (x, y) = ground(side);
            let left = pt(x - tile, y);
            let front = pt(x, y + rim);
            let right = pt(x + tile, y);
            let bottom = pt(x, y + rim + depth);
            face(&[left, front, bottom, pt(x - tile, y + depth)], c.rule);
            face(&[front, right, pt(x + tile, y + depth), bottom], c.paper);
            face(&[left, pt(x, y - rim), right, front], c.chip);
            if side == target {
                let mark = tile * 0.13;
                face(
                    &[
                        pt(x - mark, y),
                        pt(x, y - mark * 0.36),
                        pt(x + mark, y),
                        pt(x, y + mark * 0.36),
                    ],
                    AMBER,
                );
            }
        }

        // Only the most recent stretch of actual flight leaves a trace.
        if let Some(jump) = self.landing.filter(|_| self.motion) {
            let age = now.saturating_duration_since(jump.started);
            if age < jump.duration() {
                for (ms, color) in [(48, c.rule), (32, c.rule), (16, c.muted), (0, c.muted)] {
                    let past = if ms == 0 {
                        pose
                    } else {
                        jump.pose(jump.started + age.saturating_sub(Duration::from_millis(ms)))
                    };
                    let (x, y) = feet(past);
                    let p = pt(x, y);
                    if ms == 48 {
                        MoveToEx(dc, p.x, p.y, null_mut());
                    } else {
                        SetDCPenColor(dc, color);
                        LineTo(dc, p.x, p.y);
                    }
                }
            }
        }
        let (x, y) = feet(pose);
        let k = (tile * 0.155).min(h * 0.064).min(15.);
        let width = k * (1. + 0.12 * pose.squash);
        let height = k * (1. - 0.16 * pose.squash);
        let pawn = |a: f32, b: f32| pt(x + a * width, y - b * height);
        // A rounded base and a lightly tapered neck, with the foot anchored on contact.
        BeginPath(dc);
        let top = pawn(-0.32, 1.84);
        MoveToEx(dc, top.x, top.y, null_mut());
        let contour = [
            pawn(-0.32, 2.06),
            pawn(0.32, 2.06),
            pawn(0.32, 1.84),
            pawn(0.33, 1.25),
            pawn(0.65, 0.62),
            pawn(0.77, 0.28),
            pawn(0.92, -0.09),
            pawn(-0.92, -0.09),
            pawn(-0.77, 0.28),
            pawn(-0.65, 0.62),
            pawn(-0.33, 1.25),
            top,
        ];
        PolyBezierTo(dc, contour.as_ptr(), contour.len() as u32);
        CloseFigure(dc);
        EndPath(dc);
        SetDCBrushColor(dc, c.ink);
        FillPath(dc);
        oval(pawn(-0.48, 3.10), pawn(0.48, 2.14), c.ink);
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
unsafe fn paint_region(scene: &Scene, dc: HDC, dirty: RECT) {
    let (w, h) = (dirty.right - dirty.left, dirty.bottom - dirty.top);
    if w <= 0 || h <= 0 {
        return;
    }
    let mem = CreateCompatibleDC(dc);
    let bmp = CreateCompatibleBitmap(dc, w, h);
    let mut copied = false;
    if !mem.is_null() && !bmp.is_null() {
        let old = SelectObject(mem, bmp);
        if !old.is_null() {
            // Both drawing and BitBlt's source use client coordinates in this DC.
            if SetWindowOrgEx(mem, dirty.left, dirty.top, null_mut()) != 0 {
                scene.draw(mem, dirty);
                copied = BitBlt(
                    dc, dirty.left, dirty.top, w, h, mem, dirty.left, dirty.top, SRCCOPY,
                ) != 0;
            }
            SelectObject(mem, old);
        }
    }
    DeleteObject(bmp);
    DeleteDC(mem);
    if !copied {
        scene.draw(dc, dirty);
    }
}

pub unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let dc = BeginPaint(hwnd, &mut ps);
    SCENE.with_borrow(|s| {
        if let Some(s) = s {
            paint_region(s, dc, ps.rcPaint);
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
        let jump = Jump::next(None, start);
        let during = start + std::time::Duration::from_millis(100);
        let frame = animation_frame(true, during, start, Some(jump));
        assert!(frame.0 > 0. && frame.0 < 1. && frame.1.lift > 0.);
        assert_eq!(
            animation_frame(false, during, start, Some(jump)),
            (1., jump.rest())
        );
        assert_eq!(
            animation_frame(
                true,
                start + std::time::Duration::from_secs(1),
                start,
                Some(jump)
            ),
            (1., jump.rest())
        );
    }
}
