//! Color: two surfaces, one derived from the other.
//!
//! A theme defines only the *terminal* palette — the colors a program running
//! in the pty can address. Every chrome color is then derived from that
//! palette, so swapping a theme restyles the sidebar coherently without the
//! theme having to know the sidebar exists.
//!
//! One deliberate constraint: hue in the chrome is reserved for agent
//! identity. Derived chrome tokens are all neutral mixes of the terminal
//! background and foreground, never of an accent color, so an agent's dot is
//! the only saturated thing in the sidebar.

use std::collections::HashMap;

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor};

pub type Rgb8 = [u8; 3];

// MARK: - Color math

/// Converts one sRGB channel to linear light.
///
/// The swapchain format is `Bgra8UnormSrgb`, so wgpu applies a linear->sRGB
/// encode on whatever the fragment shader emits. glyphon's shader does this
/// conversion internally for text, so quad colors have to be linearized here
/// to keep backgrounds and glyphs in the same color space.
pub fn srgb_to_linear(c: u8) -> f32 {
    let s = c as f32 / 255.0;
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

pub fn linear_rgba(c: Rgb8, alpha: f32) -> [f32; 4] {
    [
        srgb_to_linear(c[0]),
        srgb_to_linear(c[1]),
        srgb_to_linear(c[2]),
        alpha,
    ]
}

/// WCAG relative luminance.
fn luminance(c: Rgb8) -> f32 {
    0.2126 * srgb_to_linear(c[0]) + 0.7152 * srgb_to_linear(c[1]) + 0.0722 * srgb_to_linear(c[2])
}

/// WCAG contrast ratio, always >= 1.0.
pub fn contrast_ratio(a: Rgb8, b: Rgb8) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Linear interpolation in sRGB space. Perceptually crude, but it is what
/// makes the derived tokens land on values a designer would have picked by
/// hand, which matters more here than colorimetric purity.
pub fn mix(a: Rgb8, b: Rgb8, t: f32) -> Rgb8 {
    let t = t.clamp(0.0, 1.0);
    [
        (a[0] as f32 + (b[0] as f32 - a[0] as f32) * t).round() as u8,
        (a[1] as f32 + (b[1] as f32 - a[1] as f32) * t).round() as u8,
        (a[2] as f32 + (b[2] as f32 - a[2] as f32) * t).round() as u8,
    ]
}

/// Pushes a color further from mid-gray: darker if it is already dark, lighter
/// if already light. Used so the sidebar recedes from the terminal surface in
/// both light and dark themes without branching at the call site.
fn away_from_mid(c: Rgb8, amount: f32) -> Rgb8 {
    let target = if luminance(c) < 0.5 {
        [0, 0, 0]
    } else {
        [255, 255, 255]
    };
    mix(c, target, amount)
}

// MARK: - Terminal palette

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalPalette {
    pub bg: Rgb8,
    pub fg: Rgb8,
    pub cursor: Rgb8,
    pub selection_bg: Rgb8,
    pub ansi: [Rgb8; 16],
}

impl Default for TerminalPalette {
    fn default() -> Self {
        Self {
            bg: [0x17, 0x17, 0x17],
            fg: [0xe6, 0xe6, 0xe6],
            cursor: [0xe6, 0xe6, 0xe6],
            selection_bg: [0x3a, 0x3f, 0x4b],
            ansi: [
                [0x1c, 0x1c, 0x1c],
                [0xe5, 0x5f, 0x5f],
                [0x8f, 0xc7, 0x7a],
                [0xe0, 0xb5, 0x5f],
                [0x6c, 0x9e, 0xd9],
                [0xb4, 0x8e, 0xd8],
                [0x63, 0xc4, 0xb8],
                [0xcf, 0xcf, 0xcf],
                [0x50, 0x50, 0x50],
                [0xff, 0x7b, 0x7b],
                [0xa8, 0xe0, 0x94],
                [0xff, 0xd0, 0x7b],
                [0x8a, 0xb9, 0xf0],
                [0xcf, 0xa9, 0xf0],
                [0x7e, 0xdd, 0xd1],
                [0xff, 0xff, 0xff],
            ],
        }
    }
}

// MARK: - Derived chrome tokens

/// Mix fractions for deriving chrome from the terminal palette. These values
/// reproduce the hand-picked chrome of the AppKit prototype when fed its
/// palette, which is the sanity check that the derivation is tuned right.
const SURFACE_RECESS: f32 = 0.12;
const RAISED_LIFT: f32 = 0.06;
const BORDER_LIFT: f32 = 0.12;
const SECONDARY_FADE: f32 = 0.45;
const TERTIARY_FADE: f32 = 0.62;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChromeTokens {
    /// Sidebar background.
    pub surface: Rgb8,
    /// Focused row background.
    pub surface_raised: Rgb8,
    /// Hairline between sidebar and terminal.
    pub border: Rgb8,
    pub text_primary: Rgb8,
    pub text_secondary: Rgb8,
    pub text_tertiary: Rgb8,
}

impl ChromeTokens {
    pub fn derive(palette: &TerminalPalette) -> Self {
        let surface = away_from_mid(palette.bg, SURFACE_RECESS);
        Self {
            surface,
            surface_raised: mix(surface, palette.fg, RAISED_LIFT),
            border: mix(surface, palette.fg, BORDER_LIFT),
            text_primary: palette.fg,
            text_secondary: mix(palette.fg, surface, SECONDARY_FADE),
            text_tertiary: mix(palette.fg, surface, TERTIARY_FADE),
        }
    }
}

// MARK: - Theme

pub struct Theme {
    pub palette: TerminalPalette,
    pub chrome: ChromeTokens,
    /// Minimum WCAG contrast ratio enforced between cell text and its
    /// background. 1.0 disables it. Guards against agent CLIs emitting dim
    /// grays that vanish under a theme they weren't designed for.
    pub min_contrast: f32,
    /// Cache of contrast-corrected foregrounds, keyed by the pair that needed
    /// correcting. The number of distinct pairs on screen is tiny.
    contrast_cache: HashMap<(Rgb8, Rgb8), Rgb8>,
}

impl Default for Theme {
    fn default() -> Self {
        Self::new(TerminalPalette::default(), 1.3)
    }
}

impl Theme {
    pub fn new(palette: TerminalPalette, min_contrast: f32) -> Self {
        let chrome = ChromeTokens::derive(&palette);
        Self {
            palette,
            chrome,
            min_contrast,
            contrast_cache: HashMap::new(),
        }
    }

    pub fn set_palette(&mut self, palette: TerminalPalette) {
        self.chrome = ChromeTokens::derive(&palette);
        self.palette = palette;
        self.contrast_cache.clear();
    }

    pub fn bg(&self) -> Rgb8 {
        self.palette.bg
    }

    pub fn fg(&self) -> Rgb8 {
        self.palette.fg
    }

    /// Raises `fg` away from `bg` until it clears `min_contrast`.
    ///
    /// Moves toward white or black — whichever direction actually increases
    /// contrast against this background — by bisection, which converges fast
    /// enough to be irrelevant next to the cache.
    pub fn enforce_contrast(&mut self, fg: Rgb8, bg: Rgb8) -> Rgb8 {
        if self.min_contrast <= 1.0 || contrast_ratio(fg, bg) >= self.min_contrast {
            return fg;
        }
        if let Some(fixed) = self.contrast_cache.get(&(fg, bg)) {
            return *fixed;
        }

        let target = if luminance(bg) < 0.5 {
            [255, 255, 255]
        } else {
            [0, 0, 0]
        };

        let mut lo = 0.0f32;
        let mut hi = 1.0f32;
        let mut best = target;
        for _ in 0..12 {
            let mid = (lo + hi) / 2.0;
            let candidate = mix(fg, target, mid);
            if contrast_ratio(candidate, bg) >= self.min_contrast {
                best = candidate;
                hi = mid;
            } else {
                lo = mid;
            }
        }

        self.contrast_cache.insert((fg, bg), best);
        best
    }

    /// Resolves a cell color, honoring any OSC-4/OSC-10 overrides the program
    /// running in the pty has set.
    pub fn resolve(&self, color: AnsiColor, overrides: &Colors) -> Rgb8 {
        match color {
            AnsiColor::Spec(rgb) => [rgb.r, rgb.g, rgb.b],
            AnsiColor::Named(named) => {
                if let Some(rgb) = overrides[named] {
                    return [rgb.r, rgb.g, rgb.b];
                }
                self.named(named)
            },
            AnsiColor::Indexed(idx) => {
                if let Some(rgb) = overrides[idx as usize] {
                    return [rgb.r, rgb.g, rgb.b];
                }
                self.indexed(idx)
            },
        }
    }

    fn named(&self, n: NamedColor) -> Rgb8 {
        use NamedColor as N;
        let p = &self.palette;
        let dim = |c: Rgb8| mix(c, p.bg, 0.34);
        match n {
            N::Foreground | N::BrightForeground => p.fg,
            N::DimForeground => dim(p.fg),
            N::Background => p.bg,
            N::Cursor => p.cursor,
            N::DimBlack => dim(p.ansi[0]),
            N::DimRed => dim(p.ansi[1]),
            N::DimGreen => dim(p.ansi[2]),
            N::DimYellow => dim(p.ansi[3]),
            N::DimBlue => dim(p.ansi[4]),
            N::DimMagenta => dim(p.ansi[5]),
            N::DimCyan => dim(p.ansi[6]),
            N::DimWhite => dim(p.ansi[7]),
            // Everything left is Black..=BrightWhite, whose discriminants are 0..=15.
            other => p.ansi[(other as usize).min(15)],
        }
    }

    /// The standard xterm 256-color cube.
    fn indexed(&self, idx: u8) -> Rgb8 {
        match idx {
            0..=15 => self.palette.ansi[idx as usize],
            16..=231 => {
                let i = idx as u32 - 16;
                let level = |v: u32| -> u8 {
                    if v == 0 {
                        0
                    } else {
                        (55 + 40 * v) as u8
                    }
                };
                [level(i / 36), level((i % 36) / 6), level(i % 6)]
            },
            _ => {
                let v = (8 + 10 * (idx as u32 - 232)) as u8;
                [v, v, v]
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derivation_reproduces_hand_picked_chrome() {
        // The AppKit prototype's sidebar was #141414 on a #171717 terminal
        // with a #2a2a2a border, all chosen by eye. The derivation should
        // land within a couple of levels of those.
        let chrome = ChromeTokens::derive(&TerminalPalette::default());
        assert_eq!(chrome.surface, [0x14, 0x14, 0x14]);
        assert!(
            (chrome.border[0] as i32 - 0x2a).abs() <= 4,
            "border drifted: {:?}",
            chrome.border
        );
        assert!(chrome.surface_raised > chrome.surface);
    }

    #[test]
    fn derivation_inverts_for_light_palettes() {
        let light = TerminalPalette {
            bg: [0xff, 0xff, 0xff],
            fg: [0x1a, 0x1a, 0x1a],
            ..TerminalPalette::default()
        };
        let chrome = ChromeTokens::derive(&light);
        // On a light theme the sidebar must stay light and the raised row must
        // get *darker*, not lighter.
        assert!(luminance(chrome.surface) > 0.5);
        assert!(luminance(chrome.surface_raised) < luminance(chrome.surface));
    }

    #[test]
    fn contrast_floor_rescues_unreadable_text() {
        let mut theme = Theme::new(TerminalPalette::default(), 1.3);
        let bg = [0x17, 0x17, 0x17];
        // Near-invisible dark gray on a dark background.
        let fixed = theme.enforce_contrast([0x1e, 0x1e, 0x1e], bg);
        assert!(
            contrast_ratio(fixed, bg) >= 1.3,
            "still unreadable: {fixed:?} ratio {}",
            contrast_ratio(fixed, bg)
        );
    }

    #[test]
    fn contrast_floor_leaves_readable_text_alone() {
        let mut theme = Theme::new(TerminalPalette::default(), 1.3);
        let bg = [0x17, 0x17, 0x17];
        let fg = [0xe6, 0xe6, 0xe6];
        assert_eq!(theme.enforce_contrast(fg, bg), fg);
    }

    #[test]
    fn contrast_floor_off_by_default_is_identity() {
        let mut theme = Theme::new(TerminalPalette::default(), 1.0);
        let fg = [0x1e, 0x1e, 0x1e];
        assert_eq!(theme.enforce_contrast(fg, [0x17, 0x17, 0x17]), fg);
    }
}
