//! Terminal image protocol detection and escape-sequence encoding.
//!
//! Targets two protocols, both widely supported on macOS:
//! - Kitty graphics protocol (Kitty, WezTerm, Ghostty)
//! - iTerm2 inline-image protocol (iTerm2, WezTerm)
//!
//! On terminals that speak neither, callers fall back to a placeholder line.
//! The encoders return raw escape strings; the renderer is responsible for
//! writing them at the correct cursor position. See `src/render.rs` for the
//! placeholder-only path that is wired in by default; the native escape-write
//! path is gated behind `App.image_render_native` so we can ship a stable
//! placeholder fallback first and turn on native rendering per-terminal as we
//! prove it out.
//!
//! No external image-decoding crate: dimensions are sniffed from the file
//! header bytes (PNG IHDR, JPEG SOFn, GIF logical screen, WebP VP8/VP8L/VP8X).
//! That is enough to compute row reservation for terminal cells.
//!
//! Apache-2.0 licensed, same as the rest of the workspace.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;

/// Terminal image protocol detected at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageProtocol {
    Kitty,
    ITerm2,
    None,
}

/// Detect the available image protocol from common environment variables.
///
/// Order of checks is intentional:
/// 1. `KITTY_WINDOW_ID` is the strongest Kitty signal.
/// 2. `TERM_PROGRAM == "kitty"` covers Kitty when `KITTY_WINDOW_ID` is missing.
/// 3. `LC_TERMINAL == "iTerm2"` and `TERM_PROGRAM == "iTerm.app"` cover iTerm2.
/// 4. WezTerm supports both; we prefer iTerm2 escape because its session
///    handshake is shorter and survives tmux better.
pub fn detect_protocol() -> ImageProtocol {
    detect_protocol_from(EnvSnapshot::from_env())
}

/// Test-friendly version of `detect_protocol`. The `EnvSnapshot` makes it
/// trivial to write deterministic unit tests without touching the real env.
pub fn detect_protocol_from(env: EnvSnapshot) -> ImageProtocol {
    if env.kitty_window_id.is_some() {
        return ImageProtocol::Kitty;
    }
    if env.term_program.as_deref() == Some("kitty") {
        return ImageProtocol::Kitty;
    }
    if env.lc_terminal.as_deref() == Some("iTerm2")
        || env.term_program.as_deref() == Some("iTerm.app")
    {
        return ImageProtocol::ITerm2;
    }
    if env.term_program.as_deref() == Some("WezTerm") {
        return ImageProtocol::ITerm2;
    }
    ImageProtocol::None
}

/// Snapshot of the four environment variables that drive protocol detection.
#[derive(Debug, Clone, Default)]
pub struct EnvSnapshot {
    pub term_program: Option<String>,
    pub kitty_window_id: Option<String>,
    pub lc_terminal: Option<String>,
    pub term: Option<String>,
}

impl EnvSnapshot {
    pub fn from_env() -> Self {
        Self {
            term_program: std::env::var("TERM_PROGRAM").ok(),
            kitty_window_id: std::env::var("KITTY_WINDOW_ID").ok(),
            lc_terminal: std::env::var("LC_TERMINAL").ok(),
            term: std::env::var("TERM").ok(),
        }
    }
}

/// Compute the number of terminal cell rows required to display an image of
/// `(width_px, height_px)` pixels at a cell of `(cell_w_px, cell_h_px)`,
/// preserving aspect ratio and clamped to `max_rows`.
///
/// Returns at least 1 row when both dimensions are non-zero.
pub fn calculate_image_rows(
    width_px: u32,
    height_px: u32,
    cell_w_px: u16,
    cell_h_px: u16,
    max_rows: u16,
) -> u16 {
    if width_px == 0 || height_px == 0 || cell_w_px == 0 || cell_h_px == 0 {
        return 0;
    }
    let rows = (height_px as f32 / cell_h_px as f32).ceil() as u32;
    let max = max_rows.max(1) as u32;
    let clamped = rows.clamp(1, max);
    clamped as u16
}

