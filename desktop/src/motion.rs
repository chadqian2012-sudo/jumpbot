//! Apple-inspired physical motion primitives: damped springs, rubber-banding, and LED rollers.
use std::f32::consts::TAU;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    pub value: f32,
    pub velocity: f32,
    pub target: f32,
    stiffness: f32,
    damping: f32,
}

impl Spring {
    pub fn new(value: f32, response_s: f32, damping_fraction: f32) -> Self {
        let omega = TAU / response_s.max(1e-4);
        Self {
            value,
            velocity: 0.0,
            target: value,
            stiffness: omega * omega,
            damping: 2.0 * damping_fraction * omega,
        }
    }

    pub fn snappy(value: f32) -> Self {
        Self::new(value, 0.30, 0.86)
    }

    pub fn bouncy(value: f32) -> Self {
        Self::new(value, 0.40, 0.80)
    }

    pub fn smooth(value: f32) -> Self {
        Self::new(value, 0.40, 0.92)
    }

    pub fn roll(value: f32) -> Self {
        Self::new(value, 0.26, 0.98)
    }

    pub fn hover(value: f32) -> Self {
        Self::new(value, 0.20, 0.92)
    }

    pub fn press(value: f32) -> Self {
        Self::new(value, 0.20, 0.84)
    }

    pub fn set_target(&mut self, target: f32) {
        self.target = target;
    }

    pub fn snap(&mut self, value: f32) {
        self.value = value;
        self.target = value;
        self.velocity = 0.0;
    }

    pub fn settled(&self) -> bool {
        let scale = self.target.abs().clamp(1.0, 4.0);
        self.velocity.abs() < 2e-3 * scale && (self.value - self.target).abs() < 1e-3 * scale
    }

    pub fn step(&mut self, dt: f32) {
        if self.settled() {
            self.value = self.target;
            self.velocity = 0.0;
            return;
        }
        const SUBSTEP: f32 = 1.0 / 480.0;
        let mut remaining = dt.clamp(0.0, 1.0 / 30.0);
        while remaining > 0.0 {
            let h = remaining.min(SUBSTEP);
            let accel =
                self.stiffness * (self.target - self.value) - self.damping * self.velocity;
            self.velocity += accel * h;
            self.value += self.velocity * h;
            remaining -= h;
        }
        if self.settled() {
            self.value = self.target;
            self.velocity = 0.0;
        }
    }
}

/// Closed-form unit step response (`0 -> 1`) for an oscillator parameterized by `(response, zeta)`.
pub fn spring_curve(t_s: f32, response: f32, zeta: f32) -> f32 {
    if t_s <= 0.0 {
        return 0.0;
    }
    let omega = TAU / response.max(1e-4);
    let zeta = zeta.max(0.0);
    if zeta < 1.0 - 1e-5 {
        let root = (1.0 - zeta * zeta).sqrt();
        let omega_d = omega * root;
        let decay = (-zeta * omega * t_s).exp();
        1.0 - decay * ((omega_d * t_s).cos() + (zeta / root) * (omega_d * t_s).sin())
    } else if (zeta - 1.0).abs() <= 1e-5 {
        let decay = (-omega * t_s).exp();
        1.0 - decay * (1.0 + omega * t_s)
    } else {
        let root = (zeta * zeta - 1.0).sqrt();
        let r1 = -omega * (zeta - root);
        let r2 = -omega * (zeta + root);
        1.0 - (r2 * (r1 * t_s).exp() - r1 * (r2 * t_s).exp()) / (r2 - r1)
    }
}

/// Bounded monotonic rubber-band displacement matching iOS/macOS overscroll physics.
pub fn rubber_band(excess: f32, dim: f32) -> f32 {
    if excess == 0.0 || dim <= 0.0 {
        return 0.0;
    }
    (1.0 - 1.0 / (excess.abs() * 0.55 / dim + 1.0)) * dim * excess.signum()
}

