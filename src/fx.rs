//! Game mode (`--fx`, toggled with `x`): particles, floating points, combos, levels and
//! achievements layered on top of the normal view.
//!
//! Scoring happens when changes arrive. Visual effects need screen positions, which only
//! the renderer knows, so changes are queued as [`Spawn`]s and turned into particles
//! during the next draw.

use std::collections::VecDeque;
use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::change::{Change, ChangeKind};
use crate::demo::Rng;
use crate::ui::theme::{palette, Rgb, Theme};

/// Changes closer together than this keep the combo going (seconds).
pub const COMBO_WINDOW: f64 = 2.5;
const MAX_PARTICLES: usize = 1200;
const MAX_PENDING: usize = 400;
const BANNER_LIFE: f64 = 1.5;
const TOAST_LIFE: f64 = 3.4;
/// Combo needed for FEVER (rainbow borders).
pub const FEVER_COMBO: u32 = 50;

const TIERS: &[(u32, &str)] =
    &[(10, "NICE!"), (25, "GREAT!"), (50, "AWESOME!!"), (100, "UNSTOPPABLE!!!"), (250, "GODLIKE")];

struct Achievement {
    id: &'static str,
    title: &'static str,
    desc: &'static str,
    check: fn(&Fx) -> bool,
}

const ACHIEVEMENTS: &[Achievement] = &[
    Achievement {
        id: "seed",
        title: "Seedling",
        desc: "Create your first file",
        check: |f| f.counts[0] >= 1,
    },
    Achievement { id: "reaper", title: "Reaper", desc: "Delete a file", check: |f| f.counts[2] >= 1 },
    Achievement {
        id: "shift",
        title: "Shapeshifter",
        desc: "Rename or move something",
        check: |f| f.counts[3] >= 1,
    },
    Achievement { id: "garden", title: "Gardener", desc: "Create 25 files", check: |f| f.counts[0] >= 25 },
    Achievement { id: "busy", title: "Busy Bee", desc: "100 changes", check: |f| f.total() >= 100 },
    Achievement {
        id: "combo",
        title: "Combo Breaker",
        desc: "Reach a 25x combo",
        check: |f| f.best_combo >= 25,
    },
    Achievement {
        id: "fever",
        title: "Fever Pitch",
        desc: "Reach a 50x combo",
        check: |f| f.best_combo >= 50,
    },
    Achievement {
        id: "nuke",
        title: "Demolition",
        desc: "Delete a whole directory",
        check: |f| f.dirs_deleted >= 1,
    },
    Achievement { id: "marathon", title: "Marathon", desc: "1,000 changes", check: |f| f.total() >= 1000 },
];

/// A change waiting for the renderer to give it a place on screen.
#[derive(Debug, Clone)]
pub struct Spawn {
    pub path: PathBuf,
    pub kind: ChangeKind,
    pub is_dir: bool,
    pub points: u64,
}

struct Particle {
    x: f64,
    y: f64,
    vx: f64,
    vy: f64,
    born: f64,
    life: f64,
    color: Rgb,
    /// Fixed glyph (shattered letters); `None` animates through spark glyphs.
    glyph: Option<char>,
    gravity: f64,
}

struct Popup {
    text: String,
    x: f64,
    y: f64,
    born: f64,
    color: Rgb,
}

struct Sweep {
    x: u16,
    y: u16,
    len: u16,
    born: f64,
    color: Rgb,
}

struct Banner {
    text: String,
    sub: String,
    born: f64,
}

struct Toast {
    title: &'static str,
    desc: &'static str,
    born: Option<f64>,
}

pub struct Fx {
    pub enabled: bool,
    rng: Rng,
    particles: Vec<Particle>,
    popups: Vec<Popup>,
    sweeps: Vec<Sweep>,
    banner: Option<Banner>,
    toasts: VecDeque<Toast>,
    pending: Vec<Spawn>,
    fireworks: u32,
    pub score: u64,
    shown_score: f64,
    pub combo: u32,
    pub best_combo: u32,
    last_event: f64,
    pub level: u32,
    shake_until: f64,
    shake_power: i16,
    flash_at: f64,
    last_kind: Option<(ChangeKind, f64)>,
    last_update: f64,
    /// Create, modify, delete, rename.
    counts: [u64; 4],
    dirs_deleted: u64,
    unlocked: Vec<&'static str>,
}

