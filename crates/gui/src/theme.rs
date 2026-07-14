//! base16 theming for the iced GUI (GUI-only; the TUI uses the terminal
//! palette). A `Base16` is the standard 16-slot palette (`base00..base07`
//! background→foreground ramp plus eight accents `base08..base0F`). Presets are
//! the built-in Solarized pair plus every base16 file in the themes dir; the
//! active pair (dark, light) is env-selected and the freedesktop appearance
//! portal flips between them live.
//!
//! Files use the current tinted-theming base16 spec (`name`, `variant`,
//! `palette: { base00..base0F }`), so the whole `tinted-theming/schemes`
//! gallery drops in. We parse it with a lenient flat `key: value` scan rather
//! than pull in a YAML dependency: the base16 keys are unique, so nesting does
//! not matter, and the files are machine-generated and regular.

use std::collections::HashMap;
use std::path::PathBuf;

use iced::Color;

/// A base16 palette. `base` is `base00..base07` (background → foreground ramp,
/// reversed for a light scheme); `accent` is `base08..base0F` (red, orange,
/// yellow, green, cyan, blue, violet, magenta).
#[derive(Debug, Clone)]
pub struct Base16 {
    pub name: String,
    pub base: [Color; 8],
    pub accent: [Color; 8],
}

/// Accents referenced by role so cards/badges never hardcode indices.
impl Base16 {
    pub const RED: usize = 0; // base08 — attention
    pub const GREEN: usize = 3; // base0B — running
    pub const BLUE: usize = 5; // base0D — selection / links
}

/// Parse a `#`-optional 6-digit hex color.
fn parse_hex(s: &str) -> Option<Color> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(s, 16).ok()?;
    Some(Color::from_rgb8((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

/// Parse a tinted-theming base16 scheme. Returns `None` unless all sixteen
/// `base0X` colors are present and valid. The `variant` field is not read: the
/// active pair is chosen by slug, not by a scheme's self-declared variant.
pub fn parse(src: &str) -> Option<Base16> {
    let mut kv: HashMap<String, String> = HashMap::new();
    for line in src.lines() {
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim().trim_matches(['"', '\'']).trim().to_string();
            if !v.is_empty() {
                kv.insert(k.trim().to_string(), v);
            }
        }
    }
    // base16 slots are base00..base0F (`base0` + one hex digit).
    let color = |i: usize| kv.get(&format!("base0{i:X}")).and_then(|s| parse_hex(s));
    let mut base = [Color::BLACK; 8];
    let mut accent = [Color::BLACK; 8];
    for i in 0..8 {
        base[i] = color(i)?;
        accent[i] = color(i + 8)?;
    }
    let name = kv.get("name").cloned().unwrap_or_else(|| "unnamed".into());
    Some(Base16 { name, base, accent })
}

const fn rgb(hex: u32) -> Color {
    Color::from_rgb(
        ((hex >> 16) & 0xff) as f32 / 255.0,
        ((hex >> 8) & 0xff) as f32 / 255.0,
        (hex & 0xff) as f32 / 255.0,
    )
}

/// The Solarized accents (shared between the dark and light variants).
const SOLARIZED_ACCENT: [Color; 8] = [
    rgb(0xdc322f), // base08 red
    rgb(0xcb4b16), // base09 orange
    rgb(0xb58900), // base0A yellow
    rgb(0x859900), // base0B green
    rgb(0x2aa198), // base0C cyan
    rgb(0x268bd2), // base0D blue
    rgb(0x6c71c4), // base0E violet
    rgb(0xd33682), // base0F magenta
];

fn solarized_dark() -> Base16 {
    Base16 {
        name: "Solarized Dark".into(),
        base: [
            rgb(0x002b36),
            rgb(0x073642),
            rgb(0x586e75),
            rgb(0x657b83),
            rgb(0x839496),
            rgb(0x93a1a1),
            rgb(0xeee8d5),
            rgb(0xfdf6e3),
        ],
        accent: SOLARIZED_ACCENT,
    }
}

fn solarized_light() -> Base16 {
    Base16 {
        name: "Solarized Light".into(),
        // The Solarized Dark ramp reversed.
        base: [
            rgb(0xfdf6e3),
            rgb(0xeee8d5),
            rgb(0x93a1a1),
            rgb(0x839496),
            rgb(0x657b83),
            rgb(0x586e75),
            rgb(0x073642),
            rgb(0x002b36),
        ],
        accent: SOLARIZED_ACCENT,
    }
}

/// The themes dir: `$CORRAL_THEME_DIR`, else `$XDG_CONFIG_HOME/corral/themes`,
/// else `$HOME/.config/corral/themes`.
fn themes_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("CORRAL_THEME_DIR") {
        return Some(PathBuf::from(d));
    }
    let cfg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(cfg.join("corral").join("themes"))
}

/// All presets keyed by slug: the built-in Solarized pair plus every
/// `*.yaml`/`*.yml` in the themes dir (a file overrides a built-in of the same
/// slug — the file's stem). Malformed files are ignored (never fail loud here;
/// a bad theme file must not stop the board from drawing).
pub fn presets() -> HashMap<String, Base16> {
    let mut m = HashMap::new();
    m.insert("solarized-dark".to_string(), solarized_dark());
    m.insert("solarized-light".to_string(), solarized_light());
    if let Some(dir) = themes_dir() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let p = e.path();
                let is_yaml =
                    matches!(p.extension().and_then(|x| x.to_str()), Some("yaml" | "yml"));
                if !is_yaml {
                    continue;
                }
                if let (Some(stem), Ok(src)) = (
                    p.file_stem().and_then(|s| s.to_str()).map(str::to_string),
                    std::fs::read_to_string(&p),
                ) {
                    if let Some(b) = parse(&src) {
                        m.insert(stem, b);
                    }
                }
            }
        }
    }
    m
}

