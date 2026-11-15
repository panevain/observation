//! A small RGB canvas for the map's picture: alpha blending, even-odd
//! scanline polygon filling, thick and dashed lines, discs, a 5 × 7 bitmap
//! font, and PNG encoding. Hand-written so that the server needs no 2D
//! graphics library for one picture of flat shapes.

use observation_tokens::Rgba;

/// An RGB image, row by row from the top left.
#[derive(Debug, Clone)]
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    px: Vec<[u8; 3]>,
}

impl Canvas {
    /// A canvas filled with `bg` (its alpha is ignored).
    pub fn new(width: u32, height: u32, bg: Rgba) -> Self {
        Self {
            width,
            height,
            px: vec![[bg[0], bg[1], bg[2]]; width as usize * height as usize],
        }
    }

    /// The pixel at `(x, y)`, if on the canvas.
    #[cfg(test)]
    pub fn get(&self, x: u32, y: u32) -> Option<[u8; 3]> {
        (x < self.width && y < self.height)
            .then(|| self.px[y as usize * self.width as usize + x as usize])
    }

    /// Paints `c` over the pixel at `(x, y)` with `c`'s alpha; off the
    /// canvas, nothing.
    pub fn blend(&mut self, x: i64, y: i64, c: Rgba) {
        if x < 0 || y < 0 || x >= i64::from(self.width) || y >= i64::from(self.height) {
            return;
        }
        let i = y as usize * self.width as usize + x as usize;
        let a = u16::from(c[3]);
        let p = &mut self.px[i];
        for k in 0..3 {
            let over = u16::from(c[k]) * a + u16::from(p[k]) * (255 - a);
            p[k] = ((over + 127) / 255) as u8;
        }
    }

    /// Fills the polygon whose rings (in pixels, each closed implicitly) are
    /// `rings`, by the even-odd rule, so holes stay open. A pixel is inside
    /// when its centre is.
    pub fn fill_polygon(&mut self, rings: &[Vec<[f32; 2]>], c: Rgba) {
        let (mut top, mut bottom) = (f32::INFINITY, f32::NEG_INFINITY);
        for p in rings.iter().flatten() {
            top = top.min(p[1]);
            bottom = bottom.max(p[1]);
        }
        if !(top.is_finite() && bottom.is_finite()) {
            return;
        }
        let y0 = (top - 0.5).ceil().max(0.0) as i64;
        let y1 = ((bottom - 0.5).floor() as i64).min(i64::from(self.height) - 1);
        let mut xs: Vec<f32> = Vec::new();
        for y in y0..=y1 {
            let sy = y as f32 + 0.5;
            xs.clear();
            for ring in rings {
                let n = ring.len();
                for k in 0..n {
                    let (a, b) = (ring[k], ring[(k + 1) % n]);
                    // Half-open in y, so a vertex on the scanline counts once.
                    if (a[1] <= sy) != (b[1] <= sy) {
                        xs.push(a[0] + (sy - a[1]) / (b[1] - a[1]) * (b[0] - a[0]));
                    }
                }
            }
            xs.sort_by(f32::total_cmp);
            for pair in xs.as_chunks::<2>().0 {
                let x0 = (pair[0] - 0.5).ceil().max(0.0) as i64;
                let x1 = ((pair[1] - 0.5).floor() as i64).min(i64::from(self.width) - 1);
                for x in x0..=x1 {
                    self.blend(x, y, c);
                }
            }
        }
    }

    /// A line `width` pixels wide from `a` to `b` (pixel coordinates), with
    /// round ends: every pixel whose centre lies within `width / 2` of it.
    pub fn line(&mut self, a: [f32; 2], b: [f32; 2], width: f32, c: Rgba) {
        let r = width / 2.0;
        let x0 = (a[0].min(b[0]) - r).floor().max(0.0) as i64;
        let x1 = ((a[0].max(b[0]) + r).ceil() as i64).min(i64::from(self.width) - 1);
        let y0 = (a[1].min(b[1]) - r).floor().max(0.0) as i64;
        let y1 = ((a[1].max(b[1]) + r).ceil() as i64).min(i64::from(self.height) - 1);
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len2 = dx * dx + dy * dy;
        for y in y0..=y1 {
            for x in x0..=x1 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let t = if len2 > 0.0 {
                    (((px - a[0]) * dx + (py - a[1]) * dy) / len2).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let (qx, qy) = (a[0] + t * dx - px, a[1] + t * dy - py);
                if qx * qx + qy * qy <= r * r {
                    self.blend(x, y, c);
                }
            }
        }
    }

    /// A dashed line: `dash` pixels drawn, then `dash` left out.
    pub fn dashed_line(&mut self, a: [f32; 2], b: [f32; 2], width: f32, dash: f32, c: Rgba) {
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = (dx * dx + dy * dy).sqrt();
        if len <= 0.0 || dash <= 0.0 {
            return;
        }
        let mut s = 0.0;
        while s < len {
            let e = (s + dash).min(len);
            let at = |t: f32| [a[0] + dx * t / len, a[1] + dy * t / len];
            self.line(at(s), at(e), width, c);
            s += 2.0 * dash;
        }
    }

    /// A filled disc of radius `r` pixels centred on `p`.
    pub fn disc(&mut self, p: [f32; 2], r: f32, c: Rgba) {
        self.line(p, p, 2.0 * r, c);
    }

