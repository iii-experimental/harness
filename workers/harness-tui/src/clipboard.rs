//! Clipboard image read + minimal RGBA-to-PNG encode.
//!
//! `arboard::Clipboard::get_image()` returns RGBA frames on macOS and Linux;
//! we re-encode to a PNG buffer (via the `png` crate) and return
//! `(bytes, mime)` for callers to pack into `ContentBlock::Image`.
//!
//! Windows is best-effort — `arboard` exposes the same `get_image` API on
//! Windows, but support is gated by feature flags downstream of this crate;
//! the function returns `None` on platforms where the runtime call fails.
//!
//! Apache-2.0 licensed.

/// Read the clipboard's current image, if any. Returns `(png_bytes, mime)`.
pub fn read_image() -> Option<(Vec<u8>, &'static str)> {
    let mut clip = arboard::Clipboard::new().ok()?;
    let img = clip.get_image().ok()?;
    let bytes = encode_png_rgba(img.width as u32, img.height as u32, &img.bytes)?;
    Some((bytes, "image/png"))
}

/// Read the clipboard's current text (fallback for non-image paste).
pub fn read_text() -> Option<String> {
    let mut clip = arboard::Clipboard::new().ok()?;
    clip.get_text().ok()
}

/// Encode an RGBA pixel buffer to a PNG byte stream. Returns `None` when
/// the dimensions or buffer length disagree.
pub fn encode_png_rgba(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let expected = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    if rgba.len() != expected {
        return None;
    }
    let mut out = Vec::with_capacity(rgba.len() / 4 + 256);
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(rgba).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_png_rejects_size_mismatch() {
        // 2x2 image needs 16 bytes; supply 4.
        assert!(encode_png_rgba(2, 2, &[0, 0, 0, 255]).is_none());
    }

    #[test]
    fn encode_png_emits_valid_signature() {
        let rgba = vec![0xFFu8; 4 * 2 * 2];
        let bytes = encode_png_rgba(2, 2, &rgba).expect("png encode");
        assert!(bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]));
    }
}
