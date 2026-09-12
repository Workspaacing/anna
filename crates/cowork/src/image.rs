//! Turning a file on disk into something a model can look at.
//!
//! The work here is small and almost entirely about refusing correctly. Every provider takes an
//! image as base64 plus a declared media type, so the encoding is trivial; what is not trivial is
//! that a request carrying the wrong media type, an unsupported format or too many bytes comes
//! back as a flat `400` whose body does not say which of several images was the problem. That
//! failure arrives after the request is spent and with the user's message already gone. So
//! everything that can be checked locally is checked locally, and the user is told which file and
//! why before anything is sent.

use anyhow::{Context as _, Result, bail};
use base64::Engine as _;
use std::path::Path;

use crate::provider::Attachment;

/// The formats that every provider in the catalog accepts.
///
/// This is a closed set, not a passthrough. Anthropic's `media_type` is an enum of exactly these
/// four and rejects everything else; OpenAI and Google accept a wider range, but a picture the
/// user can attach to one model and not another is a worse experience than one rule that always
/// holds. The intersection is the rule.
const SUPPORTED: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/// The largest image that may be sent inline, measured the way the providers measure it.
///
/// The limit is on the *encoded* payload, not the file: Anthropic documents 10 MB base64 for its
/// own API, and 5 MB when the same request is served through Amazon Bedrock or Google Cloud.
/// Base64 is four bytes for every three, so the file itself may be three quarters of this.
///
/// 10 MB is the figure used because that is what the endpoints this app talks to enforce. Anyone
/// routing through Bedrock will hit the lower one, and will be told so by the provider rather than
/// silently refused here — a limit that is stricter than the server's turns a working request into
/// a error message for no reason.
const MAX_BASE64_BYTES: usize = 10 * 1024 * 1024;

/// The same budget expressed in bytes of the original file, for checking before encoding.
const MAX_BYTES: usize = MAX_BASE64_BYTES / 4 * 3;

/// The media type of an image, read from the bytes rather than from the file name.
///
/// A file called `shot.png` that is really a JPEG is ordinary — screenshots get renamed, browsers
/// save under the wrong extension, and Windows hides the extension by default so the user may
/// never have seen it. Trusting the name means declaring `image/png` over JPEG bytes, which every
/// provider rejects with an error that blames the image rather than the label. The first bytes of
/// each format are unambiguous, so there is no reason to guess.
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
    const JPEG: &[u8] = b"\xff\xd8\xff";

    if bytes.starts_with(PNG) {
        Some("image/png")
    } else if bytes.starts_with(JPEG) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(&b"WEBP"[..]) {
        // WebP is a RIFF container: "RIFF", four bytes of length, then the form type. The length
        // sitting in between is why this cannot be a single prefix check like the others.
        Some("image/webp")
    } else {
        None
    }
}

/// Whether a model that takes images would accept this one.
pub fn is_supported(media_type: &str) -> bool {
    SUPPORTED.contains(&media_type)
}

/// Whether these bytes are a Windows bitmap.
///
/// "BM" and nothing more. The length that follows is not checked, because a truncated bitmap
/// should fail in the decoder with a real message rather than here as "not an image".
fn is_bitmap(bytes: &[u8]) -> bool {
    bytes.starts_with(b"BM")
}

/// Re-encodes a bitmap as PNG, which is the only conversion done anywhere in this module.
///
/// BMP is here for one specific reason: it is what a Windows screenshot becomes. PrintScreen puts
/// a device-independent bitmap on the clipboard, and GPUI hands that to the app as BMP — so the
/// most ordinary way a Windows user attaches a picture produces the one common format no provider
/// accepts. Refusing it would be correct and useless.
///
/// The conversion loses nothing. BMP stores raw pixels and PNG compresses them without discarding
/// any, so every pixel that reaches the model is the pixel that was on the screen. That is what
/// "the original image" has to mean here — the picture, not the container it arrived in. Nothing
/// else is converted, because every other conversion worth doing is lossy.
fn bitmap_to_png(bytes: &[u8]) -> Result<Vec<u8>> {
    use image::ImageFormat;

    let decoded =
        image::load_from_memory_with_format(bytes, ImageFormat::Bmp).context("reading the bitmap")?;

    let mut png = std::io::Cursor::new(Vec::new());
    decoded
        .write_to(&mut png, ImageFormat::Png)
        .context("re-encoding it as PNG")?;

    Ok(png.into_inner())
}