#[derive(Clone, Debug, PartialEq)]
pub enum Slot {
    Digit(Spring),
    Static(char),
}

#[derive(Clone, Debug, Default)]
pub struct Roller {
    pub slots: Vec<Slot>,
    last_text: String,
    last_numeric: Option<f64>,
}

fn parse_numeric(text: &str) -> Option<f64> {
    let mut buf = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii_digit() || c == '.' {
            buf.push(c);
        }
    }
    if buf.is_empty() {
        None
    } else {
        buf.parse::<f64>().ok()
    }
}

impl Roller {
    pub fn set(&mut self, text: &str, animate: bool) {
        if self.last_text == text && !self.slots.is_empty() {
            if !animate {
                self.snap();
            }
            return;
        }
        let next_numeric = parse_numeric(text);
        let snap_mode = !animate
            || self.slots.is_empty()
            || text == "—"
            || self.last_text == "—"
            || self.last_text.is_empty();

        let chars: Vec<char> = text.chars().collect();
        if snap_mode {
            self.slots = chars
                .into_iter()
                .map(|c| match c.to_digit(10) {
                    Some(d) => Slot::Digit(Spring::roll(d as f32)),
                    None => Slot::Static(c),
                })
                .collect();
            self.last_text = text.to_owned();
            self.last_numeric = next_numeric;
            return;
        }

        let is_rate = text.ends_with('%');
        let decreasing = match (self.last_numeric, next_numeric) {
            (Some(prev), Some(next)) => next < prev,
            _ => false,
        };

        let old_len = self.slots.len();
        let new_len = chars.len();
        let mut next_slots = Vec::with_capacity(new_len);
        for (i, &c) in chars.iter().enumerate() {
            let from_right = new_len - 1 - i;
            let old_slot = old_len
                .checked_sub(1 + from_right)
                .and_then(|idx| self.slots.get(idx));
            match c.to_digit(10) {
                Some(d) => {
                    let new_d = d as i32;
                    if let Some(Slot::Digit(prev_spring)) = old_slot {
                        let mut spring = *prev_spring;
                        let old_d = (spring.target.round() as i32).rem_euclid(10);
                        if new_d != old_d {
                            let target = if is_rate {
                                let shortest = (new_d - old_d + 4).rem_euclid(10) - 4;
                                spring.target + shortest as f32
                            } else if decreasing {
                                spring.target - (old_d - new_d).rem_euclid(10) as f32
                            } else {
                                spring.target + (new_d - old_d).rem_euclid(10) as f32
                            };
                            spring.set_target(target);
                        }
                        next_slots.push(Slot::Digit(spring));
                    } else {
                        let mut spring = Spring::roll(0.0);
                        if new_d != 0 {
                            spring.set_target(new_d as f32);
                        }
                        next_slots.push(Slot::Digit(spring));
                    }
                }
                None => next_slots.push(Slot::Static(c)),
            }
        }
        self.slots = next_slots;
        self.last_text = text.to_owned();
        self.last_numeric = next_numeric;
    }

    pub fn snap(&mut self) {
        for slot in &mut self.slots {
            if let Slot::Digit(s) = slot {
                let d = s.target.round().rem_euclid(10.0);
                s.snap(d);
            }
        }
    }

    pub fn step(&mut self, dt: f32) {
        for slot in &mut self.slots {
            if let Slot::Digit(s) = slot {
                s.step(dt);
                if s.settled() {
                    let d = s.target.round().rem_euclid(10.0);
                    s.snap(d);
                }
            }
        }
    }

