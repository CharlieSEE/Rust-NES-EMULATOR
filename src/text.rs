//! The 8x8 bitmap font shared by the launcher and the in-game overlay.
//!
//! `font8x8` keeps text to one small dependency: no SDL2_ttf, no font file to
//! find at runtime.

use font8x8::{UnicodeFonts, BASIC_FONTS};

/// Width in pixels of one character cell.
pub(crate) const CELL: usize = 8;

/// Draw one line of text into a framebuffer of `width` x `height` pixels.
/// `px` and `py` are the top-left of the first character; text that runs past
/// the right edge is cut, not wrapped.
pub(crate) fn put_text(
    fb: &mut [u32],
    width: usize,
    height: usize,
    px: usize,
    py: usize,
    text: &str,
    colour: u32,
) {
    let mut x = px;
    for ch in text.chars() {
        if x + CELL > width {
            break;
        }
        let glyph = BASIC_FONTS.get(ch).or_else(|| BASIC_FONTS.get('?'));
        if let Some(glyph) = glyph {
            for (row, bits) in glyph.iter().enumerate() {
                let y = py + row;
                if y >= height {
                    break;
                }
                for col in 0..CELL {
                    if bits & (1 << col) != 0 {
                        fb[y * width + x + col] = colour;
                    }
                }
            }
        }
        x += CELL;
    }
}