/// Refuses an image too large to send, saying how large it is.
fn check_size(name: &str, len: usize) -> Result<()> {
    if len > MAX_BYTES {
        bail!(
            "{name} is {:.1} MB, and an image may be at most {:.1} MB. Resizing it is usually \
             enough; models read a shrunk image about as well as the original.",
            len as f64 / (1024.0 * 1024.0),
            MAX_BYTES as f64 / (1024.0 * 1024.0),
        );
    }
    Ok(())
}

/// Prepares bytes under a name, which is what the clipboard has instead of a file.
pub fn attach_named(name: String, bytes: Vec<u8>) -> Result<Attachment> {
    // Checked before decoding, so a 400 MB bitmap is refused rather than expanded in memory first.
    check_size(&name, bytes.len())?;

    let (media_type, bytes) = match sniff(&bytes) {
        Some(media_type) => (media_type, bytes),
        None if is_bitmap(&bytes) => (
            "image/png",
            bitmap_to_png(&bytes).with_context(|| {
                format!("{name} looks like a bitmap but could not be converted to PNG")
            })?,
        ),
        None => bail!(
            "{name} is not an image Cowork can send. Models accept PNG, JPEG, GIF and WebP; this \
             file is none of them — convert it and try again."
        ),
    };

    // `sniff` and `SUPPORTED` are two hand-maintained lists of the same four formats, and a
    // signature added to one but not the other would declare a media type no provider accepts —
    // arriving as a 400 that blames the picture. Checking costs a comparison.
    if !is_supported(media_type) {
        bail!("{name} is a {media_type}, which no provider accepts.");
    }

    // Again on the result: a bitmap that fit may not fit once re-encoded.
    check_size(&name, bytes.len())?;

    Ok(Attachment {
        media_type: media_type.to_owned(),
        data: base64::engine::general_purpose::STANDARD.encode(&bytes),
        name,
    })
}

