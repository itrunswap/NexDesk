/// Two scanlines retain the original luma while the capture buffer is updated in place.
#[derive(Default)]
pub(crate) struct TextDetail {
    rows: Vec<u8>,
}

impl TextDetail {
    pub(crate) fn apply(&mut self, luma: &mut [u8], stride: usize, width: usize, height: usize) {
        if width < 3 || height < 3 || stride < width || luma.len() / stride < height {
            return;
        }
        self.rows.resize(width * 2, 0);
        let (mut above, mut original) = self.rows.split_at_mut(width);
        above.copy_from_slice(&luma[..width]);
        for row in 1..height - 1 {
            let (head, tail) = luma.split_at_mut((row + 1) * stride);
            let current = &mut head[row * stride..row * stride + width];
            original.copy_from_slice(current);
            enhance_row(above, original, &tail[..width], current);
            std::mem::swap(&mut above, &mut original);
        }
    }
}

fn enhance_row(above: &[u8], original: &[u8], below: &[u8], output: &mut [u8]) {
    let width = output.len();
    let (above, original, below) = (&above[..width], &original[..width], &below[..width]);
    for x in 1..width - 1 {
        let c = original[x];
        let (l, r, u, d) = (original[x - 1], original[x + 1], above[x], below[x]);
        let low = l.min(r).min(u).min(d).min(c);
        let high = l.max(r).max(u).max(d).max(c);
        let laplacian =
            4 * i16::from(c) - i16::from(l) - i16::from(r) - i16::from(u) - i16::from(d);
        // At most ten luma codes per pixel, with no new extrema at a glyph boundary.
        let change = (laplacian / 8).clamp(-10, 10);
        // A contiguous write for every lane lets LLVM vectorize this row on both x64 and ARM.
        output[x] = if high - low < 16 {
            c
        } else {
            (i16::from(c) + change)
                .max(i16::from(low))
                .min(i16::from(high)) as u8
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn antialiased_strokes_gain_contrast_without_ringing_or_chroma_changes() {
        let mut filter = TextDetail::default();
        let source = [32, 32, 62, 151, 211, 211, 211, 249];
        let mut image = source.repeat(5);
        filter.apply(&mut image, 8, 7, 5);
        assert_eq!(&image[0..8], &source);
        assert_eq!(&image[32..40], &source);
        for row in image[8..32].chunks_exact(8) {
            assert!(row[3] - row[2] > source[3] - source[2]);
            assert_eq!(&row[0..2], &[32, 32]);
            assert_eq!(&row[4..], &[211, 211, 211, 249]);
        }
        for pattern in [[32, 32, 32, 211, 211], [52, 55, 58, 61, 64], [125; 5]] {
            let mut frame = pattern.repeat(6);
            filter.apply(&mut frame, 5, 5, 6);
            assert_eq!(frame, pattern.repeat(6));
        }
    }
}
