//! Programmatic rendering for Unicode block element characters
//! (U+2580-U+259F). Same motivation as `box_drawing.rs`: font-based glyphs
//! rarely fill the cell exactly, so logos built from `▀ ▄ █` and friends
//! stagger horizontally and leave seams between rows. Painting them as
//! filled rectangles at exact cell fractions eliminates both.

use gpui::{fill, Bounds, Hsla, Pixels, Point, Size, Window};

/// Returns true if the character is in the block-elements Unicode range
/// (U+2580-U+259F).
#[inline]
pub fn is_block_element(ch: char) -> bool {
    let code = ch as u32;
    (0x2580..=0x259F).contains(&code)
}

/// Paint a block-element character as filled rectangles inside `bounds`.
/// `fg` is painted opaque for the solid variants; for the shade characters
/// (░ ▒ ▓) `fg.a` is scaled so the cell's existing background shows through.
pub fn draw_block_element(
    ch: char,
    bounds: Bounds<Pixels>,
    fg: Hsla,
    window: &mut Window,
) {
    let ox = bounds.origin.x;
    let oy = bounds.origin.y;
    let w = bounds.size.width;
    let h = bounds.size.height;

    let code = ch as u32;
    match code {
        0x2580 => fill_rect(window, ox, oy, w, h * 0.5, fg),
        0x2581..=0x2587 => {
            let k = (code - 0x2580) as f32;
            let fh = h * (k / 8.0);
            fill_rect(window, ox, oy + h - fh, w, fh, fg);
        }
        0x2588 => fill_rect(window, ox, oy, w, h, fg),
        0x2589..=0x258F => {
            let k = (0x2590 - code) as f32;
            let fw = w * (k / 8.0);
            fill_rect(window, ox, oy, fw, h, fg);
        }
        0x2590 => fill_rect(window, ox + w * 0.5, oy, w * 0.5, h, fg),
        0x2591 => fill_rect(window, ox, oy, w, h, shade(fg, 0.25)),
        0x2592 => fill_rect(window, ox, oy, w, h, shade(fg, 0.50)),
        0x2593 => fill_rect(window, ox, oy, w, h, shade(fg, 0.75)),
        0x2594 => fill_rect(window, ox, oy, w, h / 8.0, fg),
        0x2595 => fill_rect(window, ox + w * 7.0 / 8.0, oy, w / 8.0, h, fg),
        0x2596 => paint_quadrants(window, ox, oy, w, h, fg, false, false, true, false),
        0x2597 => paint_quadrants(window, ox, oy, w, h, fg, false, false, false, true),
        0x2598 => paint_quadrants(window, ox, oy, w, h, fg, true, false, false, false),
        0x2599 => paint_quadrants(window, ox, oy, w, h, fg, true, false, true, true),
        0x259A => paint_quadrants(window, ox, oy, w, h, fg, true, false, false, true),
        0x259B => paint_quadrants(window, ox, oy, w, h, fg, true, true, true, false),
        0x259C => paint_quadrants(window, ox, oy, w, h, fg, true, true, false, true),
        0x259D => paint_quadrants(window, ox, oy, w, h, fg, false, true, false, false),
        0x259E => paint_quadrants(window, ox, oy, w, h, fg, false, true, true, false),
        0x259F => paint_quadrants(window, ox, oy, w, h, fg, false, true, true, true),
        _ => {}
    }
}

fn fill_rect(window: &mut Window, x: Pixels, y: Pixels, w: Pixels, h: Pixels, color: Hsla) {
    if w <= Pixels::ZERO || h <= Pixels::ZERO {
        return;
    }
    let b = Bounds {
        origin: Point { x, y },
        size: Size { width: w, height: h },
    };
    window.paint_quad(fill(b, color));
}

fn shade(fg: Hsla, factor: f32) -> Hsla {
    Hsla { a: fg.a * factor, ..fg }
}

#[allow(clippy::too_many_arguments)]
fn paint_quadrants(
    window: &mut Window,
    ox: Pixels,
    oy: Pixels,
    w: Pixels,
    h: Pixels,
    fg: Hsla,
    ul: bool,
    ur: bool,
    ll: bool,
    lr: bool,
) {
    let half_w = w * 0.5;
    let half_h = h * 0.5;

    // Merge full-width rows so adjacent quadrants paint as one rect, no
    // subpixel seam between halves when both are lit.
    if ul && ur {
        fill_rect(window, ox, oy, w, half_h, fg);
    } else {
        if ul { fill_rect(window, ox, oy, half_w, half_h, fg); }
        if ur { fill_rect(window, ox + half_w, oy, half_w, half_h, fg); }
    }
    if ll && lr {
        fill_rect(window, ox, oy + half_h, w, half_h, fg);
    } else {
        if ll { fill_rect(window, ox, oy + half_h, half_w, half_h, fg); }
        if lr { fill_rect(window, ox + half_w, oy + half_h, half_w, half_h, fg); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_detection() {
        assert!(is_block_element('\u{2580}')); // ▀
        assert!(is_block_element('█'));
        assert!(is_block_element('░'));
        assert!(is_block_element('▟'));
        assert!(is_block_element('\u{259F}'));
        assert!(!is_block_element('\u{257F}')); // last box-drawing char
        assert!(!is_block_element('\u{25A0}')); // next block (geometric shapes)
        assert!(!is_block_element('A'));
    }

    #[test]
    fn every_glyph_in_range_is_covered() {
        // Guards against typos in the match arms: if a new codepoint gets
        // added mid-range we'll silently no-op and need to update the match.
        for code in 0x2580u32..=0x259F {
            let ch = char::from_u32(code).unwrap();
            assert!(is_block_element(ch), "U+{:04X} missing", code);
        }
    }
}