/// Prepares one file for sending, or explains why it cannot be sent.
///
/// Apart from the lossless bitmap case above, the bytes are passed through unchanged — no
/// re-encoding, no resizing, no conversion to some "safe" format. What the model sees is the file
/// the user attached, because an image that has been through a lossy round-trip is a different
/// image, and the detail the user wanted looked at is exactly the kind of thing a re-encode
/// destroys.
pub fn attach(path: &Path, bytes: Vec<u8>) -> Result<Attachment> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".to_owned());

    attach_named(name, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_format_is_recognised_by_its_first_bytes() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\nanything"), Some("image/png"));
        assert_eq!(sniff(b"\xff\xd8\xff\xe0rest"), Some("image/jpeg"));
        assert_eq!(sniff(b"GIF89a...."), Some("image/gif"));
        assert_eq!(sniff(b"GIF87a...."), Some("image/gif"));
        assert_eq!(sniff(b"RIFF\x24\x00\x00\x00WEBPVP8 "), Some("image/webp"));
    }

    #[test]
    fn the_name_does_not_decide_the_type() {
        // The case this exists for: a JPEG saved as `.png`. Declaring the extension's type would
        // be rejected by the provider, blaming the picture rather than the label.
        let jpeg_bytes = b"\xff\xd8\xff\xe0\x00\x10JFIF".to_vec();
        let attachment = attach(Path::new("screenshot.png"), jpeg_bytes).unwrap();

        assert_eq!(attachment.media_type, "image/jpeg");
        assert_eq!(attachment.name, "screenshot.png");
    }

    #[test]
    fn a_riff_container_that_is_not_webp_is_not_an_image() {
        // A WAV file also begins "RIFF"; only the form type at byte 8 separates them.
        assert_eq!(sniff(b"RIFF\x24\x00\x00\x00WAVEfmt "), None);
    }

    #[test]
    fn truncated_input_does_not_panic() {
        // An empty or part-written file is ordinary — a screenshot caught mid-save, a failed
        // download. Indexing past the end would take the whole app down with it.
        for length in 0..12 {
            assert_eq!(sniff(&b"RIFF\x24\x00\x00\x00WEBP"[..length]), None);
        }
    }

    #[test]
    fn an_unsupported_format_is_refused_by_name() {
        // A TIFF: a real image in a real format, and one no provider takes.
        let error = attach(Path::new("scan.tiff"), b"II\x2a\x00\x08\x00\x00\x00".to_vec())
            .expect_err("TIFF should be refused");

        assert!(error.to_string().contains("scan.tiff"), "{error}");
    }

    #[test]
    fn a_windows_screenshot_arrives_as_a_bitmap_and_is_sent_as_a_png() {
        // The exact path a pasted PrintScreen takes on Windows. Two pixels, red and blue.
        let bitmap = {
            let mut pixels = image::RgbImage::new(2, 1);
            pixels.put_pixel(0, 0, image::Rgb([255, 0, 0]));
            pixels.put_pixel(1, 0, image::Rgb([0, 0, 255]));
            let mut buffer = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgb8(pixels)
                .write_to(&mut buffer, image::ImageFormat::Bmp)
                .unwrap();
            buffer.into_inner()
        };
        assert_eq!(sniff(&bitmap), None, "a bitmap is not directly sendable");

        let attachment = attach_named("screenshot.bmp".to_owned(), bitmap).unwrap();
        assert_eq!(attachment.media_type, "image/png");

        // Lossless: the pixels that come back out are the pixels that went in.
        let png = base64::engine::general_purpose::STANDARD
            .decode(&attachment.data)
            .unwrap();
        let decoded = image::load_from_memory(&png).unwrap().to_rgb8();
        assert_eq!(decoded.dimensions(), (2, 1));
        assert_eq!(decoded.get_pixel(0, 0), &image::Rgb([255, 0, 0]));
        assert_eq!(decoded.get_pixel(1, 0), &image::Rgb([0, 0, 255]));
    }

    #[test]
    fn something_merely_starting_with_bm_is_not_silently_accepted() {
        // The signature check is cheap on purpose, which leaves the decoder to reject a bad
        // bitmap — but it does have to reject it, rather than sending nonsense labelled as a PNG.
        let error = attach_named("notes.txt".to_owned(), b"BMX not a bitmap at all".to_vec())
            .expect_err("garbage should not become a PNG");

        assert!(error.to_string().contains("notes.txt"), "{error}");
    }

    #[test]
    fn an_image_too_large_to_send_says_how_large_it_is() {
        let mut oversized = b"\x89PNG\r\n\x1a\n".to_vec();
        oversized.resize(MAX_BYTES + 1, 0);
        let error = attach(Path::new("huge.png"), oversized).expect_err("over the limit");

        let message = error.to_string();
        assert!(message.contains("huge.png"), "{message}");
        assert!(message.contains("7.5 MB"), "{message}");
    }

    #[test]
    fn the_file_limit_is_the_encoded_limit_the_providers_actually_apply() {
        // The published limit is on the base64 payload. Base64 is four bytes for every three, so a
        // file of `MAX_BYTES` must encode to no more than `MAX_BASE64_BYTES` — if this ever stops
        // holding, images that look acceptable here would be refused by the provider.
        let encoded = MAX_BYTES.div_ceil(3) * 4;
        assert!(
            encoded <= MAX_BASE64_BYTES,
            "{MAX_BYTES} bytes encodes to {encoded}, over the {MAX_BASE64_BYTES} allowed"
        );
    }

    #[test]
    fn the_bytes_are_sent_exactly_as_they_were_read() {
        // "no formato da imagem original": what arrives at the model decodes back to the file on
        // disk, byte for byte.
        let original = b"\x89PNG\r\n\x1a\n\x00\x01\x02\xfe\xff".to_vec();
        let attachment = attach(Path::new("a.png"), original.clone()).unwrap();

        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&attachment.data)
            .unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn every_sniffed_type_is_one_a_provider_takes() {
        for bytes in [
            &b"\x89PNG\r\n\x1a\n"[..],
            &b"\xff\xd8\xff"[..],
            &b"GIF89a"[..],
            &b"RIFF\x00\x00\x00\x00WEBP"[..],
        ] {
            let media_type = sniff(bytes).expect("known signature");
            assert!(
                is_supported(media_type),
                "{media_type} is sniffed but not sendable"
            );
        }
    }
}