/// Sniff the leading bytes of an image and return `(width, height, format)`
/// when the format is one of `png` / `jpeg` / `gif` / `webp`.
///
/// Returns `None` when the magic does not match a supported format.
pub fn get_image_dimensions(bytes: &[u8]) -> Option<(u32, u32, &'static str)> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        // PNG: 8-byte signature, then 4-byte length, "IHDR" type, width(4),
        // height(4) — all big-endian.
        if bytes.len() >= 24 {
            let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
            let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
            return Some((w, h, "png"));
        }
        return None;
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return jpeg_dimensions(bytes).map(|(w, h)| (w, h, "jpeg"));
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        if bytes.len() >= 10 {
            let w = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
            let h = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
            return Some((w, h, "gif"));
        }
        return None;
    }
    if bytes.len() >= 30 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return webp_dimensions(bytes).map(|(w, h)| (w, h, "webp"));
    }
    None
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    // Walk the JPEG segment chain looking for an SOFn marker (0xFFC0..=0xFFCF
    // except 0xFFC4 and 0xFFC8). The first SOFn carries height (2) then width
    // (2) at offset +5 from the marker.
    let mut i = 2;
    while i + 1 < bytes.len() {
        if bytes[i] != 0xFF {
            return None;
        }
        let marker = bytes[i + 1];
        // Skip 0xFF padding bytes.
        if marker == 0xFF {
            i += 1;
            continue;
        }
        // Standalone markers (no length): SOI, EOI, RSTn, TEM.
        if matches!(marker, 0xD0..=0xD7 | 0xD8 | 0xD9 | 0x01) {
            i += 2;
            continue;
        }
        if i + 4 > bytes.len() {
            return None;
        }
        let seg_len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
        if seg_len < 2 {
            return None;
        }
        let is_sof = matches!(marker, 0xC0..=0xCF) && marker != 0xC4 && marker != 0xC8;
        if is_sof {
            if i + 9 > bytes.len() {
                return None;
            }
            let h = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
            let w = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]) as u32;
            return Some((w, h));
        }
        i += 2 + seg_len;
    }
    None
}

fn webp_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    // RIFF<size>WEBP <chunk-id><chunk-size>...
    let chunk = &bytes.get(12..16)?;
    match *chunk {
        b"VP8 " => {
            // Lossy: width/height live at offset 26..30 (10-bit each, low bits at 26/28).
            if bytes.len() < 30 {
                return None;
            }
            let w = (u16::from_le_bytes([bytes[26], bytes[27]]) & 0x3FFF) as u32;
            let h = (u16::from_le_bytes([bytes[28], bytes[29]]) & 0x3FFF) as u32;
            Some((w, h))
        }
        b"VP8L" => {
            // Lossless: at offset 21..25 we have packed width-1 (14 bits) and
            // height-1 (14 bits) plus 4 trailing bits (alpha + version).
            if bytes.len() < 25 {
                return None;
            }
            let b0 = bytes[21] as u32;
            let b1 = bytes[22] as u32;
            let b2 = bytes[23] as u32;
            let b3 = bytes[24] as u32;
            let w = ((b1 & 0x3F) << 8 | b0) + 1;
            let h = ((b3 & 0x0F) << 10 | b2 << 2 | (b1 >> 6) & 0x03) + 1;
            Some((w, h))
        }
        b"VP8X" => {
            // Extended: width-1 (24 bits LE) at 24..27, height-1 (24 bits LE) at 27..30.
            if bytes.len() < 30 {
                return None;
            }
            let w =
                (u32::from(bytes[24]) | (u32::from(bytes[25]) << 8) | (u32::from(bytes[26]) << 16))
                    + 1;
            let h =
                (u32::from(bytes[27]) | (u32::from(bytes[28]) << 8) | (u32::from(bytes[29]) << 16))
                    + 1;
            Some((w, h))
        }
        _ => None,
    }
}

/// Encode an image as a Kitty graphics protocol payload, split into one
/// escape sequence per cell row so the caller can write each line at the
/// matching terminal row.
///
/// `cell_rows` is the number of rows the image will occupy. Kitty's `r=`
/// parameter takes a target row count and the protocol does the scaling.
pub fn encode_kitty(image_bytes: &[u8], cell_rows: u16) -> Vec<String> {
    if image_bytes.is_empty() {
        return Vec::new();
    }
    let payload = B64.encode(image_bytes);
    // Kitty splits long base64 in chunks of <= 4096 chars, with `m=1` on every
    // chunk except the last (`m=0`).
    const CHUNK: usize = 4096;
    let bytes = payload.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut idx = 0usize;
    let mut first = true;
    while idx < bytes.len() {
        let end = (idx + CHUNK).min(bytes.len());
        let slice = &bytes[idx..end];
        let more = u8::from(end != bytes.len());
        let header = if first {
            format!("\x1b_Ga=T,f=100,r={cell_rows},m={more};")
        } else {
            format!("\x1b_Gm={more};")
        };
        let mut s = String::with_capacity(header.len() + slice.len() + 2);
        s.push_str(&header);
        s.push_str(std::str::from_utf8(slice).unwrap_or(""));
        s.push_str("\x1b\\");
        out.push(s);
        idx = end;
        first = false;
    }
    out
}

