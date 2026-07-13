//! The corral tray mark, drawn in code (no asset files, no icon-theme install).
//! The logo is "the pen": a rounded-square enclosure holding three dots \u2014 the
//! same glyph vocabulary the board speaks for agent state (`\u25cf`/`\u25cb`/`\u00b7`), so the
//! icon reads as "a corral holding your agents". Calm (gray) when idle; the
//! attention variant (warm red, matching the board's Requires Action) is shown
//! by the tray when a message waits.

/// Sizes emitted so the tray host can pick a crisp one for its panel.
const SIZES: [i32; 4] = [16, 22, 32, 48];

/// The three enclosed dots, as fractions of the icon side: one on top, two
/// below (the `\u2234` arrangement the board footer mark also uses).
const DOTS: [(f32, f32); 3] = [(0.5, 0.40), (0.375, 0.61), (0.625, 0.61)];

/// The pen mark at every `SIZES` resolution, in the given color.
/// `accent` picks the warm attention red over the calm gray.
pub fn pen_pixmaps(accent: bool) -> Vec<ksni::Icon> {
    SIZES.iter().map(|&n| render(n, accent)).collect()
}

/// Whether a subpixel point lies on the mark (the rounded-square frame or any
/// of the three dots).
fn on_mark(px: f32, py: f32, n: f32) -> bool {
    let lo = n * 0.14;
    let hi = n - 1.0 - n * 0.14;
    let t = (n / 13.0).max(1.5); // frame thickness
    let rc = n * 0.24; // corner radius
    let frame = inside_rrect(px, py, lo, hi, rc)
        && !inside_rrect(px, py, lo + t, hi - t, (rc - t).max(0.0));
    let dr = n * 0.072; // dot radius
    let on_dot = DOTS.iter().any(|&(fx, fy)| {
        let (cx, cy) = (n * fx, n * fy);
        (px - cx).powi(2) + (py - cy).powi(2) <= dr * dr
    });
    frame || on_dot
}

/// Render one square icon (ARGB32, network byte order). Edges are anti-aliased
/// by 4x4 supersampled coverage, so the mark stays smooth at small sizes.
fn render(n: i32, accent: bool) -> ksni::Icon {
    let (r, g, b) = if accent {
        (0xE0, 0x52, 0x4D)
    } else {
        (0xD5, 0xD8, 0xDC)
    };
    let nf = n as f32;
    let s = 4; // supersamples per axis
    let mut data = Vec::with_capacity((n * n * 4) as usize);
    for y in 0..n {
        for x in 0..n {
            let mut cov = 0u32;
            for sy in 0..s {
                for sx in 0..s {
                    let px = x as f32 + (sx as f32 + 0.5) / s as f32;
                    let py = y as f32 + (sy as f32 + 0.5) / s as f32;
                    if on_mark(px, py, nf) {
                        cov += 1;
                    }
                }
            }
            let a = (cov * 255 / (s * s) as u32) as u8;
            // Straight (non-premultiplied) ARGB; color only where painted.
            if a > 0 {
                data.extend_from_slice(&[a, r, g, b]);
            } else {
                data.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    ksni::Icon {
        width: n,
        height: n,
        data,
    }
}

/// Whether a point is inside a rounded rectangle with bounds `[lo, hi]` on both
/// axes and corner radius `rc`. In a corner region the test falls back to the
/// distance from that corner's center; elsewhere it is the plain rectangle.
fn inside_rrect(x: f32, y: f32, lo: f32, hi: f32, rc: f32) -> bool {
    if x < lo || x > hi || y < lo || y > hi {
        return false;
    }
    let nx = x.clamp(lo + rc, hi - rc);
    let ny = y.clamp(lo + rc, hi - rc);
    let (dx, dy) = (x - nx, y - ny);
    dx * dx + dy * dy <= rc * rc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_one_icon_per_size_with_argb_data() {
        let icons = pen_pixmaps(false);
        assert_eq!(icons.len(), SIZES.len());
        for ic in &icons {
            assert_eq!(ic.data.len(), (ic.width * ic.height * 4) as usize);
            // Some pixels are painted (the mark is not empty) and some are
            // transparent (it does not fill the whole square).
            assert!(ic.data.chunks(4).any(|p| p[0] > 0));
            assert!(ic.data.chunks(4).any(|p| p[0] == 0));
        }
    }

    #[test]
    fn accent_and_calm_differ_in_color() {
        let calm = &pen_pixmaps(false)[0].data;
        let accent = &pen_pixmaps(true)[0].data;
        // Same coverage (alpha), different painted color.
        assert_ne!(calm, accent);
    }
}
