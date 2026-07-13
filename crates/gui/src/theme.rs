//! base16 theming for the GUI. A `Base16` is the standard 16-slot palette
//! (a `base00..base07` background→foreground ramp plus eight accents
//! `base08..base0F`); `visuals` maps one onto egui. Shipping Solarized Dark and
//! Light means two schemes cover both system appearances, and any other base16
//! scheme drops in by defining its 16 colors.

use egui::{Color32, Stroke, Visuals};

/// A base16 palette. Slots follow the base16 convention: `base00` darkest
/// background … `base07` lightest foreground (reversed for a light scheme),
/// then `base08..base0F` are red, orange, yellow, green, cyan, blue, violet,
/// magenta.
#[derive(Debug, Clone, Copy)]
pub struct Base16 {
    pub base: [Color32; 8],   // base00..base07 (bg -> fg ramp)
    pub accent: [Color32; 8], // base08..base0F
}

/// Which accents the UI reaches for by name, so cards/badges do not hardcode
/// indices. Attention = red, running = green, links/selection = blue. RED and
/// GREEN are consumed by the card badges once the dashboard lands.
impl Base16 {
    #[allow(dead_code)]
    pub const RED: usize = 0; // base08
    #[allow(dead_code)]
    pub const GREEN: usize = 3; // base0B
    pub const BLUE: usize = 5; // base0D
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// The Solarized accents (identical between the dark and light variants).
const SOLARIZED_ACCENT: [Color32; 8] = [
    rgb(0xdc322f), // base08 red
    rgb(0xcb4b16), // base09 orange
    rgb(0xb58900), // base0A yellow
    rgb(0x859900), // base0B green
    rgb(0x2aa198), // base0C cyan
    rgb(0x268bd2), // base0D blue
    rgb(0x6c71c4), // base0E violet
    rgb(0xd33682), // base0F magenta
];

/// Solarized Dark (base00 darkest).
pub const SOLARIZED_DARK: Base16 = Base16 {
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
};

/// Solarized Light (base00 lightest): the ramp is Solarized Dark reversed.
pub const SOLARIZED_LIGHT: Base16 = Base16 {
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
};

/// The scheme for a given system appearance. (Used once the dashboard picks a
/// scheme dynamically; the spike sets both variants up front.)
#[allow(dead_code)]
pub fn scheme(dark: bool) -> Base16 {
    if dark {
        SOLARIZED_DARK
    } else {
        SOLARIZED_LIGHT
    }
}

/// Map a base16 palette onto egui `Visuals`. Backgrounds walk up the base ramp
/// (base00 window, base01 panels/widgets, base02 selection/hover), text uses
/// base05, and blue (base0D) is the accent for selection and links.
pub fn visuals(p: &Base16, dark: bool) -> Visuals {
    let mut v = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    let (bg0, bg1, bg2, dim, _fg_dark, fg, _fg1, fg_bright) = (
        p.base[0], p.base[1], p.base[2], p.base[3], p.base[4], p.base[5], p.base[6], p.base[7],
    );
    let blue = p.accent[Base16::BLUE];

    v.panel_fill = bg0;
    v.window_fill = bg0;
    v.extreme_bg_color = bg0;
    v.faint_bg_color = bg1;
    v.window_stroke = Stroke::new(1.0, bg2);
    v.override_text_color = Some(fg);
    v.hyperlink_color = blue;

    v.widgets.noninteractive.bg_fill = bg0;
    v.widgets.noninteractive.weak_bg_fill = bg0;
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, dim);

    v.widgets.inactive.bg_fill = bg1;
    v.widgets.inactive.weak_bg_fill = bg1;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, fg);

    v.widgets.hovered.bg_fill = bg2;
    v.widgets.hovered.weak_bg_fill = bg2;
    v.widgets.hovered.fg_stroke = Stroke::new(1.5, fg_bright);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, blue);

    v.widgets.active.bg_fill = bg2;
    v.widgets.active.weak_bg_fill = bg2;
    v.widgets.active.fg_stroke = Stroke::new(2.0, fg_bright);

    v.selection.bg_fill = blue.linear_multiply(0.4);
    v.selection.stroke = Stroke::new(1.0, blue);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_and_light_swap_the_ramp_but_share_accents() {
        assert_eq!(SOLARIZED_DARK.base[0], SOLARIZED_LIGHT.base[7]);
        assert_eq!(SOLARIZED_DARK.base[7], SOLARIZED_LIGHT.base[0]);
        assert_eq!(SOLARIZED_DARK.accent, SOLARIZED_LIGHT.accent);
    }

    #[test]
    fn visuals_use_the_palette_background() {
        let v = visuals(&SOLARIZED_DARK, true);
        assert_eq!(v.panel_fill, SOLARIZED_DARK.base[0]);
        assert_eq!(v.override_text_color, Some(SOLARIZED_DARK.base[5]));
    }
}
