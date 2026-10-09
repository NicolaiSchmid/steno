//! The host's `QrEncoder`: the pairing URL drawn as a QR code, the PNG the
//! Phones section shows. Swift: `QRCode.png(for:)` in
//! `apps/macos/Steno/Services/QRCode.swift`, which draws it with Core
//! Image's `CIQRCodeGenerator`.

use base64::Engine as _;
use qrcode::{Color, EcLevel, QrCode};
use steno_host::services::QrEncoder;

/// Pixels per module, as Swift's `scale`; the page scales the image
/// further with nearest-neighbour sampling.
pub const MODULE_PIXELS: usize = 8;

/// White modules around the code. The page's white frame widens it to the
/// quiet zone a scanner needs.
pub const MARGIN_MODULES: usize = 1;

/// Draws the text as a QR code at error correction level M, as Swift set
/// `correctionLevel`: black modules on white, [`MODULE_PIXELS`] pixels
/// each, [`MARGIN_MODULES`] white modules around, written as an 8-bit
/// greyscale PNG in standard base64. `None` when the text is too long for a
/// QR code, as Swift answered nil.
#[derive(Debug, Default, Clone, Copy)]
pub struct PngQrEncoder;

impl QrEncoder for PngQrEncoder {
    fn png_base64(&self, text: &str) -> Option<String> {
        let png = png_bytes(text)
            .inspect_err(|error| tracing::warn!("the pairing QR code could not be drawn: {error}"))
            .ok()?;
        Some(base64::engine::general_purpose::STANDARD.encode(png))
    }
}

/// Why a QR code could not be drawn.
#[derive(Debug, thiserror::Error)]
pub enum QrError {
    #[error("the text does not fit a QR code: {0}")]
    Encode(#[from] qrcode::types::QrError),
    #[error("the PNG could not be written: {0}")]
    Png(#[from] png::EncodingError),
}

/// The PNG [`PngQrEncoder`] draws, before base64.
pub fn png_bytes(text: &str) -> Result<Vec<u8>, QrError> {
    let code = QrCode::with_error_correction_level(text.as_bytes(), EcLevel::M)?;
    let modules = code.width();
    let colors = code.to_colors();
    let side_modules = modules + 2 * MARGIN_MODULES;
    let side = side_modules * MODULE_PIXELS;
    let mut pixels = vec![u8::MAX; side * side];
    for (index, color) in colors.iter().enumerate() {
        if *color != Color::Dark {
            continue;
        }
        let (row, column) = (index / modules, index % modules);
        let top = (row + MARGIN_MODULES) * MODULE_PIXELS;
        let left = (column + MARGIN_MODULES) * MODULE_PIXELS;
        for y in top..top + MODULE_PIXELS {
            pixels[y * side + left..y * side + left + MODULE_PIXELS].fill(0);
        }
    }
    // A version 40 code is 177 modules, so the side stays far below u32.
    let side = u32::try_from(side).expect("a QR code's side fits u32");
    let mut png = Vec::new();
    let mut encoder = png::Encoder::new(&mut png, side, side);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&pixels)?;
    writer.finish()?;
    Ok(png)
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone as _, Utc};
    use steno_handover::PairingPayload;
    use uuid::Uuid;

    use super::*;

    /// Reads a PNG the way a phone's camera reads the screen: decoded to
    /// grey levels, then searched for codes.
    fn scan(png: &[u8]) -> Vec<String> {
        let decoder = png::Decoder::new(std::io::Cursor::new(png));
        let mut reader = decoder.read_info().unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let frame = reader.next_frame(&mut pixels).unwrap();
        assert_eq!(frame.color_type, png::ColorType::Grayscale);
        let (width, height) = (frame.width as usize, frame.height as usize);
        let mut image = rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| {
            pixels[y * width + x]
        });
        image
            .detect_grids()
            .into_iter()
            .map(|grid| grid.decode().unwrap().1)
            .collect()
    }

    fn payload(name: &str) -> PairingPayload {
        PairingPayload::new(
            Uuid::parse_str("6F9619FF-8B86-D011-B42D-00C04FC964FF").unwrap(),
            name,
            (0u8..32).map(|byte| byte.wrapping_mul(7)).collect(),
            (0u8..32).map(|byte| 255 - byte).collect(),
            Utc.with_ymd_and_hms(2026, 10, 9, 12, 0, 0).unwrap(),
        )
    }

    /// The image decodes back to the exact pairing URL, which parses into
    /// the payload it came from, also with a name that needs
    /// percent-encoding and one outside ASCII.
    #[test]
    fn the_code_decodes_back_to_the_pairing_payload() {
        for name in [
            "Studio Mac",
            "Nicolai's MacBook Pro + Office",
            "Büro-PC 東京",
        ] {
            let payload = payload(name);
            let url = payload.url_string();
            let base64 = PngQrEncoder.png_base64(&url).expect("a code");
            let png = base64::engine::general_purpose::STANDARD
                .decode(base64)
                .unwrap();
            assert_eq!(scan(&png), vec![url.clone()], "{name}");
            assert_eq!(PairingPayload::parse(&url).unwrap(), payload);
        }
    }

    /// Each module is a square of [`MODULE_PIXELS`], inside a margin of
    /// [`MARGIN_MODULES`] white modules, and the top-left finder pattern
    /// starts right after it.
    #[test]
    fn the_modules_are_square_blocks_inside_a_white_margin() {
        let png = png_bytes(&payload("Studio Mac").url_string()).unwrap();
        let decoder = png::Decoder::new(std::io::Cursor::new(&png));
        let mut reader = decoder.read_info().unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let frame = reader.next_frame(&mut pixels).unwrap();
        let side = frame.width as usize;
        assert_eq!(frame.height as usize, side);
        assert_eq!(side % MODULE_PIXELS, 0);
        let margin = MARGIN_MODULES * MODULE_PIXELS;
        assert!(pixels[..margin * side].iter().all(|&grey| grey == u8::MAX));
        for y in margin..margin + MODULE_PIXELS {
            for x in margin..margin + MODULE_PIXELS {
                assert_eq!(pixels[y * side + x], 0, "({x}, {y})");
            }
        }
    }

    /// A text too long for the largest code draws nothing, as Swift's nil.
    #[test]
    fn a_text_too_long_for_a_code_draws_nothing() {
        assert!(PngQrEncoder.png_base64(&"x".repeat(4000)).is_none());
    }
}
