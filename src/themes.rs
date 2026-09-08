//! Named palettes, plus a reader for Ghostty's theme file format.
//!
//! Only a small curated set is embedded. Everything else is expected to come
//! from disk in Ghostty's format, which is flat `key = value` text — that way
//! the hundreds of palettes already published for Ghostty work here unchanged
//! instead of this project growing a theme ecosystem of its own.

use std::path::{Path, PathBuf};

use crate::theme::{Rgb8, TerminalPalette};

pub const DEFAULT_DARK: &str = "prism";
pub const DEFAULT_LIGHT: &str = "prism-light";

pub const BUILTIN_NAMES: &[&str] = &["prism", "prism-light", "graphite"];

/// "Prism" — sampled from the app icon: near-black with a blue cast, a
/// cyan-to-violet accent ramp, and a silver cursor.
fn prism() -> TerminalPalette {
    TerminalPalette {
        bg: [0x0b, 0x0c, 0x13],
        fg: [0xe6, 0xe9, 0xf2],
        cursor: [0xf2, 0xf5, 0xff],
        selection_bg: [0x2a, 0x2f, 0x45],
        ansi: [
            [0x14, 0x17, 0x22], // black — the icon's glass interior
            [0xff, 0x6b, 0x8f], // red, pulled cool to sit with the violets
            [0x5e, 0xe6, 0xb8], // green
            [0xff, 0xd4, 0x79], // yellow
            [0x91, 0xa9, 0xff], // blue — sampled at 226deg
            [0xac, 0x8a, 0xfe], // magenta — sampled at 257deg
            [0x6f, 0xdc, 0xf7], // cyan — the chevron's bright end
            [0xc9, 0xcf, 0xdd], // white
            [0x3a, 0x40, 0x55],
            [0xff, 0x8f, 0xab],
            [0x7d, 0xf0, 0xcb],
            [0xff, 0xe3, 0xa3],
            [0xb3, 0xc4, 0xff],
            [0xc9, 0xaa, 0xff],
            [0xa8, 0xf0, 0xff],
            [0xff, 0xff, 0xff],
        ],
    }
}

/// Prism's light counterpart, for following system appearance. Same hues,
/// re-tuned for contrast on paper rather than glass.
fn prism_light() -> TerminalPalette {
    TerminalPalette {
        bg: [0xf7, 0xf8, 0xfc],
        fg: [0x1f, 0x23, 0x33],
        cursor: [0x2c, 0x33, 0x4a],
        selection_bg: [0xd4, 0xdb, 0xf0],
        ansi: [
            [0x1f, 0x23, 0x33],
            [0xc9, 0x2f, 0x5c],
            [0x11, 0x8a, 0x66],
            [0xa5, 0x6c, 0x00],
            [0x33, 0x54, 0xd6],
            [0x6b, 0x35, 0xd6],
            [0x0d, 0x7e, 0x9c],
            [0x5b, 0x62, 0x77],
            [0x4a, 0x51, 0x66],
            [0xe0, 0x4a, 0x75],
            [0x1a, 0xa8, 0x7e],
            [0xc4, 0x86, 0x0d],
            [0x4a, 0x6b, 0xf0],
            [0x84, 0x4f, 0xf0],
            [0x11, 0x99, 0xba],
            [0x2c, 0x33, 0x4a],
        ],
    }
}

/// A neutral dark palette for when Prism's blue cast is not wanted.
fn graphite() -> TerminalPalette {
    TerminalPalette::default()
}

pub fn builtin(name: &str) -> Option<TerminalPalette> {
    match name.to_ascii_lowercase().as_str() {
        "prism" => Some(prism()),
        "prism-light" => Some(prism_light()),
        "graphite" => Some(graphite()),
        _ => None,
    }
}

/// Where user themes are read from.
pub fn theme_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("shell").join("themes"))
}

/// Resolves a theme by name: built-ins first, then a file of the same name in
/// the theme directory.
pub fn resolve(name: &str) -> Option<TerminalPalette> {
    if let Some(palette) = builtin(name) {
        return Some(palette);
    }
    let path = theme_dir()?.join(name);
    load_file(&path).ok()
}

pub fn load_file(path: &Path) -> std::io::Result<TerminalPalette> {
    let text = std::fs::read_to_string(path)?;
    Ok(parse_ghostty(&text))
}