    /// `text` in the 5 × 7 font, each dot `scale` pixels square, with its
    /// top left at `(x, y)`, over a one-dot `halo` so that it reads on any
    /// colour. Characters the font lacks are left blank.
    pub fn text(&mut self, x: i64, y: i64, text: &str, scale: i64, c: Rgba, halo: Rgba) {
        for pass in 0..2 {
            for (k, ch) in text.chars().enumerate() {
                let Some(rows) = glyph(ch) else { continue };
                let gx = x + k as i64 * 6 * scale;
                for (ry, bits) in rows.iter().enumerate() {
                    for rx in 0..5 {
                        if bits & (0x10 >> rx) == 0 {
                            continue;
                        }
                        let (px, py) = (gx + rx * scale, y + ry as i64 * scale);
                        if pass == 0 {
                            self.square(px - scale, py - scale, 3 * scale, halo);
                        } else {
                            self.square(px, py, scale, c);
                        }
                    }
                }
            }
        }
    }

    fn square(&mut self, x: i64, y: i64, size: i64, c: Rgba) {
        for yy in y..y + size {
            for xx in x..x + size {
                self.blend(xx, yy, c);
            }
        }
    }

    /// The canvas as a PNG file (8-bit RGB).
    pub fn png(&self) -> Result<Vec<u8>, png::EncodingError> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, self.width, self.height);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
            enc.set_compression(png::Compression::Balanced);
            let mut writer = enc.write_header()?;
            let data: Vec<u8> = self.px.iter().flatten().copied().collect();
            writer.write_image_data(&data)?;
            writer.finish()?;
        }
        Ok(out)
    }
}

/// Width of `text` in the font at `scale`, pixels.
pub fn text_width(text: &str, scale: i64) -> i64 {
    let n = text.chars().count() as i64;
    if n == 0 { 0 } else { (6 * n - 1) * scale }
}

/// Height of the font at `scale`, pixels.
pub const fn text_height(scale: i64) -> i64 {
    7 * scale
}

/// The 5 × 7 dots of `c`, top row first, the leftmost dot in bit 4: the
/// digits, capitals, `m`, `-` and `+`, which is all the map writes.
fn glyph(c: char) -> Option<[u8; 7]> {
    Some(match c {
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        'A' => [0x0E, 0x11, 0x11, 0x11, 0x1F, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1C, 0x12, 0x11, 0x11, 0x11, 0x12, 0x1C],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x11, 0x0A, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        'm' => [0x00, 0x00, 0x1A, 0x15, 0x15, 0x11, 0x11],
        '-' => [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00],
        '+' => [0x00, 0x04, 0x04, 0x1F, 0x04, 0x04, 0x00],
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const INK: Rgba = [0, 0, 0, 255];
    const WHITE: Rgba = [255, 255, 255, 255];

    fn dark(c: &Canvas) -> usize {
        (0..c.height)
            .flat_map(|y| (0..c.width).map(move |x| (x, y)))
            .filter(|&(x, y)| c.get(x, y) == Some([0, 0, 0]))
            .count()
    }

    #[test]
    fn a_square_fills_its_pixels_and_a_hole_stays_open() {
        let mut c = Canvas::new(10, 10, WHITE);
        let outer = vec![[1.0, 1.0], [9.0, 1.0], [9.0, 9.0], [1.0, 9.0]];
        c.fill_polygon(std::slice::from_ref(&outer), INK);
        assert_eq!(dark(&c), 64);
        let mut c = Canvas::new(10, 10, WHITE);
        let hole = vec![[3.0, 3.0], [7.0, 3.0], [7.0, 7.0], [3.0, 7.0]];
        c.fill_polygon(&[outer, hole], INK);
        assert_eq!(dark(&c), 64 - 16);
        assert_eq!(c.get(5, 5), Some([255, 255, 255]));
    }

    #[test]
    fn blending_mixes_by_alpha() {
        let mut c = Canvas::new(1, 1, WHITE);
        c.blend(0, 0, [0, 0, 0, 128]);
        assert_eq!(c.get(0, 0), Some([127, 127, 127]));
        // Off the canvas: nothing, and no panic.
        c.blend(-1, 0, INK);
        c.blend(0, 1, INK);
    }

    #[test]
    fn a_line_covers_its_length_and_width() {
        let mut c = Canvas::new(20, 5, WHITE);
        c.line([2.0, 2.5], [18.0, 2.5], 1.0, INK);
        // 16 pixels long, and half a pixel more at each round end.
        assert_eq!(dark(&c), 18);
        let mut c = Canvas::new(20, 5, WHITE);
        c.dashed_line([0.0, 2.5], [20.0, 2.5], 1.0, 5.0, INK);
        assert!(c.get(2, 2) == Some([0, 0, 0]) && c.get(7, 2) == Some([255, 255, 255]));
    }

    #[test]
    fn text_is_drawn_in_the_font_and_unknown_characters_are_blank() {
        let mut c = Canvas::new(20, 10, WHITE);
        c.text(1, 1, "1", 1, INK, WHITE);
        // The digit 1 has 10 dots.
        assert_eq!(dark(&c), 10);
        let mut c = Canvas::new(20, 10, WHITE);
        c.text(1, 1, "?", 1, INK, WHITE);
        assert_eq!(dark(&c), 0);
        assert_eq!(text_width("12", 2), 22);
        assert_eq!(text_height(2), 14);
    }

    #[test]
    fn the_png_decodes_to_the_canvas() {
        let mut c = Canvas::new(3, 2, WHITE);
        c.blend(1, 1, INK);
        let bytes = c.png().unwrap();
        let dec = png::Decoder::new(std::io::Cursor::new(bytes));
        let mut reader = dec.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (3, 2));
        assert_eq!(&buf[12..15], &[0, 0, 0]);
        assert_eq!(&buf[0..3], &[255, 255, 255]);
    }
}