/// The env-selected (dark, light) pair, falling back to the Solarized built-ins
/// when the named preset is missing. `CORRAL_THEME_DARK` / `CORRAL_THEME_LIGHT`
/// name a preset slug; the portal picks which one is shown per frame.
pub fn selected_pair() -> (Base16, Base16) {
    let presets = presets();
    let pick = |var: &str, default_slug: &str, fallback: fn() -> Base16| {
        let slug = std::env::var(var).unwrap_or_else(|_| default_slug.to_string());
        presets.get(&slug).cloned().unwrap_or_else(fallback)
    };
    (
        pick("CORRAL_THEME_DARK", "solarized-dark", solarized_dark),
        pick("CORRAL_THEME_LIGHT", "solarized-light", solarized_light),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r##"
system: "base16"
name: "Test Scheme"
author: "nobody"
variant: "dark"
palette:
  base00: "1a1a1a"
  base01: "#282828"
  base02: "383838"
  base03: "484848"
  base04: "585858"
  base05: "d8d8d8"
  base06: "e8e8e8"
  base07: "f8f8f8"
  base08: "ab4642"
  base09: "dc9656"
  base0A: "f7ca88"
  base0B: "a1b56c"
  base0C: "86c1b9"
  base0D: "7cafc2"
  base0E: "ba8baf"
  base0F: "a16946"
"##;

    #[test]
    fn parses_tinted_theming_spec() {
        let b = parse(SAMPLE).expect("parse");
        assert_eq!(b.name, "Test Scheme");
        assert_eq!(b.base[0], Color::from_rgb8(0x1a, 0x1a, 0x1a));
        // A leading '#' on the value is tolerated.
        assert_eq!(b.base[1], Color::from_rgb8(0x28, 0x28, 0x28));
        assert_eq!(b.accent[Base16::RED], Color::from_rgb8(0xab, 0x46, 0x42));
        assert_eq!(b.accent[Base16::BLUE], Color::from_rgb8(0x7c, 0xaf, 0xc2));
    }

    #[test]
    fn missing_slot_fails() {
        let truncated = SAMPLE.replace("  base0F: \"a16946\"\n", "");
        assert!(parse(&truncated).is_none());
    }

    #[test]
    fn presets_include_the_solarized_pair() {
        let p = presets();
        assert!(p.contains_key("solarized-dark"));
        assert!(p.contains_key("solarized-light"));
        assert_eq!(p["solarized-dark"].name, "Solarized Dark");
    }

    #[test]
    fn solarized_ramp_reverses_and_shares_accents() {
        let d = solarized_dark();
        let l = solarized_light();
        assert_eq!(d.name, "Solarized Dark");
        assert_eq!(d.base[0], l.base[7]);
        assert_eq!(d.base[7], l.base[0]);
        assert_eq!(d.accent, l.accent);
    }
}