impl Fx {
    pub fn new(enabled: bool, seed: u64) -> Self {
        Fx {
            enabled,
            rng: Rng::new(seed),
            particles: Vec::new(),
            popups: Vec::new(),
            sweeps: Vec::new(),
            banner: None,
            toasts: VecDeque::new(),
            pending: Vec::new(),
            fireworks: 0,
            score: 0,
            shown_score: 0.0,
            combo: 0,
            best_combo: 0,
            last_event: f64::NEG_INFINITY,
            level: 1,
            shake_until: f64::NEG_INFINITY,
            shake_power: 0,
            flash_at: f64::NEG_INFINITY,
            last_kind: None,
            last_update: 0.0,
            counts: [0; 4],
            dirs_deleted: 0,
            unlocked: Vec::new(),
        }
    }

    fn total(&self) -> u64 {
        self.counts.iter().sum()
    }

    /// Score needed to reach `level`.
    pub fn threshold(level: u32) -> u64 {
        let n = level.saturating_sub(1) as u64;
        1500 * n * n
    }

    pub fn points(kind: ChangeKind, is_dir: bool) -> u64 {
        match (kind, is_dir) {
            (ChangeKind::Create, false) => 100,
            (ChangeKind::Create, true) => 200,
            (ChangeKind::Modify, _) => 25,
            (ChangeKind::Delete, false) => 75,
            (ChangeKind::Delete, true) => 300,
            (ChangeKind::Rename, _) => 150,
        }
    }

    pub fn multiplier(&self) -> f64 {
        1.0 + self.combo.min(90) as f64 / 10.0
    }

    pub fn on_change(&mut self, c: &Change, now: f64) {
        if !self.enabled {
            return;
        }
        if now - self.last_event > COMBO_WINDOW {
            self.combo = 0;
        }
        self.combo += 1;
        self.best_combo = self.best_combo.max(self.combo);
        self.last_event = now;
        self.last_kind = Some((c.kind, now));
        let slot = match c.kind {
            ChangeKind::Create => 0,
            ChangeKind::Modify => 1,
            ChangeKind::Delete => 2,
            ChangeKind::Rename => 3,
        };
        self.counts[slot] += 1;

        let points = (Self::points(c.kind, c.is_dir) as f64 * self.multiplier()) as u64;
        self.score += points;
        if self.pending.len() < MAX_PENDING {
            self.pending.push(Spawn { path: c.path.clone(), kind: c.kind, is_dir: c.is_dir, points });
        }

        if c.kind == ChangeKind::Delete && c.is_dir {
            self.dirs_deleted += 1;
            self.shake(2, 0.45, now);
        }
        if let Some((_, word)) = TIERS.iter().find(|(n, _)| *n == self.combo) {
            self.banner =
                Some(Banner { text: word.to_string(), sub: format!("{}x COMBO", self.combo), born: now });
        }
        while self.score >= Self::threshold(self.level + 1) {
            self.level += 1;
            self.banner =
                Some(Banner { text: "LEVEL UP!".into(), sub: format!("Lv.{}", self.level), born: now });
            self.flash_at = now;
            self.fireworks += 4;
        }
        for a in ACHIEVEMENTS {
            if !self.unlocked.contains(&a.id) && (a.check)(self) {
                self.unlocked.push(a.id);
                self.toasts.push_back(Toast { title: a.title, desc: a.desc, born: None });
            }
        }
    }

    /// Called once per delivered batch.
    pub fn on_batch(&mut self, size: usize, now: f64) {
        if self.enabled && size >= 12 {
            self.shake(1, 0.3, now);
        }
    }

    fn shake(&mut self, power: i16, secs: f64, now: f64) {
        self.shake_power = self.shake_power.max(power);
        if now >= self.shake_until {
            self.shake_power = power;
        }
        self.shake_until = self.shake_until.max(now + secs);
    }

    /// Horizontal offset for screen shake.
    pub fn shake_offset(&self, now: f64) -> i16 {
        if !self.enabled || now >= self.shake_until {
            return 0;
        }
        if (now * 40.0) as i64 % 2 == 0 {
            self.shake_power
        } else {
            -self.shake_power
        }
    }

    pub fn take_pending(&mut self, max: usize) -> Vec<Spawn> {
        let n = self.pending.len().min(max);
        let out: Vec<Spawn> = self.pending.drain(..n).collect();
        // Anything beyond `max` in one frame is a flood; drop it rather than lag behind.
        self.pending.clear();
        out
    }

