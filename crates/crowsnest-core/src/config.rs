//! Configuration and theming.
//!
//! Colours are authored as 24-bit RGB and quantised at render time to whatever
//! the terminal actually supports. That matters for the SSH-first requirement
//! in DESIGN.md §1: WezTerm and Windows Terminal both do truecolor, but a bare
//! `TERM=xterm` over SSH does not, and banding is not an acceptable default.

use std::path::PathBuf;

use ratatui::style::Color;
use serde::{Deserialize, Serialize};

/// What the terminal can render. Detected once at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorDepth {
    TrueColor,
    Indexed256,
    /// 8/16 colours. Rare, but SSH into old systems finds it.
    Ansi16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Width of the tree pane as a percentage of the terminal.
    pub tree_width_percent: u16,
    /// Columns a tab expands to when rendering.
    pub tab_width: usize,
    /// Files above this size render a placeholder instead of loading.
    pub max_file_bytes: u64,
    pub theme: Theme,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            tree_width_percent: 30,
            tab_width: 4,
            max_file_bytes: 8 * 1024 * 1024,
            theme: Theme::default(),
        }
    }
}

impl Config {
    /// Load from disk, falling back to defaults.
    ///
    /// Returns the config plus a warning if a file existed but could not be
    /// parsed — silently ignoring a broken config is worse than saying so.
    pub fn load() -> (Self, Option<String>) {
        let Some(path) = Self::path() else {
            return (Self::default(), None);
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return (Self::default(), None);
        };
        match toml::from_str(&text) {
            Ok(cfg) => (cfg, None),
            Err(e) => (
                Self::default(),
                Some(format!("{}: {e}; using defaults", path.display())),
            ),
        }
    }

    /// Config location, without pulling in a directories crate for one rule.
    pub fn path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("crowsnest").join("config.toml"))
    }

    pub fn tree_width_percent(&self) -> u16 {
        self.tree_width_percent.clamp(10, 80)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Theme {
    pub fg: Rgb,
    pub bg: Rgb,
    pub dim: Rgb,
    pub accent: Rgb,
    pub directory: Rgb,
    pub selection_bg: Rgb,
    pub gutter: Rgb,
    pub border: Rgb,
    pub border_focused: Rgb,
    pub warning: Rgb,
    // --- diff -----------------------------------------------------------
    pub added: Rgb,
    pub removed: Rgb,
    /// Subtle backgrounds behind changed lines, so a diff still reads when
    /// syntax highlighting arrives in Phase 3 and owns the foreground.
    pub added_bg: Rgb,
    pub removed_bg: Rgb,
    pub hunk_header: Rgb,
    // --- syntax ---------------------------------------------------------
    pub syn_keyword: Rgb,
    pub syn_string: Rgb,
    pub syn_comment: Rgb,
    pub syn_function: Rgb,
    pub syn_type: Rgb,
    pub syn_number: Rgb,
    pub syn_constant: Rgb,
    pub syn_operator: Rgb,
    pub syn_punctuation: Rgb,
    pub syn_variable: Rgb,
    pub syn_attribute: Rgb,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            fg: Rgb(0xc8, 0xcc, 0xd4),
            bg: Rgb(0x14, 0x17, 0x1c),
            dim: Rgb(0x6b, 0x73, 0x84),
            accent: Rgb(0x7a, 0xa2, 0xf7),
            directory: Rgb(0x7d, 0xcf, 0xff),
            selection_bg: Rgb(0x28, 0x2e, 0x3a),
            gutter: Rgb(0x4a, 0x52, 0x63),
            border: Rgb(0x2a, 0x30, 0x3c),
            border_focused: Rgb(0x7a, 0xa2, 0xf7),
            warning: Rgb(0xe0, 0xaf, 0x68),
            added: Rgb(0x9e, 0xce, 0x6a),
            removed: Rgb(0xf7, 0x76, 0x8e),
            added_bg: Rgb(0x1c, 0x28, 0x1c),
            removed_bg: Rgb(0x2c, 0x1c, 0x22),
            hunk_header: Rgb(0x7d, 0xcf, 0xff),
            syn_keyword: Rgb(0xbb, 0x9a, 0xf7),
            syn_string: Rgb(0x9e, 0xce, 0x6a),
            syn_comment: Rgb(0x56, 0x5f, 0x89),
            syn_function: Rgb(0x7a, 0xa2, 0xf7),
            syn_type: Rgb(0x2a, 0xc3, 0xde),
            syn_number: Rgb(0xff, 0x9e, 0x64),
            syn_constant: Rgb(0xff, 0x9e, 0x64),
            syn_operator: Rgb(0x89, 0xdd, 0xff),
            syn_punctuation: Rgb(0x9a, 0xa5, 0xce),
            syn_variable: Rgb(0xc0, 0xca, 0xf5),
            syn_attribute: Rgb(0xe0, 0xaf, 0x68),
        }
    }
}