/// Parses a hex color, accepting `#rrggbb`, `rrggbb`, and `#rgb`.
fn parse_hex(value: &str) -> Option<Rgb8> {
    let v = value.trim().trim_start_matches('#');
    match v.len() {
        6 => {
            let n = u32::from_str_radix(v, 16).ok()?;
            Some([(n >> 16) as u8, (n >> 8) as u8, n as u8])
        },
        3 => {
            let n = u32::from_str_radix(v, 16).ok()?;
            // Expand each nibble, so #abc means #aabbcc.
            let (r, g, b) = ((n >> 8) & 0xf, (n >> 4) & 0xf, n & 0xf);
            Some([(r * 17) as u8, (g * 17) as u8, (b * 17) as u8])
        },
        _ => None,
    }
}

/// Reads a Ghostty theme file. Unknown keys are ignored and anything absent
/// keeps its value from the default palette, so partial themes still load.
pub fn parse_ghostty(text: &str) -> TerminalPalette {
    let mut palette = TerminalPalette::default();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();

        match key.as_str() {
            "background" => {
                if let Some(c) = parse_hex(value) {
                    palette.bg = c;
                }
            },
            "foreground" => {
                if let Some(c) = parse_hex(value) {
                    palette.fg = c;
                }
            },
            "cursor-color" => {
                if let Some(c) = parse_hex(value) {
                    palette.cursor = c;
                }
            },
            "selection-background" => {
                if let Some(c) = parse_hex(value) {
                    palette.selection_bg = c;
                }
            },
            // `palette = 4=#rrggbb`
            "palette" => {
                if let Some((index, color)) = value.split_once('=') {
                    if let (Ok(i), Some(c)) = (index.trim().parse::<usize>(), parse_hex(color)) {
                        if i < 16 {
                            palette.ansi[i] = c;
                        }
                    }
                }
            },
            _ => {},
        }
    }

    palette
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_advertised_builtin_resolves() {
        for name in BUILTIN_NAMES {
            assert!(builtin(name).is_some(), "missing builtin: {name}");
        }
        assert!(builtin(DEFAULT_DARK).is_some());
        assert!(builtin(DEFAULT_LIGHT).is_some());
    }

    #[test]
    fn builtin_lookup_is_case_insensitive() {
        assert_eq!(builtin("PRISM"), builtin("prism"));
    }

    #[test]
    fn parses_ghostty_theme_format() {
        let src = "
# a comment
background = #1d1f21
foreground = c5c8c6
cursor-color = #fff
selection-background = #373b41
palette = 0=#282a2e
palette = 4=#5f819d
palette = 15=#ffffff
unknown-key = whatever
";
        let p = parse_ghostty(src);
        assert_eq!(p.bg, [0x1d, 0x1f, 0x21]);
        // No leading '#'.
        assert_eq!(p.fg, [0xc5, 0xc8, 0xc6]);
        // Three-digit shorthand expands.
        assert_eq!(p.cursor, [0xff, 0xff, 0xff]);
        assert_eq!(p.selection_bg, [0x37, 0x3b, 0x41]);
        assert_eq!(p.ansi[0], [0x28, 0x2a, 0x2e]);
        assert_eq!(p.ansi[4], [0x5f, 0x81, 0x9d]);
        assert_eq!(p.ansi[15], [0xff, 0xff, 0xff]);
    }

    #[test]
    fn partial_theme_keeps_defaults_for_missing_keys() {
        let p = parse_ghostty("background = #000000");
        assert_eq!(p.bg, [0, 0, 0]);
        assert_eq!(p.fg, TerminalPalette::default().fg);
    }

    #[test]
    fn out_of_range_palette_index_is_ignored() {
        let p = parse_ghostty("palette = 99=#ff0000");
        assert_eq!(p.ansi, TerminalPalette::default().ansi);
    }

    #[test]
    fn prism_matches_the_icon_artwork() {
        let p = prism();
        // Sampled directly from the icon: background, and the chevron's
        // cyan and violet gradient stops.
        assert_eq!(p.bg, [0x0b, 0x0c, 0x13]);
        assert_eq!(p.ansi[5], [0xac, 0x8a, 0xfe]);
        assert_eq!(p.ansi[4], [0x91, 0xa9, 0xff]);
    }
}