    /// Advances the simulation to `now`.
    pub fn update(&mut self, now: f64) {
        let dt = (now - self.last_update).clamp(0.0, 0.1);
        self.last_update = now;
        for p in &mut self.particles {
            p.vy += p.gravity * dt;
            p.x += p.vx * dt;
            p.y += p.vy * dt;
            p.vx *= 1.0 - 1.2 * dt;
        }
        self.particles.retain(|p| now - p.born < p.life);
        self.popups.retain(|p| now - p.born < 1.0);
        self.sweeps.retain(|s| now - s.born < 0.5);
        if self.banner.as_ref().is_some_and(|b| now - b.born > BANNER_LIFE) {
            self.banner = None;
        }
        if let Some(t) = self.toasts.front_mut() {
            let born = *t.born.get_or_insert(now);
            if now - born > TOAST_LIFE {
                self.toasts.pop_front();
            }
        }
        if now - self.last_event > COMBO_WINDOW {
            self.combo = 0;
        }
        let gap = self.score as f64 - self.shown_score;
        self.shown_score =
            if gap < 1.0 { self.score as f64 } else { self.shown_score + gap * (dt * 9.0).min(1.0) };
    }

    pub fn is_animating(&self, now: f64) -> bool {
        self.enabled
            && (!self.particles.is_empty()
                || !self.popups.is_empty()
                || !self.sweeps.is_empty()
                || !self.pending.is_empty()
                || self.fireworks > 0
                || self.banner.is_some()
                || !self.toasts.is_empty()
                || self.combo > 0
                || (self.shown_score as u64) < self.score
                || now < self.shake_until)
    }

    pub fn shown_score(&self) -> u64 {
        self.shown_score as u64
    }

    /// Remaining combo time as 0..1.
    pub fn combo_left(&self, now: f64) -> f64 {
        if self.combo == 0 {
            0.0
        } else {
            (1.0 - (now - self.last_event) / COMBO_WINDOW).clamp(0.0, 1.0)
        }
    }

    pub fn fever(&self) -> bool {
        self.enabled && self.combo >= FEVER_COMBO
    }

    /// Panel border color override: flashes, combo glow, FEVER rainbow.
    pub fn border_color(&self, theme: &Theme, now: f64) -> Option<Rgb> {
        if !self.enabled {
            return None;
        }
        if now - self.flash_at < 0.2 {
            return Some(palette::FLASH);
        }
        if self.fever() {
            return Some(hue(now * 0.6));
        }
        let (kind, at) = self.last_kind?;
        let g = theme.glow((now - at) * 2.0);
        (g > 0.0).then(|| Theme::kind_rgb(kind).mix(palette::BORDER, g * 0.8))
    }

    // ----- spawning (screen coordinates) -----

    /// Sparks flying out of a point, e.g. a lit grid cell.
    pub fn burst(&mut self, x: u16, y: u16, kind: ChangeKind, now: f64, count: usize) {
        let color = Theme::kind_rgb(kind);
        for _ in 0..count {
            let angle = self.rng.below(360) as f64 * std::f64::consts::PI / 180.0;
            let speed = 4.0 + self.rng.below(100) as f64 / 100.0 * 10.0;
            let life = 0.45 + self.rng.below(50) as f64 / 100.0;
            // Cells are about twice as tall as wide; stretch x so bursts look round.
            self.push(Particle {
                x: x as f64 + 0.5,
                y: y as f64 + 0.5,
                vx: angle.cos() * speed * 2.0,
                vy: angle.sin() * speed - 3.0,
                born: now,
                life,
                color,
                glyph: None,
                gravity: 14.0,
            });
        }
    }

    /// A name breaking apart letter by letter and falling (deletions).
    pub fn shatter(&mut self, x: u16, y: u16, text: &str, now: f64) {
        for (i, ch) in text.chars().enumerate().filter(|(_, c)| !c.is_whitespace()) {
            let spread = self.rng.below(100) as f64 / 100.0;
            self.push(Particle {
                x: x as f64 + i as f64,
                y: y as f64,
                vx: (spread - 0.5) * 12.0,
                vy: -3.0 - spread * 4.0,
                born: now,
                life: 0.8 + spread * 0.5,
                color: palette::DELETE,
                glyph: Some(ch),
                gravity: 22.0,
            });
        }
    }

    pub fn popup(&mut self, x: u16, y: u16, text: String, kind: ChangeKind, now: f64) {
        self.popups.push(Popup { text, x: x as f64, y: y as f64, born: now, color: Theme::kind_rgb(kind) });
        if self.popups.len() > 40 {
            self.popups.remove(0);
        }
    }