/// Encode an image as an iTerm2 inline-image OSC 1337 sequence. iTerm2 sizes
/// the image off `height=<n>` cell rows; width auto-scales.
pub fn encode_iterm2(image_bytes: &[u8], rows: u16) -> String {
    if image_bytes.is_empty() {
        return String::new();
    }
    let payload = B64.encode(image_bytes);
    format!("\x1b]1337;File=inline=1;preserveAspectRatio=1;height={rows}:{payload}\x07")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(term_program: Option<&str>, kitty: Option<&str>, lc: Option<&str>) -> EnvSnapshot {
        EnvSnapshot {
            term_program: term_program.map(str::to_string),
            kitty_window_id: kitty.map(str::to_string),
            lc_terminal: lc.map(str::to_string),
            term: Some("xterm-256color".into()),
        }
    }

    #[test]
    fn detect_kitty_via_window_id() {
        let s = snap(None, Some("42"), None);
        assert_eq!(detect_protocol_from(s), ImageProtocol::Kitty);
    }

    #[test]
    fn detect_iterm2_via_lc_terminal() {
        let s = snap(None, None, Some("iTerm2"));
        assert_eq!(detect_protocol_from(s), ImageProtocol::ITerm2);
    }

    #[test]
    fn detect_iterm2_via_term_program() {
        let s = snap(Some("iTerm.app"), None, None);
        assert_eq!(detect_protocol_from(s), ImageProtocol::ITerm2);
    }

    #[test]
    fn detect_wezterm_falls_back_to_iterm2() {
        let s = snap(Some("WezTerm"), None, None);
        assert_eq!(detect_protocol_from(s), ImageProtocol::ITerm2);
    }

    #[test]
    fn detect_xterm_returns_none() {
        let s = snap(Some("xterm"), None, None);
        assert_eq!(detect_protocol_from(s), ImageProtocol::None);
    }

    #[test]
    fn calculate_rows_clamps_to_max() {
        // 1000px tall on 16px cell -> 63 rows; clamp to 20.
        assert_eq!(calculate_image_rows(800, 1000, 8, 16, 20), 20);
    }

    #[test]
    fn calculate_rows_returns_at_least_one() {
        // 1px tall image still gets one row.
        assert_eq!(calculate_image_rows(8, 1, 8, 16, 20), 1);
    }

    #[test]
    fn calculate_rows_zero_for_empty_image() {
        assert_eq!(calculate_image_rows(0, 0, 8, 16, 20), 0);
    }

    #[test]
    fn dimensions_detect_png() {
        // 1x1 transparent PNG.
        let bytes = vec![
            0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, // signature
            0x00, 0x00, 0x00, 0x0D, // length
            b'I', b'H', b'D', b'R', // type
            0x00, 0x00, 0x00, 0x07, // width  = 7
            0x00, 0x00, 0x00, 0x05, // height = 5
            0x08, 0x06, 0x00, 0x00, 0x00, // bit depth, colour type, etc
        ];
        assert_eq!(get_image_dimensions(&bytes), Some((7, 5, "png")));
    }

    #[test]
    fn dimensions_detect_jpeg_via_sof0() {
        // Minimal JPEG: SOI, JFIF APP0, SOF0(width=10, height=8).
        let mut b = vec![0xFF, 0xD8]; // SOI
        b.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]); // APP0 length=16
        b.extend_from_slice(b"JFIF\0");
        b.extend_from_slice(&[1, 1, 0, 0, 72, 0, 72, 0, 0]); // 9 bytes payload
        b.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11]); // SOF0 length=17
        b.push(8); // precision
        b.extend_from_slice(&[0x00, 0x08]); // height = 8
        b.extend_from_slice(&[0x00, 0x0A]); // width  = 10
        let dims = get_image_dimensions(&b);
        assert_eq!(dims, Some((10, 8, "jpeg")));
    }

    #[test]
    fn dimensions_detect_gif() {
        let mut b = b"GIF89a".to_vec();
        b.extend_from_slice(&[0x10, 0x00, 0x20, 0x00]); // width=16, height=32 LE
        assert_eq!(get_image_dimensions(&b), Some((16, 32, "gif")));
    }

    #[test]
    fn dimensions_detect_webp_vp8x() {
        // RIFF + size + WEBP + VP8X chunk with 24-bit width-1/height-1.
        let mut b: Vec<u8> = b"RIFF".to_vec();
        b.extend_from_slice(&[0, 0, 0, 0]); // riff size, ignored
        b.extend_from_slice(b"WEBP");
        b.extend_from_slice(b"VP8X");
        b.extend_from_slice(&[10, 0, 0, 0]); // chunk size
        b.extend_from_slice(&[0x10, 0x00, 0x00, 0x00]); // flags + reserved
                                                        // width-1 = 99 -> width = 100
        b.extend_from_slice(&[99, 0, 0]);
        // height-1 = 49 -> height = 50
        b.extend_from_slice(&[49, 0, 0]);
        assert_eq!(get_image_dimensions(&b), Some((100, 50, "webp")));
    }

    #[test]
    fn dimensions_unknown_returns_none() {
        assert_eq!(get_image_dimensions(b"not an image at all"), None);
    }

    #[test]
    fn encode_kitty_emits_at_least_one_chunk() {
        let chunks = encode_kitty(&[1u8, 2, 3, 4], 5);
        assert!(!chunks.is_empty());
        assert!(chunks[0].starts_with("\x1b_G"));
        assert!(chunks.last().unwrap().ends_with("\x1b\\"));
    }

    #[test]
    fn encode_iterm2_starts_with_osc_1337_and_carries_payload() {
        let s = encode_iterm2(b"abc", 4);
        assert!(s.starts_with("\x1b]1337;"));
        assert!(s.contains("inline=1"));
        assert!(s.contains("height=4"));
        assert!(s.ends_with('\x07'));
    }
}
