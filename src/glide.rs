//! Smooth keyboard resizing: held `Mod + Shift + Arrow` keys glide a window edge instead of
//! jumping in fixed steps. Smithay's winit backend never delivers key repeats, so the main
//! loop advances the motion every frame while a key is down.

use std::time::Instant;

use smithay::backend::input::Keycode;

use crate::config::{RESIZE_RAMP_SECS, RESIZE_SPEED_MAX, RESIZE_SPEED_MIN, RESIZE_STEP};

/// Longest frame gap credited to the motion, so a stalled frame cannot fling the edge.
const MAX_FRAME_SECS: f64 = 0.05;

#[derive(Debug, Clone)]
struct GlideKey {
    /// `None` for a tap with no physical key behind it (released at once).
    keycode: Option<Keycode>,
    dir: (i32, i32),
    pressed: Instant,
    held: bool,
    travelled: f64,
}

/// Arrow keys currently gliding a window edge, and the motion they still owe.
#[derive(Debug, Clone)]
pub struct KeyGlide {
    keys: Vec<GlideKey>,
    last_tick: Instant,
    /// Sub-pixel motion carried over to the next frame.
    carry: (f64, f64),
}

/// Edge speed (logical px/s) once a key has been held for `held` seconds: slow at first
/// for precise nudges, easing up to full speed for long moves.
pub(crate) fn glide_speed(held: f64) -> f64 {
    let t = (held / RESIZE_RAMP_SECS).clamp(0.0, 1.0);
    RESIZE_SPEED_MIN + (RESIZE_SPEED_MAX - RESIZE_SPEED_MIN) * t * t
}

impl KeyGlide {
    pub fn new(now: Instant) -> Self {
        Self {
            keys: Vec::new(),
            last_tick: now,
            carry: (0.0, 0.0),
        }
    }

    /// Start gliding in `dir` (unit steps); `keycode: None` glides a single tap.
    pub fn press(&mut self, keycode: Option<Keycode>, dir: (i32, i32), now: Instant) {
        if self.keys.is_empty() {
            self.last_tick = now;
        }
        if keycode.is_some() && self.keys.iter().any(|k| k.held && k.keycode == keycode) {
            return;
        }
        self.keys.push(GlideKey {
            keycode,
            dir,
            pressed: now,
            held: keycode.is_some(),
            travelled: 0.0,
        });
    }

    /// The key came up: it finishes its tap distance, then stops.
    pub fn release(&mut self, keycode: Keycode) {
        for key in self.keys.iter_mut().filter(|k| k.keycode == Some(keycode)) {
            key.held = false;
        }
    }

    pub fn release_all(&mut self) {
        for key in &mut self.keys {
            key.held = false;
        }
    }

    pub fn is_done(&self) -> bool {
        self.keys.is_empty()
    }

    /// Whole logical pixels `(dx, dy)` the edge moves since the previous frame.
    pub fn advance(&mut self, now: Instant) -> (i32, i32) {
        let dt = now
            .saturating_duration_since(self.last_tick)
            .as_secs_f64()
            .min(MAX_FRAME_SECS);
        self.last_tick = now;
        let (mut x, mut y) = self.carry;
        for key in &mut self.keys {
            let held_for = now.saturating_duration_since(key.pressed).as_secs_f64();
            let mut step = glide_speed(held_for) * dt.min(held_for);
            if !key.held {
                step = step.min(RESIZE_STEP - key.travelled).max(0.0);
            }
            key.travelled += step;
            x += f64::from(key.dir.0) * step;
            y += f64::from(key.dir.1) * step;
        }
        self.keys
            .retain(|k| k.held || k.travelled < RESIZE_STEP - 1e-6);
        // Once the last key has finished, round instead of dropping the sub-pixel rest.
        let whole = if self.keys.is_empty() {
            (x.round(), y.round())
        } else {
            (x.trunc(), y.trunc())
        };
        self.carry = (x - whole.0, y - whole.1);
        (whole.0 as i32, whole.1 as i32)
    }
}