    pub fn sweep(&mut self, x: u16, y: u16, len: u16, kind: ChangeKind, now: f64) {
        self.sweeps.push(Sweep { x, y, len, born: now, color: Theme::kind_rgb(kind) });
    }

    fn push(&mut self, p: Particle) {
        if self.particles.len() >= MAX_PARTICLES {
            self.particles.remove(0);
        }
        self.particles.push(p);
    }

    // ----- drawing -----

    /// Draws everything that floats above the panels. `body` is where banners center.
    pub fn render(&mut self, screen: Rect, body: Rect, buf: &mut Buffer, theme: &Theme, now: f64) {
        if !self.enabled {
            return;
        }
        while self.fireworks > 0 {
            self.fireworks -= 1;
            let x = screen.x + 4 + self.rng.below(screen.width.saturating_sub(8).max(1) as usize) as u16;
            let y = body.y + 1 + self.rng.below((body.height / 2).max(1) as usize) as u16;
            let kind = [ChangeKind::Create, ChangeKind::Modify, ChangeKind::Rename, ChangeKind::Delete]
                [self.rng.below(4)];
            self.burst(x, y, kind, now, 36);
        }
        self.render_sweeps(buf, screen, theme, now);
        self.render_particles(buf, screen, theme, now);
        self.render_popups(buf, screen, theme, now);
        self.render_banner(buf, body, theme, now);
        self.render_toast(buf, screen, theme, now);
    }

    fn render_sweeps(&self, buf: &mut Buffer, clip: Rect, theme: &Theme, now: f64) {
        for s in &self.sweeps {
            let head = (now - s.born) / 0.4 * (s.len as f64 + 4.0);
            for i in 0..s.len {
                let d = head - i as f64;
                if !(0.0..4.0).contains(&d) {
                    continue;
                }
                let (x, y) = (s.x + i, s.y);
                if !contains(clip, x, y) {
                    continue;
                }
                let c = palette::FLASH.mix(s.color, 1.0 - d / 4.0);
                buf[(x, y)].set_style(theme.fg(c).add_modifier(Modifier::BOLD));
            }
        }
    }

    fn render_particles(&self, buf: &mut Buffer, clip: Rect, theme: &Theme, now: f64) {
        const SPARKS: [&str; 5] = ["✦", "✧", "*", "+", "·"];
        const SPARKS_ASCII: [&str; 5] = ["*", "*", "+", "+", "."];
        for p in &self.particles {
            let (x, y) = (p.x.round(), p.y.round());
            if x < 0.0 || y < 0.0 {
                continue;
            }
            let (x, y) = (x as u16, y as u16);
            if !contains(clip, x, y) {
                continue;
            }
            let t = ((now - p.born) / p.life).clamp(0.0, 1.0);
            let color = if t < 0.12 {
                palette::FLASH.mix(p.color, 1.0 - t / 0.12)
            } else {
                p.color.mix(palette::FAINT, 1.0 - t)
            };
            let cell = &mut buf[(x, y)];
            // Sparks fly around text instead of erasing it; falling letters go anywhere.
            if p.glyph.is_none() && !is_background(cell.symbol()) {
                continue;
            }
            match p.glyph {
                Some(ch) => {
                    let mut tmp = [0u8; 4];
                    cell.set_symbol(ch.encode_utf8(&mut tmp));
                }
                None => {
                    let i = ((t * 5.0) as usize).min(4);
                    cell.set_symbol(if theme.ascii { SPARKS_ASCII[i] } else { SPARKS[i] });
                }
            }
            cell.set_style(theme.fg(color).add_modifier(if t < 0.5 {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }));
        }
    }

    fn render_popups(&self, buf: &mut Buffer, clip: Rect, theme: &Theme, now: f64) {
        for p in &self.popups {
            let t = (now - p.born).clamp(0.0, 1.0);
            let y = p.y - t * 2.5;
            if y < clip.y as f64 {
                continue;
            }
            let color = if t < 0.1 { palette::FLASH } else { p.color.mix(palette::FAINT, 1.0 - t * t) };
            let style = theme.fg(color).add_modifier(Modifier::BOLD);
            put(buf, clip, p.x as u16, y.round() as u16, &p.text, style);
        }
    }