    pub fn active(&self) -> bool {
        self.slots
            .iter()
            .any(|slot| matches!(slot, Slot::Digit(s) if !s.settled()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spring_converges_and_respects_damping_and_velocity() {
        let mut underdamped = Spring::new(0.0, 0.35, 0.65);
        underdamped.set_target(1.0);
        let mut max_val = 0.0_f32;
        for _ in 0..120 {
            underdamped.step(1.0 / 60.0);
            max_val = max_val.max(underdamped.value);
        }
        assert!(max_val > 1.0, "underdamped spring should overshoot: {max_val}");
        assert!(underdamped.settled());
        assert_eq!(underdamped.value, 1.0);

        let mut critical = Spring::new(0.0, 0.35, 1.0);
        critical.set_target(1.0);
        let mut crit_max = 0.0_f32;
        for _ in 0..120 {
            critical.step(1.0 / 60.0);
            crit_max = crit_max.max(critical.value);
        }
        assert!(crit_max <= 1.0005, "critically damped should not overshoot: {crit_max}");
        assert!(critical.settled());

        let mut interrupted = Spring::snappy(0.0);
        interrupted.set_target(10.0);
        for _ in 0..5 {
            interrupted.step(1.0 / 60.0);
        }
        let v_before = interrupted.velocity;
        assert!(v_before > 0.0);
        interrupted.set_target(2.0);
        assert_eq!(interrupted.velocity, v_before);
    }

    #[test]
    fn rubber_band_is_odd_monotonic_and_bounded() {
        let dim = 48.0;
        let mut prev = 0.0;
        for x in [1.0, 10.0, 50.0, 200.0, 1000.0] {
            let y = rubber_band(x, dim);
            assert!(y > prev && y < dim, "rubber_band({x}) = {y}");
            assert!((rubber_band(-x, dim) + y).abs() < 1e-6);
            prev = y;
        }
        assert_eq!(rubber_band(0.0, dim), 0.0);
    }

    #[test]
    fn roller_rolls_odometer_from_nine_to_ten() {
        let mut roller = Roller::default();
        roller.set("9", false);
        assert!(!roller.active());

        roller.set("10", true);
        assert!(roller.active());
        assert_eq!(roller.slots.len(), 2);
        match (&roller.slots[0], &roller.slots[1]) {
            (Slot::Digit(tens), Slot::Digit(ones)) => {
                assert_eq!(tens.value, 0.0);
                assert_eq!(tens.target, 1.0);
                assert_eq!(ones.value, 9.0);
                assert_eq!(ones.target, 10.0);
            }
            other => panic!("unexpected slots: {other:?}"),
        }

        for _ in 0..120 {
            roller.step(1.0 / 60.0);
        }
        assert!(!roller.active());
        let displayed: String = roller
            .slots
            .iter()
            .map(|slot| match slot {
                Slot::Digit(s) => char::from_digit((s.value.floor() as i32).rem_euclid(10) as u32, 10).unwrap(),
                Slot::Static(c) => *c,
            })
            .collect();
        assert_eq!(displayed, "10");
    }

    #[test]
    fn roller_rate_takes_shortest_path_without_overshoot() {
        let mut roller = Roller::default();
        roller.set("90.9%", false);
        roller.set("91.7%", true);
        match &roller.slots[3] {
            Slot::Digit(tenths) => {
                assert_eq!(tenths.target, 7.0, "9 -> 7 should roll backward by 2");
            }
            other => panic!("expected tenths digit, got {other:?}"),
        }
        let mut min_val = 9.0_f32;
        for _ in 0..120 {
            roller.step(1.0 / 60.0);
            if let Slot::Digit(tenths) = &roller.slots[3] {
                min_val = min_val.min(tenths.value);
            }
        }
        assert!(min_val >= 6.99, "roller should not noticeably overshoot: {min_val}");
        assert!(!roller.active());
    }

    #[test]
    fn spring_curve_starts_at_zero_and_settles_near_one() {
        assert_eq!(spring_curve(0.0, 0.45, 0.78), 0.0);
        let settled = spring_curve(5.0, 0.45, 0.78);
        assert!((settled - 1.0).abs() < 1e-4, "settled={settled}");
    }
}
