//! Parent surface and owner-drawn controls. All geometry is in logical pixels.
use crate::{
    console::{Jump, Pose},
    motion::{rubber_band, spring_curve, Roller, Slot, Spring},
    native::{wide, Id},
};
use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
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
pub fn lerp_color(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |shift: u32| -> u32 {
        let ca = ((a >> shift) & 0xff) as f32;
        let cb = ((b >> shift) & 0xff) as f32;
        (ca + (cb - ca) * t).round().clamp(0.0, 255.0) as u32
    };
    mix(0) | (mix(8) << 8) | (mix(16) << 16)
}
pub const AMBER: u32 = 0x1fb3ff;
pub const ALERT: u32 = 0x475ae0; // rgb(0xe05a47)
pub const DEFAULT_RATIOS: [f32; 3] = [0.58, 0.66, 0.66];
#[derive(Clone, Copy)]
pub struct Colors {
    pub paper: u32,
    pub panel: u32,
    pub ink: u32,
    pub muted: u32,
    pub rule: u32,
    pub chip: u32,
    pub header: u32,
    pub track: u32,
}
impl Colors {
    pub fn new(dark: bool) -> Self {
        let [paper, panel, ink, muted, rule, chip, header, track] = if dark {
            [
                0x1b1a17, 0x24221e, 0xf3f0e7, 0xc0bdb3, 0x46433a, 0x39362e, 0x191815, 0x292824,
            ]
        } else {
            [
                0xf3f1ea, 0xfffefa, 0x181815, 0x56564e, 0xdcd8cc, 0xe7e3d8, 0xfffefa, 0xe7e3d8,
            ]
        };
        Self {
            paper: rgb(paper),
            panel: rgb(panel),
            ink: rgb(ink),
            muted: rgb(muted),
            rule: rgb(rule),
            chip: rgb(chip),
            header: rgb(header),
            track: rgb(track),
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
    pub stretch: [i32; 3],
}

impl Layout {
    pub fn new(
        width: i32,
        height: i32,
        dpi: u32,
        top: i32,
        ratios: [f32; 3],
        stretch: [i32; 3],
    ) -> Self {
        // Keep a full logical canvas even on a small display or with the notice visible.
        let scale = (dpi as f32 / 96.)
            .min(width as f32 / 860.)
            .min(height as f32 / 640.);
        Self {
            scale,
            w: (width as f32 / scale).round() as i32,
            h: (height as f32 / scale).round() as i32,
            top,
            ratios,
            stretch,
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
    fn base_position(self, d: Divider) -> i32 {
        let (start, span, lo, hi) = self.track(d);
        (start + (span as f32 * self.ratios[d as usize]).round() as i32).clamp(lo, hi)
    }
    pub fn position(self, d: Divider) -> i32 {
        self.base_position(d)
    }
    pub fn move_to(&mut self, d: Divider, pos: i32, rubber: bool) -> bool {
        let (start, span, lo, hi) = self.track(d);
        let clamped = pos.clamp(lo, hi);
        let next_stretch = if rubber {
            rubber_band((pos - clamped) as f32, 5.0)
                .round()
                .clamp(-5.0, 5.0) as i32
        } else {
            0
        };
        let old_pos = self.position(d);
        let old_stretch = self.stretch[d as usize];
        self.ratios[d as usize] = (clamped - start) as f32 / span as f32;
        self.stretch[d as usize] = next_stretch;
        self.position(d) != old_pos || self.stretch[d as usize] != old_stretch
    }
    fn left(self) -> i32 {
        self.position(Divider::Columns) - 32
    }
    fn right(self) -> i32 {
        self.position(Divider::Columns) + 8
    }
    pub fn band(self, d: Divider) -> RECT {
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
    pub fn entry_bar_pixels(self) -> [RECT; 3] {
        let left_split = self.position(Divider::LeftRow);
        [
            self.pixels(rect(24, self.top, 80, 5)),
            self.pixels(rect(self.right(), self.top, 80, 5)),
            self.pixels(rect(24, left_split + 12, 80, 5)),
        ]
    }
    pub fn pixels(self, b: RECT) -> RECT {
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
            ActualBackend => rect(right + 84, split - 43, (right_width / 2 - 74).max(36), 23),
            Kernel => rect(
                right + right_width / 2 + 79,
                split - 43,
                right_width / 2 - 80,
                23,
            ),
            Backend => rect(42, body + 70, (left / 3).clamp(128, 186), 180),
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
            LogCopy => rect(w - 24 - 134, split + 19, 44, 26),
            LogSmaller | LogLarger => {
                let inset = if id == LogSmaller { 84 } else { 48 };
                rect(w - 24 - inset, split + 19, 32, 26)
            }
            Ledger | Empty => rect(right + 16, split + 57, right_width - 32, h - split - 85),
        };
        self.pixels(b)
    }
}
pub struct Fonts {
    pub body: HFONT,
    pub log: HFONT,
    pub small: HFONT,
    pub strong: HFONT,
    pub latin: HFONT,
    pub score: HFONT,
    pub title: HFONT,
    pub section: HFONT,
}
impl Fonts {
    unsafe fn make(l: Layout, face: &str, height: i32, weight: i32) -> HFONT {
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
    }
    pub unsafe fn log(l: Layout, size: i32) -> HFONT {
        Self::make(l, "Microsoft YaHei UI", size, 400)
    }
    pub unsafe fn new(l: Layout, log_size: i32) -> Self {
        let f = |face, height, weight| Self::make(l, face, height, weight);
        Self {
            body: f("Microsoft YaHei UI", 15, 400),
            log: Self::log(l, log_size),
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
            Id::Ledger | Id::Empty => self.log,
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
                self.log,
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
#[derive(Clone, Copy, Debug)]
pub struct ButtonAnim {
    pub hover: Spring,
    pub press: Spring,
}

impl Default for ButtonAnim {
    fn default() -> Self {
        Self {
            hover: Spring::hover(0.0),
            press: Spring::press(0.0),
        }
    }
}

impl ButtonAnim {
    pub fn active(&self) -> bool {
        !self.hover.settled() || !self.press.settled()
    }
    pub fn step(&mut self, dt: f32) {
        self.hover.step(dt);
        self.press.step(dt);
    }
    pub fn snap(&mut self) {
        self.hover.snap(self.hover.target);
        self.press.snap(self.press.target);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhaseTone {
    Idle,
    Active,
    Warm,
    Alert,
}

impl PhaseTone {
    pub fn for_phase(phase: &str, has_notice: bool) -> Self {
        match phase {
            "playing" | "landing" | "acquiring" | "selftest" | "starting" => Self::Active,
            "waiting_window" | "countdown" | "resuming" | "paused" | "pausing" => Self::Warm,
            "capture_blocked" | "unrecognized_dark" | "blocked_modal" | "dead" => Self::Alert,
            "stopped" if has_notice => Self::Alert,
            _ => Self::Idle,
        }
    }
    pub fn colors(self, c: Colors) -> (u32, u32, u32, u32) {
        // (bg, border, left_bar, text)
        match self {
            Self::Idle => (c.chip, c.rule, c.ink, c.ink),
            Self::Active => (AMBER, AMBER, AMBER, rgb(0x171714)),
            Self::Warm => (lerp_color(c.chip, AMBER, 0.30), AMBER, AMBER, c.ink),
            Self::Alert => (ALERT, ALERT, ALERT, rgb(0xfffefa)),
        }
    }
}

pub struct Scene {
    pub layout: Layout,
    pub colors: Colors,
    pub fonts: Fonts,
    pub log_size: i32,
    pub motion: bool,
    pub transition: Instant,
    pub frame_time: Instant,
    pub landing: Option<Jump>,
    pub notice: bool,
    pub drag: Option<(Divider, i32)>,
    pub hovered_divider: Option<Divider>,
    pub divider_hover: [Spring; 3],
    pub rollers: HashMap<Id, Roller>,
    pub buttons: HashMap<Id, ButtonAnim>,
    pub theme_thumb: Spring,
    pub charging: Spring,
    pub daemon_check: Spring,
    pub phase_tone: PhaseTone,
    pub phase_prev_colors: (u32, u32, u32, u32),
    pub phase_target_colors: (u32, u32, u32, u32),
    pub phase_blend: Spring,
    pub phase_slide: Spring,
    pub countdown_ratio: Option<f32>,
    pub log_items: VecDeque<(u64, Instant, String)>,
}
thread_local! {pub static SCENE:RefCell<Option<Scene>>=const{RefCell::new(None)};}
pub const ENTRY_DURATION: Duration = Duration::from_millis(540);
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
            Some(Id::Theme) => self.colors.header,
            Some(Id::Phase | Id::Notice) => AMBER,
            _ => self.colors.panel,
        }
    }
    pub fn current_phase_colors(&self) -> (u32, u32, u32, u32) {
        let t = self.phase_blend.value.clamp(0.0, 1.0);
        let (b0, r0, l0, t0) = self.phase_prev_colors;
        let (b1, r1, l1, t1) = self.phase_target_colors;
        (
            lerp_color(b0, b1, t),
            lerp_color(r0, r1, t),
            lerp_color(l0, l1, t),
            lerp_color(t0, t1, t),
        )
    }
    pub fn theme_thumb_center(&self) -> POINT {
        let r = self.layout.bounds(Id::Theme);
        let pad = self.layout.px(3);
        let inner_w = (r.right - r.left) - 2 * pad;
        let cx0 = r.left + pad + inner_w / 4;
        let cx1 = r.left + pad + 3 * inner_w / 4;
        let cx = cx0 as f32 + (cx1 - cx0) as f32 * self.theme_thumb.value;
        POINT {
            x: cx.round() as i32,
            y: (r.top + r.bottom) / 2,
        }
    }
    unsafe fn draw(&self, dc: HDC, dirty: RECT) {
        let l = self.layout;
        let board = l.diagram_pixels();
        let (t, pose) =
            animation_frame(self.motion, self.frame_time, self.transition, self.landing);
        let entry_s = t * ENTRY_DURATION.as_secs_f32();
        let amount = if self.motion {
            spring_curve(entry_s, 0.36, 0.80)
        } else {
            1.
        };
        if dirty.left >= board.left
            && dirty.top >= board.top
            && dirty.right <= board.right
            && dirty.bottom <= board.bottom
        {
            fill(dc, dirty, self.colors.panel);
        } else {
            self.draw_panels(dc, dirty, entry_s);
        }
        if IntersectRect(&mut RECT::default(), &board, &dirty) != 0 {
            self.draw_diagram_aa(dc, l.diagram(), amount, pose, self.frame_time);
        }
    }
    unsafe fn draw_panels(&self, dc: HDC, dirty: RECT, entry_s: f32) {
        let l = self.layout;
        let c = self.colors;
        let f = &self.fonts;
        let hits = |r: RECT| IntersectRect(&mut RECT::default(), &r, &dirty) != 0;
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
        fill(dc, dirty, c.paper);
        if hits(l.pixels(rect(0, 0, l.w + 2, 136))) {
            box_at(0, 0, l.w, 53, c.header);
            box_at(0, 52, l.w, 1, c.rule);
            for (i, hex) in [0xe2a4bc, 0xffb31f, 0xa2c7af].into_iter().enumerate() {
                box_at(i as i32 * l.w / 3, 0, l.w / 3 + 1, 3, rgb(hex));
            }
            box_at(24, 13, 25, 25, AMBER);
            label(
                dc,
                l.pixels(rect(24, 13, 25, 25)),
                f.latin,
                rgb(0x171714),
                "J",
                DT_CENTER,
            );
            text(59, 8, 132, 39, f.latin, c.ink, "J U M P B O T");
            box_at(192, 12, 91, 30, AMBER);
            label(
                dc,
                l.pixels(rect(192, 12, 91, 30)),
                f.strong,
                rgb(0x171714),
                "运行控制",
                DT_CENTER,
            );
            text(24, 60, 310, 24, f.latin, c.muted, "//  JUMP CONSOLE");
            text(22, 82, 430, 48, f.title, c.ink, "跳一跳控制台");
            box_at(24, 132, l.w - 48, 1, c.ink);
        }
        let active_drag = self.drag.map(|(d, _)| d);
        for d in Divider::ALL {
            let band_px = l.pixels(l.band(d));
            if !hits(band_px) {
                continue;
            }
            let mut line = band_px;
            let stretch = l.stretch[d as usize];
            let center = l.px(l.position(d) + stretch);
            let hover_t = self.divider_hover[d as usize].value.clamp(0.0, 1.0);
            let active = active_drag == Some(d) || stretch != 0;
            let t = if active { 1.0 } else { hover_t };
            let thickness = if stretch != 0 { 2 } else { 1 };
            if d == Divider::Columns {
                line.left = center;
                line.right = center + thickness;
            } else {
                line.top = center;
                line.bottom = center + thickness;
            }
            fill(dc, line, lerp_color(c.rule, AMBER, t));
            if t > 0.02 {
                let half_len = (l.px(14) as f32 * (0.55 + 0.45 * t)).round() as i32;
                let pill_thick = l.px(3).max(2);
                let pill = if d == Divider::Columns {
                    let mid_y = (band_px.top + band_px.bottom) / 2;
                    RECT {
                        left: center - pill_thick / 2,
                        top: mid_y - half_len,
                        right: center + (pill_thick + 1) / 2 + 1,
                        bottom: mid_y + half_len,
                    }
                } else {
                    let mid_x = (band_px.left + band_px.right) / 2;
                    RECT {
                        left: mid_x - half_len,
                        top: center - pill_thick / 2,
                        right: mid_x + half_len,
                        bottom: center + (pill_thick + 1) / 2 + 1,
                    }
                };
                fill(dc, pill, lerp_color(c.muted, AMBER, t));
            }
        }
        let left_split = l.position(Divider::LeftRow);
        let right_split = l.position(Divider::RightRow);
        let right_width = l.w - l.right() - 24;
        for (idx, (x, y, w, h, en, cn)) in [
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
                right_width,
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
                right_width,
                l.h - right_split - 36,
                "//  ACTIVITY",
                "运行日志",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            if !hits(l.pixels(rect(x, y, w, h))) {
                continue;
            }
            box_at(x, y, w, h, c.panel);
            box_at(x, y, w, 1, c.rule);
            if idx == 3 {
                text(x + 16, y + 6, 120, 29, f.strong, c.ink, cn);
                text(
                    x + 96,
                    y + 8,
                    (w - 238).max(20),
                    25,
                    f.small,
                    c.muted,
                    "ACTIVITY",
                );
                box_at(x + 16, y + 39, w - 32, 1, c.rule);
                continue;
            }
            let bar_amount = if self.motion {
                spring_curve((entry_s - idx as f32 * 0.045).max(0.0), 0.34, 0.84).clamp(0.0, 1.0)
            } else {
                1.0
            };
            box_at(x, y, (68. * bar_amount).round() as i32, 4, c.ink);
            text(x + 17, y + 8, w - 34, 19, f.latin, c.muted, en);
            text(x + 16, y + 26, w - 34, 30, f.section, c.ink, cn);
            match idx {
                0 => {
                    let foot = left_split - 49;
                    box_at(42, foot, l.left() - 36, 1, c.rule);
                }
                1 => {
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
                        let ry = r.top - 1;
                        r.left = l.px(l.right() + 18);
                        r.right = l.px(l.right() + right_width / 2);
                        fill(dc, rect(r.left, ry, l.px(right_width - 36), 1), c.rule);
                        label(dc, r, f.small, c.muted, s, DT_LEFT);
                    }
                    box_at(l.right() + 18, right_split - 49, right_width - 36, 1, c.rule);
                    text(
                        l.right() + 18,
                        right_split - 43,
                        64,
                        23,
                        f.small,
                        c.muted,
                        "识别后端",
                    );
                    text(
                        l.right() + right_width / 2 + 12,
                        right_split - 43,
                        60,
                        23,
                        f.small,
                        c.muted,
                        "背景取样",
                    );
                }
                2 => {
                    text(42, left_split + 66, 130, 16, f.small, c.muted, "识别后端");
                }
                _ => {}
            }
        }
    }
    unsafe fn draw_diagram_aa(
        &self,
        dc: HDC,
        bounds: RECT,
        entry: f32,
        pose: Pose,
        now: Instant,
    ) {
        let clip = self.layout.pixels(bounds);
        let px_w = clip.right - clip.left;
        let px_h = clip.bottom - clip.top;
        if px_w <= 0 || px_h <= 0 {
            return;
        }
        if px_w <= 1600 && px_h <= 1200 {
            let ripple_active = self.motion
                && self.landing.is_some_and(|j| {
                    now.saturating_duration_since(j.started)
                        < j.flight_duration() + Duration::from_millis(440)
                });
            let charge_active = self.charging.value > 0.02;
            let cache_key = (!ripple_active && !charge_active).then_some(DiagramCacheKey {
                w: px_w,
                h: px_h,
                scale: self.layout.scale.to_bits(),
                panel: self.colors.panel,
                ink: self.colors.ink,
                entry: entry.to_bits(),
                along: pose.along.to_bits(),
                lift: pose.lift.to_bits(),
                squash: pose.squash.to_bits(),
                camera: pose.camera.to_bits(),
                target_tile: pose.target_tile,
            });
            let mut copied = false;
            DIAGRAM_BUF.with(|cell| {
                if let Ok(mut buf) = cell.try_borrow_mut() {
                    if let Some((ss_dc, out_dc)) = buf.ensure(dc, px_w, px_h) {
                        let hit = cache_key.is_some() && buf.cache == cache_key;
                        if !hit {
                            fill(ss_dc, rect(0, 0, px_w * 2, px_h * 2), self.colors.panel);
                            let local =
                                rect(0, 0, bounds.right - bounds.left, bounds.bottom - bounds.top);
                            self.diagram_scaled(
                                ss_dc,
                                local,
                                self.layout.scale * 2.0,
                                entry,
                                pose,
                                now,
                            );
                            buf.downsample_2x(px_w, px_h);
                            buf.cache = cache_key;
                        }
                        copied =
                            BitBlt(dc, clip.left, clip.top, px_w, px_h, out_dc, 0, 0, SRCCOPY) != 0;
                    }
                }
            });
            if copied {
                return;
            }
        }
        let saved_clip = SaveDC(dc);
        IntersectClipRect(dc, clip.left, clip.top, clip.right, clip.bottom);
        self.diagram_scaled(dc, bounds, self.layout.scale, entry, pose, now);
        RestoreDC(dc, saved_clip);
    }
    unsafe fn diagram_scaled(
        &self,
        dc: HDC,
        bounds: RECT,
        scale: f32,
        entry: f32,
        pose: Pose,
        now: Instant,
    ) {
        let w = (bounds.right - bounds.left) as f32;
        let h = (bounds.bottom - bounds.top) as f32;
        if h < 24. {
            return;
        }
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
        let stroke_diamond = |cx: f32, cy: f32, r: f32, color: u32| {
            let pts = [
                pt(cx - r, cy),
                pt(cx, cy - r * 0.36),
                pt(cx + r, cy),
                pt(cx, cy + r * 0.36),
            ];
            SelectObject(dc, GetStockObject(HOLLOW_BRUSH));
            SetDCPenColor(dc, color);
            Polygon(dc, pts.as_ptr(), 4);
            SelectObject(dc, GetStockObject(DC_BRUSH));
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
        let cam = pose.camera;
        let base = cam.floor() as i32;
        for k in (base - 1..=base + 2).rev() {
            let side = k as f32 - cam;
            if !(-0.95..=2.2).contains(&side) {
                continue;
            }
            let fade = if side < 0.0 {
                (-side * 1.15).clamp(0.0, 1.0)
            } else if side > 1.0 {
                ((side - 1.45) / 0.65).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let drop_y = if k == pose.target_tile && side > 1.0 && self.motion {
                let enter_p = (2.0 - side).clamp(0.0, 1.0);
                let settle = spring_curve(enter_p * 0.44, 0.32, 0.70);
                -(1.0 - settle) * h * 0.14
            } else {
                0.0
            };
            let (x, base_y) = ground(side);
            let y = base_y + drop_y;
            let left = pt(x - tile, y);
            let front = pt(x, y + rim);
            let right = pt(x + tile, y);
            let bottom = pt(x, y + rim + depth);
            let col_rule = lerp_color(c.rule, c.panel, fade);
            let col_paper = lerp_color(c.paper, c.panel, fade);
            let col_chip = lerp_color(c.chip, c.panel, fade);
            face(&[left, front, bottom, pt(x - tile, y + depth)], col_rule);
            face(&[front, right, pt(x + tile, y + depth), bottom], col_paper);
            face(&[left, pt(x, y - rim), right, front], col_chip);
            if k == pose.target_tile {
                let mark = tile * 0.13;
                let col_mark = lerp_color(AMBER, c.panel, fade);
                face(
                    &[
                        pt(x - mark, y),
                        pt(x, y - mark * 0.36),
                        pt(x + mark, y),
                        pt(x, y + mark * 0.36),
                    ],
                    col_mark,
                );
            }
        }

        let k_size = (tile * 0.155).min(h * 0.064).min(15.);

        // Landing ripple & charging ring on the tile surface.
        let mut mid_flight = false;
        if let Some(jump) = self.landing.filter(|_| self.motion) {
            let age = now.saturating_duration_since(jump.started);
            let flight = jump.flight_duration();
            if age < flight {
                mid_flight = true;
                for (ms, color) in [(48, c.rule), (32, c.rule), (16, c.muted), (0, c.muted)] {
                    let past = if ms == 0 {
                        pose
                    } else {
                        jump.pose(jump.started + age.saturating_sub(Duration::from_millis(ms)))
                    };
                    let past_along = past.camera + past.along - pose.camera;
                    let (gx, gy) = ground(past_along);
                    let p = pt(gx, gy - past.lift * h);
                    if ms == 48 {
                        MoveToEx(dc, p.x, p.y, null_mut());
                    } else {
                        SetDCPenColor(dc, color);
                        LineTo(dc, p.x, p.y);
                    }
                }
            } else {
                let post_s = (age - flight).as_secs_f32();
                if post_s < 0.42 {
                    let rp = (post_s / 0.42).clamp(0.0, 1.0);
                    let ease = 1.0 - (1.0 - rp).powi(3);
                    let (lx, ly) = ground(jump.to as f32 - cam);
                    let base_col = if jump.bullseye { AMBER } else { c.muted };
                    stroke_diamond(
                        lx,
                        ly,
                        tile * (0.16 + 0.72 * ease),
                        lerp_color(base_col, c.chip, rp * rp),
                    );
                    if jump.bullseye {
                        stroke_diamond(
                            lx,
                            ly,
                            tile * (0.10 + 0.46 * ease),
                            lerp_color(AMBER, c.chip, rp),
                        );
                    }
                }
            }
        }

        let charge_t = if !mid_flight {
            self.charging.value.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (gx, gy) = ground(pose.along);
        if charge_t > 0.02 {
            let phase_s = now.saturating_duration_since(self.transition).as_secs_f32();
            let pulse = 0.5 + 0.5 * (phase_s * 5.0).sin();
            let ring_r = tile * (0.26 + 0.10 * charge_t + 0.04 * pulse * charge_t);
            stroke_diamond(
                gx,
                gy,
                ring_r,
                lerp_color(c.chip, AMBER, 0.72 * charge_t),
            );
        }

        // Ground shadow beneath the pawn.
        let shadow_fade = (pose.lift / 0.35).clamp(0.0, 0.85);
        let eff_squash = pose.squash + 0.08 * charge_t;
        let sr = k_size * 0.96 * (1.0 - 0.38 * shadow_fade) * (1.0 + 0.08 * eff_squash);
        let shadow_bg = if pose.along.abs() < 0.35 || (pose.along - 1.0).abs() < 0.35 {
            c.chip
        } else {
            c.panel
        };
        oval(
            pt(gx - sr, gy - sr * 0.34),
            pt(gx + sr, gy + sr * 0.34),
            lerp_color(c.muted, shadow_bg, 0.50 + 0.48 * shadow_fade),
        );

        let (x, y) = feet(pose);
        let width = k_size * (1. + 0.12 * eff_squash);
        let height = k_size * (1. - 0.16 * eff_squash);
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
struct BackBuffer {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    w: i32,
    h: i32,
}

impl BackBuffer {
    const fn empty() -> Self {
        Self {
            dc: null_mut(),
            bmp: null_mut(),
            old: null_mut(),
            w: 0,
            h: 0,
        }
    }
    unsafe fn release(&mut self) {
        if !self.dc.is_null() {
            if !self.old.is_null() {
                SelectObject(self.dc, self.old);
            }
            DeleteDC(self.dc);
        }
        if !self.bmp.is_null() {
            DeleteObject(self.bmp);
        }
        *self = Self::empty();
    }
    unsafe fn ensure(&mut self, ref_dc: HDC, w: i32, h: i32) -> Option<HDC> {
        if self.dc.is_null() || w > self.w || h > self.h {
            let nw = w.max(self.w);
            let nh = h.max(self.h);
            self.release();
            let dc = CreateCompatibleDC(ref_dc);
            let bmp = CreateCompatibleBitmap(ref_dc, nw, nh);
            if dc.is_null() || bmp.is_null() {
                if !bmp.is_null() {
                    DeleteObject(bmp);
                }
                if !dc.is_null() {
                    DeleteDC(dc);
                }
                return None;
            }
            let old = SelectObject(dc, bmp);
            if old.is_null() {
                DeleteObject(bmp);
                DeleteDC(dc);
                return None;
            }
            self.dc = dc;
            self.bmp = bmp;
            self.old = old;
            self.w = nw;
            self.h = nh;
        }
        Some(self.dc)
    }
}

struct DibSurface {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    w: i32,
    h: i32,
}

impl DibSurface {
    const fn empty() -> Self {
        Self {
            dc: null_mut(),
            bmp: null_mut(),
            old: null_mut(),
            bits: null_mut(),
            w: 0,
            h: 0,
        }
    }
    unsafe fn release(&mut self) {
        if !self.dc.is_null() {
            if !self.old.is_null() {
                SelectObject(self.dc, self.old);
            }
            DeleteDC(self.dc);
        }
        if !self.bmp.is_null() {
            DeleteObject(self.bmp);
        }
        *self = Self::empty();
    }
    unsafe fn ensure(&mut self, ref_dc: HDC, w: i32, h: i32) -> bool {
        if self.dc.is_null() || w > self.w || h > self.h {
            let nw = w.max(self.w);
            let nh = h.max(self.h);
            self.release();
            let dc = CreateCompatibleDC(ref_dc);
            if dc.is_null() {
                return false;
            }
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: nw,
                    biHeight: -nh,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut std::ffi::c_void = null_mut();
            let bmp = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if bmp.is_null() || bits.is_null() {
                if !bmp.is_null() {
                    DeleteObject(bmp);
                }
                DeleteDC(dc);
                return false;
            }
            let old = SelectObject(dc, bmp);
            if old.is_null() {
                DeleteObject(bmp);
                DeleteDC(dc);
                return false;
            }
            self.dc = dc;
            self.bmp = bmp;
            self.old = old;
            self.bits = bits.cast();
            self.w = nw;
            self.h = nh;
        }
        true
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct DiagramCacheKey {
    w: i32,
    h: i32,
    scale: u32,
    panel: u32,
    ink: u32,
    entry: u32,
    along: u32,
    lift: u32,
    squash: u32,
    camera: u32,
    target_tile: i32,
}

struct DiagramBuffer {
    ss: DibSurface,
    out: DibSurface,
    cache: Option<DiagramCacheKey>,
}

impl DiagramBuffer {
    const fn empty() -> Self {
        Self {
            ss: DibSurface::empty(),
            out: DibSurface::empty(),
            cache: None,
        }
    }
    unsafe fn release(&mut self) {
        self.ss.release();
        self.out.release();
        self.cache = None;
    }
    unsafe fn ensure(&mut self, ref_dc: HDC, w: i32, h: i32) -> Option<(HDC, HDC)> {
        let prev_ss = (self.ss.w, self.ss.h);
        let prev_out = (self.out.w, self.out.h);
        if self.ss.ensure(ref_dc, w * 2, h * 2) && self.out.ensure(ref_dc, w, h) {
            if (self.ss.w, self.ss.h) != prev_ss || (self.out.w, self.out.h) != prev_out {
                self.cache = None;
            }
            Some((self.ss.dc, self.out.dc))
        } else {
            self.cache = None;
            None
        }
    }
    unsafe fn downsample_2x(&self, w: i32, h: i32) {
        let ss_stride = self.ss.w as usize;
        let out_stride = self.out.w as usize;
        let w = w as usize;
        let h = h as usize;
        GdiFlush();
        for y in 0..h {
            let r0 = std::slice::from_raw_parts(self.ss.bits.add((2 * y) * ss_stride), w * 2);
            let r1 = std::slice::from_raw_parts(self.ss.bits.add((2 * y + 1) * ss_stride), w * 2);
            let dst = std::slice::from_raw_parts_mut(self.out.bits.add(y * out_stride), w);
            for (x, out) in dst.iter_mut().enumerate() {
                let x2 = 2 * x;
                let p00 = r0[x2];
                let p01 = r0[x2 + 1];
                let p10 = r1[x2];
                let p11 = r1[x2 + 1];
                let rb = ((p00 & 0x00ff_00ff)
                    + (p01 & 0x00ff_00ff)
                    + (p10 & 0x00ff_00ff)
                    + (p11 & 0x00ff_00ff)
                    + 0x0002_0002)
                    >> 2;
                let g = (((p00 >> 8) & 0x0000_00ff)
                    + ((p01 >> 8) & 0x0000_00ff)
                    + ((p10 >> 8) & 0x0000_00ff)
                    + ((p11 >> 8) & 0x0000_00ff)
                    + 2)
                    >> 2;
                *out = (rb & 0x00ff_00ff) | ((g & 0x0000_00ff) << 8);
            }
        }
    }
}

thread_local! {
    static BACKBUF: RefCell<BackBuffer> = const { RefCell::new(BackBuffer::empty()) };
    static DIAGRAM_BUF: RefCell<DiagramBuffer> = const { RefCell::new(DiagramBuffer::empty()) };
}

pub unsafe fn release_buffers() {
    BACKBUF.with_borrow_mut(|buf| buf.release());
    DIAGRAM_BUF.with_borrow_mut(|buf| buf.release());
}

unsafe fn with_backbuffer(dc: HDC, r: RECT, mut draw: impl FnMut(HDC)) {
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    if w <= 0 || h <= 0 {
        return;
    }
    let mut copied = false;
    BACKBUF.with(|cell| {
        if let Ok(mut buf) = cell.try_borrow_mut() {
            if let Some(mem) = buf.ensure(dc, w, h) {
                let saved = SaveDC(mem);
                if SetWindowOrgEx(mem, r.left, r.top, null_mut()) != 0 {
                    draw(mem);
                    copied = BitBlt(dc, r.left, r.top, w, h, mem, r.left, r.top, SRCCOPY) != 0;
                }
                RestoreDC(mem, saved);
            }
        }
    });
    if !copied {
        draw(dc);
    }
}

unsafe fn paint_region(scene: &Scene, dc: HDC, dirty: RECT) {
    with_backbuffer(dc, dirty, |target| scene.draw(target, dirty));
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
unsafe fn chamfered_polygon(dc: HDC, r: RECT, cut: i32, fill_color: u32, stroke_color: u32) {
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
    SetDCBrushColor(dc, fill_color);
    SetDCPenColor(dc, stroke_color);
    Polygon(dc, pts.as_ptr(), 5);
}

unsafe fn draw_theme_slider(
    dc: HDC,
    r: RECT,
    s: &Scene,
    off: bool,
    focus: bool,
    hover: f32,
    press: f32,
) {
    let c = s.colors;
    fill(dc, r, c.header);
    let cut = s.layout.px(10);
    let border = if off {
        c.rule
    } else {
        lerp_color(c.ink, AMBER, 0.55 * hover)
    };
    chamfered_polygon(dc, r, cut, c.track, border);

    let pad = s.layout.px(3);
    let inner_w = (r.right - r.left) - 2 * pad;
    let inner_h = (r.bottom - r.top) - 2 * pad;
    if inner_w <= 4 || inner_h <= 4 {
        return;
    }

    let base_tw = inner_w / 2;
    let cx0 = r.left + pad + inner_w / 4;
    let cx1 = r.left + pad + 3 * inner_w / 4;
    let stretch = 1.0 + (s.theme_thumb.velocity.abs() * 0.06).min(0.22);
    let tw = ((base_tw as f32) * stretch)
        .round()
        .clamp(4.0, inner_w as f32) as i32;
    let cx = cx0 as f32 + (cx1 - cx0) as f32 * s.theme_thumb.value;
    let min_x = r.left + pad;
    let max_x = r.right - pad;
    let mut tl = (cx - tw as f32 * 0.5).round() as i32;
    let mut tr = tl + tw;
    if tl < min_x {
        let shift = min_x - tl;
        tl += shift;
        tr = (tr + shift).min(max_x);
    }
    if tr > max_x {
        let shift = tr - max_x;
        tr -= shift;
        tl = (tl - shift).max(min_x);
    }
    let v_inset = (press * s.layout.px(1) as f32).round() as i32;
    let thumb_rect = RECT {
        left: tl,
        top: r.top + pad + v_inset,
        right: tr,
        bottom: r.bottom - pad - v_inset,
    };
    let thumb_cut = s
        .layout
        .px(6)
        .min((tr - tl) / 3)
        .min((thumb_rect.bottom - thumb_rect.top).max(3) / 3)
        .max(1);
    let thumb_color = if off { c.chip } else { AMBER };
    chamfered_polygon(
        dc,
        thumb_rect,
        thumb_cut,
        thumb_color,
        if off { c.rule } else { AMBER },
    );

    let left_rect = RECT {
        left: r.left + pad,
        top: r.top + pad,
        right: r.left + pad + base_tw,
        bottom: r.bottom - pad,
    };
    let right_rect = RECT {
        left: r.left + pad + base_tw,
        top: r.top + pad,
        right: r.right - pad,
        bottom: r.bottom - pad,
    };
    let base_text = if off { c.muted } else { c.ink };
    label(dc, left_rect, s.fonts.small, base_text, "浅色", DT_CENTER);
    label(dc, right_rect, s.fonts.small, base_text, "深色", DT_CENTER);

    let saved = SaveDC(dc);
    IntersectClipRect(
        dc,
        thumb_rect.left,
        thumb_rect.top,
        thumb_rect.right,
        thumb_rect.bottom,
    );
    let active_text = if off { c.muted } else { rgb(0x171714) };
    label(dc, left_rect, s.fonts.small, active_text, "浅色", DT_CENTER);
    label(dc, right_rect, s.fonts.small, active_text, "深色", DT_CENTER);
    RestoreDC(dc, saved);

    if focus {
        let inset = s.layout.px(5);
        DrawFocusRect(
            dc,
            &rect(inset, inset, r.right - 2 * inset, r.bottom - 2 * inset),
        );
    }
}

pub unsafe fn button(d: &DRAWITEMSTRUCT) {
    SCENE.with_borrow(|s| {
        let Some(s) = s else { return };
        let Some(id) = Id::from_raw(d.CtlID as usize) else {
            return;
        };
        let off = d.itemState & ODS_DISABLED != 0;
        let focus = d.itemState & ODS_FOCUS != 0;
        let selected = d.itemState & ODS_SELECTED != 0;
        let anim = s.buttons.get(&id).copied().unwrap_or_default();
        let hover = if off {
            0.0
        } else {
            anim.hover.value.clamp(0.0, 1.0)
        };
        let press = if off {
            0.0
        } else if s.motion {
            anim.press.value.clamp(0.0, 1.15)
        } else if selected {
            1.0
        } else {
            0.0
        };
        let r = d.rcItem;
        with_backbuffer(d.hDC, r, |dc| {
            if id == Id::Theme {
                draw_theme_slider(dc, r, s, off, focus, hover, press);
                return;
            }
            let c = s.colors;
            let dark = id == Id::Primary;
            let zoom = matches!(id, Id::LogSmaller | Id::LogLarger | Id::LogCopy);
            fill(dc, r, c.panel);

            let shrink = (press * s.layout.px(if zoom { 1 } else { 2 }) as f32)
                .round()
                .clamp(0.0, 3.0) as i32;
            let br = RECT {
                left: r.left + shrink,
                top: r.top + shrink,
                right: (r.right - shrink).max(r.left + shrink + 4),
                bottom: (r.bottom - shrink).max(r.top + shrink + 4),
            };
            let rest_fill = if off {
                c.chip
            } else if dark {
                rgb(0x292824)
            } else {
                c.chip
            };
            let hover_fill = if dark {
                rgb(0x36342e)
            } else {
                lerp_color(c.chip, c.ink, 0.08)
            };
            let base = if off {
                c.chip
            } else {
                let hovered = lerp_color(rest_fill, hover_fill, hover);
                lerp_color(hovered, AMBER, press)
            };
            let rest_border = if off || zoom { c.rule } else { c.ink };
            let border = if off {
                c.rule
            } else {
                let hb = lerp_color(rest_border, AMBER, if zoom { 0.8 * hover } else { 0.5 * hover });
                lerp_color(hb, AMBER, press)
            };
            let cut = s.layout.px(if zoom { 4 } else { 10 });
            chamfered_polygon(dc, br, cut, base, border);
            if id == Id::Primary && !off {
                let bar_w = s.layout.px(5) + (hover * s.layout.px(3) as f32).round() as i32;
                fill(
                    dc,
                    rect(br.left + 1, br.top + 1, bar_w, (br.bottom - br.top - 2).max(1)),
                    AMBER,
                );
            }
            let mut buf = [0u16; 128];
            let n = GetWindowTextW(d.hwndItem, buf.as_mut_ptr(), 128);
            let rest_text = if off {
                c.muted
            } else if dark {
                rgb(0xfffefa)
            } else {
                c.ink
            };
            let text_color = if off {
                c.muted
            } else {
                lerp_color(rest_text, rgb(0x181815), press)
            };
            let font = if zoom { s.fonts.small } else { s.fonts.strong };
            label(
                dc,
                br,
                font,
                text_color,
                &String::from_utf16_lossy(&buf[..n as usize]),
                DT_CENTER,
            );
            if focus {
                let inset = s.layout.px(if zoom { 3 } else { 5 }) + shrink;
                DrawFocusRect(
                    dc,
                    &rect(inset, inset, r.right - 2 * inset, r.bottom - 2 * inset),
                );
            }
        });
    });
}

unsafe fn char_width(dc: HDC, ch: char) -> i32 {
    let mut utf16 = [0u16; 2];
    let slice = ch.encode_utf16(&mut utf16);
    let mut size = SIZE::default();
    GetTextExtentPoint32W(dc, slice.as_ptr(), slice.len() as i32, &mut size);
    size.cx
}

unsafe fn draw_roller(dc: HDC, r: RECT, id: Id, hwnd_item: HWND, s: &Scene) {
    let c = s.colors;
    let bg = s.bg(Some(id));
    fill(dc, r, bg);
    let font = s.fonts.control(id);
    let old_font = SelectObject(dc, font);
    SetBkMode(dc, TRANSPARENT as i32);

    let mut tm = TEXTMETRICW::default();
    let line_h = if GetTextMetricsW(dc, &mut tm) != 0 && tm.tmHeight > 0 {
        tm.tmHeight
    } else {
        (r.bottom - r.top).max(1)
    };
    let right_align = matches!(id, Id::Jumps | Id::Hit | Id::Land | Id::Elapsed);

    let Some(roller) = s.rollers.get(&id).filter(|rl| !rl.slots.is_empty()) else {
        let mut buf = [0u16; 128];
        let n = GetWindowTextW(hwnd_item, buf.as_mut_ptr(), 128);
        label(
            dc,
            r,
            font,
            c.ink,
            &String::from_utf16_lossy(&buf[..n as usize]),
            if right_align { DT_RIGHT } else { DT_LEFT },
        );
        SelectObject(dc, old_font);
        return;
    };

    let mut digit_widths = [0i32; 10];
    let digit_w = if GetCharWidth32W(dc, '0' as u32, '9' as u32, digit_widths.as_mut_ptr()) != 0 {
        digit_widths.iter().copied().max().unwrap_or(0)
    } else {
        ('0'..='9').map(|d| char_width(dc, d)).max().unwrap_or(0)
    };
    let widths: Vec<i32> = roller
        .slots
        .iter()
        .map(|slot| match slot {
            Slot::Digit(_) => digit_w,
            Slot::Static(ch) => char_width(dc, *ch),
        })
        .collect();
    let total_w: i32 = widths.iter().sum();
    let mut slot_x = if right_align {
        r.right - total_w
    } else {
        r.left
    };
    let base_y = r.top + (r.bottom - r.top - line_h) / 2;
    let clip_top = base_y.max(r.top);
    let clip_bottom = (base_y + line_h).min(r.bottom);
    let pitch = line_h + s.layout.px(2).max(2);

    for (slot, &slot_w) in roller.slots.iter().zip(&widths) {
        match slot {
            Slot::Static(ch) => {
                let mut utf16 = [0u16; 2];
                let slice = ch.encode_utf16(&mut utf16);
                SetTextColor(dc, c.ink);
                let mut sr = rect(slot_x, base_y, slot_w, line_h);
                DrawTextW(
                    dc,
                    slice.as_ptr(),
                    slice.len() as i32,
                    &mut sr,
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                );
            }
            Slot::Digit(spring) => {
                let draw_digit = |ch: char, y: i32, color: u32| {
                    let mut utf16 = [0u16; 2];
                    let slice = ch.encode_utf16(&mut utf16);
                    SetTextColor(dc, color);
                    let mut dr = rect(slot_x, y, slot_w, line_h);
                    DrawTextW(
                        dc,
                        slice.as_ptr(),
                        slice.len() as i32,
                        &mut dr,
                        DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                    );
                };

                let saved = SaveDC(dc);
                IntersectClipRect(dc, slot_x, clip_top, slot_x + slot_w, clip_bottom);

                let v = spring.value;
                if spring.settled() || (v - v.round()).abs() < 0.002 {
                    let d = char::from_digit((v.round() as i32).rem_euclid(10) as u32, 10)
                        .unwrap_or('0');
                    draw_digit(d, base_y, c.ink);
                } else {
                    let lo = v.floor();
                    let frac = v - lo;
                    let lo_i = lo as i32;
                    let d0 = char::from_digit(lo_i.rem_euclid(10) as u32, 10).unwrap_or('0');
                    let d1 = char::from_digit((lo_i + 1).rem_euclid(10) as u32, 10).unwrap_or('0');

                    let fade0 = (frac * frac).min(0.82);
                    let rem = 1.0 - frac;
                    let fade1 = (rem * rem).min(0.82);

                    let y0 = base_y - (frac * pitch as f32).round() as i32;
                    let y1 = base_y + (rem * pitch as f32).round() as i32;
                    draw_digit(d0, y0, lerp_color(c.ink, bg, fade0));
                    draw_digit(d1, y1, lerp_color(c.ink, bg, fade1));
                }

                RestoreDC(dc, saved);
            }
        }
        slot_x += slot_w;
    }

    SelectObject(dc, old_font);
}

pub unsafe fn roller(d: &DRAWITEMSTRUCT) {
    SCENE.with_borrow(|s| {
        let Some(s) = s else { return };
        let Some(id) = Id::from_raw(d.CtlID as usize) else {
            return;
        };
        let r = d.rcItem;
        with_backbuffer(d.hDC, r, |dc| draw_roller(dc, r, id, d.hwndItem, s));
    });
}

pub unsafe fn phase_badge(hwnd: HWND, dc: HDC) {
    SCENE.with_borrow(|s| {
        let Some(s) = s else { return };
        let mut r = RECT::default();
        GetClientRect(hwnd, &mut r);
        let mut buf = [0u16; 128];
        let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), 128);
        let text = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        let (bg, border, left_bar, fg) = s.current_phase_colors();
        let slide_t = if s.motion {
            s.phase_slide.value.clamp(0.0, 1.0)
        } else {
            1.0
        };
        let cut = s.layout.px(8);
        with_backbuffer(dc, r, |mem| {
            fill(mem, r, s.colors.panel);
            chamfered_polygon(mem, r, cut, bg, border);
            if left_bar != bg {
                fill(
                    mem,
                    rect(
                        r.left + 1,
                        r.top + 1,
                        s.layout.px(4).max(2),
                        (r.bottom - r.top - 2).max(1),
                    ),
                    left_bar,
                );
            }
            if let Some(ratio) = s.countdown_ratio {
                let bar_h = s.layout.px(3).max(2);
                let span = ((r.right - r.left - cut - s.layout.px(2)) as f32
                    * ratio.clamp(0.0, 1.0))
                    .round() as i32;
                if span > 0 {
                    fill(
                        mem,
                        rect(r.left + 1, r.bottom - 1 - bar_h, span, bar_h),
                        AMBER,
                    );
                }
            }
            let dy = ((1.0 - slide_t) * s.layout.px(4) as f32).round() as i32;
            let tr = rect(
                r.left + s.layout.px(6),
                r.top + dy,
                (r.right - r.left - s.layout.px(12)).max(1),
                (r.bottom - r.top).max(1),
            );
            let fg_fade = lerp_color(bg, fg, slide_t);
            label(mem, tr, s.fonts.strong, fg_fade, &text, DT_CENTER);
        });
    });
}

pub unsafe fn notice_banner(hwnd: HWND, dc: HDC) {
    SCENE.with_borrow(|s| {
        let Some(s) = s else { return };
        let mut r = RECT::default();
        GetClientRect(hwnd, &mut r);
        let mut buf = [0u16; 512];
        let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), 512);
        let text = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        let is_alert = text.contains("失败")
            || text.contains("缺少")
            || text.contains("退出")
            || text.contains("无法")
            || text.contains("断开");
        let cut = s.layout.px(8);
        with_backbuffer(dc, r, |mem| {
            fill(mem, r, s.colors.paper);
            chamfered_polygon(mem, r, cut, AMBER, AMBER);
            fill(
                mem,
                rect(
                    r.left + 1,
                    r.top + 1,
                    s.layout.px(5).max(3),
                    (r.bottom - r.top - 2).max(1),
                ),
                if is_alert { ALERT } else { rgb(0x171714) },
            );
            let tr = rect(
                r.left + s.layout.px(14),
                r.top,
                (r.right - r.left - s.layout.px(24)).max(1),
                (r.bottom - r.top).max(1),
            );
            label(mem, tr, s.fonts.body, rgb(0x171714), &text, DT_LEFT);
        });
    });
}

pub unsafe fn daemon_checkbox(hwnd: HWND, dc: HDC, focused: bool, disabled: bool) {
    SCENE.with_borrow(|s| {
        let Some(s) = s else { return };
        let mut r = RECT::default();
        GetClientRect(hwnd, &mut r);
        let mut buf = [0u16; 128];
        let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), 128);
        let text = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        let anim = s.buttons.get(&Id::Daemon).copied().unwrap_or_default();
        let hover = if disabled {
            0.0
        } else {
            anim.hover.value.clamp(0.0, 1.0)
        };
        let press = if disabled {
            0.0
        } else {
            anim.press.value.clamp(0.0, 1.0)
        };
        let chk = s.daemon_check.value.clamp(0.0, 1.0);
        let c = s.colors;
        with_backbuffer(dc, r, |mem| {
            fill(mem, r, c.panel);
            let box_sz = s.layout.px(18).max(12);
            let shrink = (press * s.layout.px(1) as f32).round() as i32;
            let bx = r.left + s.layout.px(2) + shrink;
            let by = r.top + (r.bottom - r.top - box_sz) / 2 + shrink;
            let sz = (box_sz - 2 * shrink).max(8);
            let box_r = rect(bx, by, sz, sz);
            let box_fill = if disabled {
                c.chip
            } else {
                lerp_color(c.chip, AMBER, chk)
            };
            let box_border = if disabled {
                c.rule
            } else {
                lerp_color(lerp_color(c.ink, AMBER, hover * 0.7), AMBER, chk)
            };
            chamfered_polygon(mem, box_r, s.layout.px(4).max(2), box_fill, box_border);
            if chk > 0.05 {
                let p0 = POINT {
                    x: bx + sz * 24 / 100,
                    y: by + sz * 52 / 100,
                };
                let p1 = POINT {
                    x: bx + sz * 44 / 100,
                    y: by + sz * 72 / 100,
                };
                let p2 = POINT {
                    x: bx + sz * 78 / 100,
                    y: by + sz * 28 / 100,
                };
                let pen = CreatePen(
                    PS_SOLID,
                    s.layout.px(2).max(2),
                    if disabled { c.muted } else { rgb(0x171714) },
                );
                let old_pen = SelectObject(mem, pen);
                MoveToEx(mem, p0.x, p0.y, null_mut());
                let seg1 = (chk * 2.0).clamp(0.0, 1.0);
                let mx = p0.x + ((p1.x - p0.x) as f32 * seg1).round() as i32;
                let my = p0.y + ((p1.y - p0.y) as f32 * seg1).round() as i32;
                LineTo(mem, mx, my);
                if chk > 0.5 {
                    let seg2 = ((chk - 0.5) * 2.0).clamp(0.0, 1.0);
                    let ex = p1.x + ((p2.x - p1.x) as f32 * seg2).round() as i32;
                    let ey = p1.y + ((p2.y - p1.y) as f32 * seg2).round() as i32;
                    LineTo(mem, ex, ey);
                }
                SelectObject(mem, old_pen);
                DeleteObject(pen);
            }
            let tx = bx + sz + s.layout.px(9);
            let tr = rect(
                tx,
                r.top,
                (r.right - tx - s.layout.px(4)).max(1),
                (r.bottom - r.top).max(1),
            );
            label(
                mem,
                tr,
                s.fonts.body,
                if disabled { c.muted } else { c.ink },
                &text,
                DT_LEFT,
            );
            if focused {
                let inset = s.layout.px(2);
                DrawFocusRect(
                    mem,
                    &rect(
                        tx - inset,
                        r.top + inset,
                        (r.right - tx).max(4),
                        (r.bottom - r.top - 2 * inset).max(4),
                    ),
                );
            }
        });
    });
}

pub unsafe fn backend_combo(hwnd: HWND, dc: HDC, focused: bool, disabled: bool) {
    SCENE.with_borrow(|s| {
        let Some(s) = s else { return };
        let mut r = RECT::default();
        GetClientRect(hwnd, &mut r);
        let sel = SendMessageW(hwnd, CB_GETCURSEL, 0, 0);
        let text = if sel >= 0 {
            let len = SendMessageW(hwnd, CB_GETLBTEXTLEN, sel as usize, 0);
            if len > 0 && len < 256 {
                let mut buf = vec![0u16; len as usize + 1];
                let got = SendMessageW(hwnd, CB_GETLBTEXT, sel as usize, buf.as_mut_ptr() as isize);
                if got > 0 {
                    String::from_utf16_lossy(&buf[..got as usize])
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        let anim = s.buttons.get(&Id::Backend).copied().unwrap_or_default();
        let hover = if disabled {
            0.0
        } else {
            anim.hover.value.clamp(0.0, 1.0)
        };
        let press = if disabled {
            0.0
        } else {
            anim.press.value.clamp(0.0, 1.0)
        };
        let c = s.colors;
        with_backbuffer(dc, r, |mem| {
            fill(mem, r, c.panel);
            let hover_fill = lerp_color(c.chip, c.ink, 0.06);
            let bg = if disabled {
                c.chip
            } else {
                lerp_color(c.chip, hover_fill, hover)
            };
            let border = if disabled {
                c.rule
            } else {
                lerp_color(c.ink, AMBER, (hover * 0.65 + press * 0.35).clamp(0.0, 1.0))
            };
            chamfered_polygon(mem, r, s.layout.px(8), bg, border);
            let arrow_zone = s.layout.px(24);
            let tr = rect(
                r.left + s.layout.px(10),
                r.top,
                (r.right - r.left - arrow_zone - s.layout.px(12)).max(1),
                (r.bottom - r.top).max(1),
            );
            label(
                mem,
                tr,
                s.fonts.body,
                if disabled { c.muted } else { c.ink },
                &text,
                DT_LEFT,
            );
            let cx = r.right - arrow_zone / 2 - s.layout.px(2);
            let cy = (r.top + r.bottom) / 2;
            let aw = s.layout.px(4).max(3);
            let ah = s.layout.px(3).max(2);
            let tri = [
                POINT {
                    x: cx - aw,
                    y: cy - ah / 2,
                },
                POINT {
                    x: cx + aw,
                    y: cy - ah / 2,
                },
                POINT {
                    x: cx,
                    y: cy + ah,
                },
            ];
            let arrow_col = if disabled {
                c.muted
            } else {
                lerp_color(c.ink, AMBER, hover * 0.7)
            };
            SelectObject(mem, GetStockObject(DC_PEN));
            SelectObject(mem, GetStockObject(DC_BRUSH));
            SetDCPenColor(mem, arrow_col);
            SetDCBrushColor(mem, arrow_col);
            Polygon(mem, tri.as_ptr(), 3);
            if focused {
                let inset = s.layout.px(3);
                DrawFocusRect(
                    mem,
                    &rect(inset, inset, r.right - 2 * inset, r.bottom - 2 * inset),
                );
            }
        });
    });
}

pub unsafe fn ledger_item(d: &DRAWITEMSTRUCT) {
    if d.itemID == u32::MAX {
        return;
    }
    SCENE.with_borrow(|s| {
        let Some(s) = s else { return };
        let len = SendMessageW(d.hwndItem, LB_GETTEXTLEN, d.itemID as usize, 0);
        if !(0..=4096).contains(&len) {
            return;
        }
        let mut buf = vec![0u16; len as usize + 1];
        let got = SendMessageW(
            d.hwndItem,
            LB_GETTEXT,
            d.itemID as usize,
            buf.as_mut_ptr() as isize,
        );
        if got < 0 {
            return;
        }
        let text = String::from_utf16_lossy(&buf[..got as usize]);
        let r = d.rcItem;
        let c = s.colors;
        let selected = d.itemState & ODS_SELECTED != 0;
        let even = d.itemID.is_multiple_of(2);
        let zebra = if even {
            c.panel
        } else {
            lerp_color(c.panel, c.paper, 0.36)
        };
        let bg = if selected {
            lerp_color(zebra, AMBER, 0.22)
        } else {
            zebra
        };
        let sage = rgb(0x6e9e82);
        let mut bar_col = if selected {
            AMBER
        } else if text.contains("本局结束")
            || text.contains("失败")
            || text.contains("退出")
            || text.contains("无法")
            || text.contains("遮挡")
            || text.contains("断开")
        {
            ALERT
        } else if text.starts_with("第 ") && text.contains("已落地") {
            AMBER
        } else if text.contains("已找到") || text.contains("通过") || text.contains("识别后端") {
            sage
        } else {
            c.rule
        };
        let mut fg = c.ink;
        let mut slide_x = 0;
        if s.motion {
            if let Some((_, added_at, item_text)) = s.log_items.get(d.itemID as usize) {
                if item_text == &text {
                    let age = s.frame_time.saturating_duration_since(*added_at).as_secs_f32();
                    if age < 0.22 {
                        let p = age / 0.22;
                        let ease = 1.0 - (1.0 - p).powi(3);
                        slide_x = ((1.0 - ease) * s.layout.px(8) as f32).round() as i32;
                        fg = lerp_color(bg, c.ink, ease);
                        bar_col = lerp_color(bg, bar_col, ease);
                    }
                }
            }
        }
        with_backbuffer(d.hDC, r, |mem| {
            fill(mem, r, bg);
            let bar_w = s.layout.px(3).max(2);
            let pad_y = s.layout.px(2).max(1);
            fill(
                mem,
                rect(
                    r.left + s.layout.px(2),
                    r.top + pad_y,
                    bar_w,
                    (r.bottom - r.top - 2 * pad_y).max(1),
                ),
                bar_col,
            );
            let mut tr = rect(
                r.left + s.layout.px(11) + slide_x,
                r.top,
                (r.right - r.left - s.layout.px(15) - slide_x).max(1),
                (r.bottom - r.top).max(1),
            );
            let old_font = SelectObject(mem, s.fonts.log);
            SetBkMode(mem, TRANSPARENT as i32);
            SetTextColor(mem, fg);
            let wtext = wide(&text);
            DrawTextW(
                mem,
                wtext.as_ptr(),
                (wtext.len() - 1) as i32,
                &mut tr,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
            SelectObject(mem, old_font);
        });
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

    #[test]
    fn phase_tone_and_color_interpolation_match_semantics() {
        let light = Colors::new(false);
        assert_eq!(
            PhaseTone::for_phase("playing", false),
            PhaseTone::Active
        );
        assert_eq!(
            PhaseTone::for_phase("capture_blocked", false),
            PhaseTone::Alert
        );
        assert_eq!(
            PhaseTone::for_phase("stopped", true),
            PhaseTone::Alert
        );
        assert_eq!(
            PhaseTone::for_phase("stopped", false),
            PhaseTone::Idle
        );
        assert_eq!(lerp_color(light.paper, light.ink, 0.0), light.paper);
        assert_eq!(lerp_color(light.paper, light.ink, 1.0), light.ink);
    }
}