/// A 24-bit colour, serialised as `"#rrggbb"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Convert for the terminal at hand.
    pub fn to_color(self, depth: ColorDepth) -> Color {
        let Rgb(r, g, b) = self;
        match depth {
            ColorDepth::TrueColor => Color::Rgb(r, g, b),
            ColorDepth::Indexed256 => Color::Indexed(self.to_xterm256()),
            ColorDepth::Ansi16 => Color::Indexed(self.to_ansi16()),
        }
    }

    /// Quantise into the xterm 256 palette: a 6×6×6 cube plus a 24-step grey
    /// ramp. Near-grey colours land on the ramp, which is visibly better than
    /// forcing them into the cube.
    pub fn to_xterm256(self) -> u8 {
        let Rgb(r, g, b) = self;
        let (rf, gf, bf) = (r as i32, g as i32, b as i32);

        let max = rf.max(gf).max(bf);
        let min = rf.min(gf).min(bf);
        if max - min < 10 {
            let level = (rf + gf + bf) / 3;
            if level < 8 {
                return 16;
            }
            if level > 248 {
                return 231;
            }
            return (232 + (level - 8) * 24 / 240) as u8;
        }

        let q = |v: i32| -> i32 {
            // Cube steps are 0, 95, 135, 175, 215, 255 — not evenly spaced.
            const STEPS: [i32; 6] = [0, 95, 135, 175, 215, 255];
            let mut best = 0;
            let mut best_d = i32::MAX;
            for (i, s) in STEPS.iter().enumerate() {
                let d = (v - s).abs();
                if d < best_d {
                    best_d = d;
                    best = i as i32;
                }
            }
            best
        };
        (16 + 36 * q(rf) + 6 * q(gf) + q(bf)) as u8
    }

    /// Collapse to the basic 16. Crude by necessity.
    pub fn to_ansi16(self) -> u8 {
        let Rgb(r, g, b) = self;
        let bright = u16::from(r) + u16::from(g) + u16::from(b) > 380;
        let bit = |v: u8| u8::from(v > 110);
        let base = bit(r) | (bit(g) << 1) | (bit(b) << 2);
        if bright {
            base + 8
        } else {
            base
        }
    }
}

impl Serialize for Rgb {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2))
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let s = String::deserialize(d)?;
        let hex = s.strip_prefix('#').unwrap_or(&s);
        if hex.len() != 6 {
            return Err(D::Error::custom(format!(
                "expected a colour like \"#7aa2f7\", got {s:?}"
            )));
        }
        let parse = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).map_err(D::Error::custom);
        Ok(Rgb(parse(0)?, parse(2)?, parse(4)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truecolor_passes_rgb_through_untouched() {
        let c = Rgb(0x7a, 0xa2, 0xf7).to_color(ColorDepth::TrueColor);
        assert_eq!(c, Color::Rgb(0x7a, 0xa2, 0xf7));
    }

    #[test]
    fn near_grey_lands_on_the_grey_ramp() {
        // The ramp is 232..=255; the cube would give a visibly tinted result.
        let idx = Rgb(0x80, 0x82, 0x81).to_xterm256();
        assert!((232..=255).contains(&idx), "expected grey ramp, got {idx}");
    }

    #[test]
    fn saturated_colours_land_in_the_cube() {
        let idx = Rgb(0xff, 0x00, 0x00).to_xterm256();
        assert_eq!(idx, 196, "pure red is cube index 196");
    }

    #[test]
    fn black_and_white_clamp_to_the_ends() {
        assert_eq!(Rgb(0, 0, 0).to_xterm256(), 16);
        assert_eq!(Rgb(255, 255, 255).to_xterm256(), 231);
    }

    #[test]
    fn colours_round_trip_through_toml() {
        let theme = Theme::default();
        let text = toml::to_string(&theme).unwrap();
        assert!(text.contains("#7aa2f7"), "serialises as hex: {text}");

        let back: Theme = toml::from_str(&text).unwrap();
        assert_eq!(back.accent, theme.accent);
    }

    #[test]
    fn a_bad_colour_is_rejected_with_a_useful_message() {
        let err = toml::from_str::<Theme>(r#"accent = "blue""#).unwrap_err();
        assert!(err.to_string().contains("#7aa2f7"), "got: {err}");
    }

    #[test]
    fn partial_config_keeps_defaults_for_the_rest() {
        let cfg: Config = toml::from_str("tab_width = 2").unwrap();
        assert_eq!(cfg.tab_width, 2);
        assert_eq!(cfg.tree_width_percent, 30);
    }

    #[test]
    fn tree_width_is_clamped_to_something_usable() {
        let cfg = Config {
            tree_width_percent: 99,
            ..Config::default()
        };
        assert_eq!(cfg.tree_width_percent(), 80);
    }
}