    fn render_banner(&self, buf: &mut Buffer, area: Rect, theme: &Theme, now: f64) {
        let Some(b) = &self.banner else { return };
        let age = now - b.born;
        // Letters punch in one by one, then the whole thing fades.
        let shown = ((age / 0.025) as usize).min(b.text.chars().count());
        let spaced: String = b.text.chars().take(shown).flat_map(|c| [c, ' ']).collect();
        let full_w = b.text.chars().count() * 2 + 3;
        let w = full_w.max(b.sub.len() + 4) as u16;
        if area.width < w + 2 || area.height < 5 {
            return;
        }
        let x = area.x + (area.width - w) / 2;
        let rise = ((1.0 - (age / 0.2).min(1.0)) * 2.0) as u16;
        let y = area.y + area.height / 3 + rise;
        let fade = ((BANNER_LIFE - age) / 0.4).clamp(0.0, 1.0);
        let frame_color = hue(now * 1.5).mix(palette::FAINT, fade);
        let (h, v, tl, tr, bl, br) = if theme.ascii {
            ("=", "|", "+", "+", "+", "+")
        } else {
            ("═", "║", "╔", "╗", "╚", "╝")
        };
        let fs = theme.fg(frame_color).add_modifier(Modifier::BOLD);
        let inner_w = (w - 2) as usize;
        put(buf, area, x, y, &format!("{tl}{}{tr}", h.repeat(inner_w)), fs);
        for dy in 1..=2 {
            put(buf, area, x, y + dy, &format!("{v}{}{v}", " ".repeat(inner_w)), fs);
        }
        put(buf, area, x, y + 3, &format!("{bl}{}{br}", h.repeat(inner_w)), fs);
        // Rainbow letters.
        let tx = x + 1 + (inner_w as u16).saturating_sub(spaced.chars().count() as u16) / 2;
        for (i, ch) in spaced.chars().enumerate() {
            let c = hue(now * 2.0 + i as f64 * 0.06).mix(palette::FAINT, fade);
            put(buf, area, tx + i as u16, y + 1, &ch.to_string(), theme.fg(c).add_modifier(Modifier::BOLD));
        }
        let sx = x + 1 + (inner_w as u16).saturating_sub(b.sub.len() as u16) / 2;
        put(buf, area, sx, y + 2, &b.sub, theme.fg(palette::TEXT.mix(palette::FAINT, fade)));
    }

    fn render_toast(&self, buf: &mut Buffer, screen: Rect, theme: &Theme, now: f64) {
        let Some(t) = self.toasts.front() else { return };
        let age = t.born.map_or(0.0, |b| now - b);
        let star = theme.sym("★", "*");
        let line1 = format!(" {star} ACHIEVEMENT UNLOCKED ");
        let line2 = format!(" {} ", t.title);
        let line3 = format!(" {} ", t.desc);
        let w = [&line1, &line2, &line3].iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16 + 2;
        if screen.width < w + 2 || screen.height < 8 {
            return;
        }
        // Slide in from the right, slide out at the end.
        let slide = if age < 0.25 {
            1.0 - age / 0.25
        } else if age > TOAST_LIFE - 0.25 {
            (age - (TOAST_LIFE - 0.25)) / 0.25
        } else {
            0.0
        };
        let x = screen.right().saturating_sub(w + 1) + (slide * (w + 1) as f64) as u16;
        let y = screen.bottom().saturating_sub(7);
        let gold = Rgb(255, 205, 80);
        let edge = theme.fg(gold).add_modifier(Modifier::BOLD);
        let (h, v) = if theme.ascii { ("-", "|") } else { ("─", "│") };
        let (tl, tr, bl, br) = if theme.ascii { ("+", "+", "+", "+") } else { ("╭", "╮", "╰", "╯") };
        let inner = (w - 2) as usize;
        let pad = |s: &str| format!("{s}{}", " ".repeat(inner.saturating_sub(s.chars().count())));
        put(buf, screen, x, y, &format!("{tl}{}{tr}", h.repeat(inner)), edge);
        put(buf, screen, x, y + 1, v, edge);
        put(buf, screen, x + 1, y + 1, &pad(&line1), edge);
        put(buf, screen, x + w - 1, y + 1, v, edge);
        put(buf, screen, x, y + 2, v, edge);
        put(buf, screen, x + 1, y + 2, &pad(&line2), theme.fg(palette::FLASH).add_modifier(Modifier::BOLD));
        put(buf, screen, x + w - 1, y + 2, v, edge);
        put(buf, screen, x, y + 3, v, edge);
        put(buf, screen, x + 1, y + 3, &pad(&line3), theme.fg(palette::DIM));
        put(buf, screen, x + w - 1, y + 3, v, edge);
        put(buf, screen, x, y + 4, &format!("{bl}{}{br}", h.repeat(inner)), edge);
    }
}

