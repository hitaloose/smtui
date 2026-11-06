//! Dark theme from the prototype, with a configurable accent and a 256-color fallback.

use ratatui::style::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn parse(hex: &str) -> Option<Self> {
        let h = hex.strip_prefix('#')?;
        if h.len() != 6 {
            return None;
        }
        let v = u32::from_str_radix(h, 16).ok()?;
        Some(Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
    }

    /// Linear mix: `amount` = 0.0 gives `self`, 1.0 gives `other`.
    pub fn mix(self, other: Rgb, amount: f32) -> Rgb {
        let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * amount).round() as u8;
        Rgb(m(self.0, other.0), m(self.1, other.1), m(self.2, other.2))
    }
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub bg: Color,
    pub bar_bg: Color,
    pub raised_bg: Color,
    pub hover_bg: Color,
    pub border: Color,
    pub fg: Color,
    pub fg_muted: Color,
    pub fg_disabled: Color,
    pub fg_dimmed: Color,
    pub accent: Color,
    /// Accent blended into the background: selection and active controls.
    pub accent_tint: Color,
}

const BG: Rgb = Rgb(0x14, 0x16, 0x1a);
const BAR_BG: Rgb = Rgb(0x0f, 0x11, 0x15);
const RAISED_BG: Rgb = Rgb(0x1b, 0x1e, 0x24);
const HOVER_BG: Rgb = Rgb(0x26, 0x2a, 0x32);
const BORDER: Rgb = Rgb(0x30, 0x35, 0x3e);
const FG: Rgb = Rgb(0xe6, 0xe8, 0xeb);
const FG_MUTED: Rgb = Rgb(0x9a, 0xa3, 0xad);
const FG_DISABLED: Rgb = Rgb(0x5b, 0x63, 0x6e);
const DEFAULT_ACCENT: Rgb = Rgb(0x5e, 0xea, 0xd4);

impl Theme {
    pub fn new(accent_hex: &str, truecolor: Option<bool>) -> Self {
        let truecolor = truecolor.unwrap_or_else(detect_truecolor);
        let accent = Rgb::parse(accent_hex).unwrap_or(DEFAULT_ACCENT);
        let c = |rgb: Rgb| to_color(rgb, truecolor);
        Self {
            bg: c(BG),
            bar_bg: c(BAR_BG),
            raised_bg: c(RAISED_BG),
            hover_bg: c(HOVER_BG),
            border: c(BORDER),
            fg: c(FG),
            fg_muted: c(FG_MUTED),
            fg_disabled: c(FG_DISABLED),
            fg_dimmed: c(FG.mix(BG, 0.5)),
            accent: c(accent),
            accent_tint: c(BG.mix(accent, 0.18)),
        }
    }
}

fn detect_truecolor() -> bool {
    matches!(
        std::env::var("COLORTERM").as_deref(),
        Ok("truecolor") | Ok("24bit")
    )
}

fn to_color(rgb: Rgb, truecolor: bool) -> Color {
    if truecolor {
        Color::Rgb(rgb.0, rgb.1, rgb.2)
    } else {
        Color::Indexed(nearest_xterm256(rgb))
    }
}

/// Nearest entry in the xterm 6x6x6 color cube or the 24-step gray ramp.
pub fn nearest_xterm256(Rgb(r, g, b): Rgb) -> u8 {
    const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let idx = |v: u8| {
        STEPS
            .iter()
            .enumerate()
            .min_by_key(|(_, s)| (**s as i32 - v as i32).abs())
            .map(|(i, _)| i as u8)
            .unwrap_or(0)
    };
    let (ri, gi, bi) = (idx(r), idx(g), idx(b));
    let cube = (STEPS[ri as usize], STEPS[gi as usize], STEPS[bi as usize]);
    let dist = |a: (u8, u8, u8)| {
        let d = |x: u8, y: u8| (x as i32 - y as i32).pow(2);
        d(a.0, r) + d(a.1, g) + d(a.2, b)
    };
    let avg = (r as u32 + g as u32 + b as u32) / 3;
    let gray_i = (avg.saturating_sub(8) / 10).min(23) as u8;
    let gray_v = 8 + gray_i * 10;
    if dist((gray_v, gray_v, gray_v)) < dist(cube) {
        232 + gray_i
    } else {
        16 + 36 * ri + 6 * gi + bi
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex() {
        assert_eq!(Rgb::parse("#5eead4"), Some(Rgb(0x5e, 0xea, 0xd4)));
        assert_eq!(Rgb::parse("5eead4"), None);
    }

    #[test]
    fn maps_dark_grays_to_gray_ramp() {
        assert!(nearest_xterm256(BG) >= 232);
        assert_eq!(nearest_xterm256(Rgb(255, 0, 0)), 196);
    }
}