/// Cells that carry no information: blanks and the grid's idle/empty marks.
fn is_background(symbol: &str) -> bool {
    matches!(symbol, " " | "·" | "." | ":" | "░" | "▁")
}

fn contains(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.right() && y >= r.y && y < r.bottom()
}

/// Writes `text` at (x, y), clipped to `clip`.
fn put(buf: &mut Buffer, clip: Rect, x: u16, y: u16, text: &str, style: Style) {
    if y < clip.y || y >= clip.bottom() || x >= clip.right() {
        return;
    }
    let max = (clip.right() - x) as usize;
    buf.set_stringn(x, y, text, max, style);
}

/// A bright color from the hue wheel; `h` wraps around every 1.0.
pub fn hue(h: f64) -> Rgb {
    let h = h.rem_euclid(1.0) * 6.0;
    let x = 1.0 - (h % 2.0 - 1.0).abs();
    let (r, g, b) = match h as u32 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    let f = |v: f64| (90.0 + v * 165.0) as u8;
    Rgb(f(r), f(g), f(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx() -> Fx {
        Fx::new(true, 1)
    }

    #[test]
    fn combo_builds_and_breaks() {
        let mut f = fx();
        for i in 0..10 {
            f.on_change(&Change::modify("a"), i as f64 * 0.5);
        }
        assert_eq!(f.combo, 10);
        assert!(f.banner.is_some(), "NICE! at 10");
        f.on_change(&Change::modify("a"), 100.0);
        assert_eq!(f.combo, 1);
        assert_eq!(f.best_combo, 10);
    }

    #[test]
    fn score_uses_multiplier_and_levels_up() {
        let mut f = fx();
        f.on_change(&Change::create("a", false), 0.0);
        assert_eq!(f.score, 110, "100 points at combo 1 (x1.1)");
        for i in 0..40 {
            f.on_change(&Change::create(format!("f{i}"), false), 0.1 * i as f64);
        }
        assert!(f.level >= 3, "level {} at score {}", f.level, f.score);
        assert!(f.score >= Fx::threshold(f.level));
    }

    #[test]
    fn achievements_unlock_once() {
        let mut f = fx();
        f.on_change(&Change::create("a", false), 0.0);
        f.on_change(&Change::create("b", false), 0.1);
        assert_eq!(f.toasts.len(), 1);
        f.on_change(&Change::delete("dir", true), 0.2);
        let titles: Vec<_> = f.toasts.iter().map(|t| t.title).collect();
        assert_eq!(titles, ["Seedling", "Reaper", "Demolition"]);
        assert!(f.shake_offset(0.25) != 0);
    }

    #[test]
    fn disabled_does_nothing() {
        let mut f = Fx::new(false, 1);
        f.on_change(&Change::create("a", false), 0.0);
        assert_eq!(f.score, 0);
        assert!(!f.is_animating(0.0));
    }

    #[test]
    fn particles_expire_and_rendering_stays_in_bounds() {
        let mut f = fx();
        let theme = Theme { colors: crate::ui::theme::ColorMode::TrueColor, ascii: false, animate: true };
        let area = Rect::new(0, 0, 20, 10);
        let mut buf = Buffer::empty(area);
        f.burst(0, 0, ChangeKind::Create, 0.0, 50);
        f.burst(19, 9, ChangeKind::Delete, 0.0, 50);
        f.shatter(15, 9, "a_long_file_name.rs", 0.0);
        f.popup(18, 1, "+1000".into(), ChangeKind::Create, 0.0);
        f.banner = Some(Banner { text: "UNSTOPPABLE!!!".into(), sub: "100x".into(), born: 0.0 });
        f.toasts.push_back(Toast { title: "T", desc: "D", born: None });
        for i in 0..60 {
            let now = i as f64 / 30.0;
            f.update(now);
            f.render(area, area, &mut buf, &theme, now);
        }
        f.update(5.0);
        f.update(5.05);
        assert!(f.particles.is_empty());
    }

    #[test]
    fn hue_wheel_is_bright() {
        for i in 0..12 {
            let c = hue(i as f64 / 12.0);
            assert!(c.0.max(c.1).max(c.2) >= 250);
        }
    }
}
