//! Turning a file that is not a picture into something a model can read.
//!
//! Two kinds are accepted. Text — source, markdown, CSV, logs, configuration — goes to every model,
//! because every model reads text: it is sent as ordinary words fenced by the file's name, not as a
//! document the provider has to parse. A PDF goes only to a model whose catalog entry says it reads
//! PDFs, because the rest reject the whole request. As with pictures, everything that can be checked
//! locally is checked while the user is still looking at the picker, not after the message has gone.

use anyhow::{Result, bail};
use base64::Engine as _;
use std::path::Path;

use crate::provider::Attachment;

/// The media type recorded for every text file.
///
/// Text is sent to the model as text, never under this label, so a more specific type guessed from
/// the extension would be a second opinion that nothing reads. It exists so the transcript and the
/// wire formats can tell a text file from a picture.
pub const TEXT_MEDIA_TYPE: &str = "text/plain";

pub const PDF_MEDIA_TYPE: &str = "application/pdf";

/// The largest text file that may be attached: 256 KiB.
///
/// A text file travels inside the conversation, so this is really a limit on how much of the
/// context window one file may take. At roughly four bytes to a token, 256 KiB is about 64,000
/// tokens — half of a 128k window, the smallest in common use among models that can do real work.
/// A larger file would leave too little room for the conversation around it and fail as a context
/// overflow, which arrives after the request is spent and says nothing about which file caused it.
const MAX_TEXT_BYTES: usize = 256 * 1024;

/// The largest PDF that may be attached, measured the way the providers measure it: on the base64
/// payload, which is four bytes for every three of the file.
///
/// The three wire formats publish different ceilings, and the lowest decides:
/// - Anthropic: 32 MB for the whole request, and at most 600 pages (100 when the context window is
///   under 1M tokens). <https://platform.claude.com/docs/en/build-with-claude/pdf-support>
/// - OpenAI: each file under 50 MB, and 50 MB across all files in a request.
///   <https://developers.openai.com/api/docs/guides/pdf-files>
/// - Gemini: 50 MB for an inline PDF, or 1000 pages.
///   <https://ai.google.dev/gemini-api/docs/file-input-methods>
///
/// Counted in decimal megabytes because that is how the limits are published, and reading "32 MB"
/// as 32 MiB would let through a file the server refuses. Anthropic's figure covers everything in
/// the request, so a PDF at the cap sent with a long conversation can still be refused — but by
/// the provider, which says why. Pages are not counted here: that needs a PDF parser, and the
/// provider's own refusal names the limit.
const MAX_PDF_BASE64_BYTES: usize = 32 * 1000 * 1000;

/// The same budget expressed in bytes of the original file, for checking before encoding.
const MAX_PDF_BYTES: usize = MAX_PDF_BASE64_BYTES / 4 * 3;

/// Whether these bytes are a PDF.
///
/// Read from the bytes rather than the extension, for the reason `image::sniff` gives: a file
/// declared as `application/pdf` that is not one comes back as a 400 blaming the document. Every
/// PDF opens with `%PDF-` followed by its version.
pub fn is_pdf(bytes: &[u8]) -> bool {
    bytes.starts_with(b"%PDF-")
}

/// Prepares one file for sending, or explains why it cannot be sent.
///
/// `accepts_pdf` is the chosen model's answer, asked when the file is picked.
pub fn attach(path: &Path, bytes: Vec<u8>, accepts_pdf: bool) -> Result<Attachment> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_owned());

    if is_pdf(&bytes) {
        if !accepts_pdf {
            bail!(
                "{name} is a PDF, and this model does not read PDFs. Choose a model that does, or \
                 attach the text instead."
            );
        }
        if bytes.len() > MAX_PDF_BYTES {
            bail!(
                "{name} is {:.1} MB, and a PDF may be at most {:.1} MB — the most every provider \
                 accepts in one request.",
                bytes.len() as f64 / 1_000_000.0,
                MAX_PDF_BYTES as f64 / 1_000_000.0,
            );
        }
        return Ok(Attachment {
            media_type: PDF_MEDIA_TYPE.to_owned(),
            data: base64::engine::general_purpose::STANDARD.encode(&bytes),
            name,
        });
    }

    // Every picture fails the text checks below, but "not UTF-8" is the wrong thing to tell someone
    // who picked a screenshot with the wrong button.
    if crate::image::sniff(&bytes).is_some() {
        bail!("{name} is an image. Attach it with the image button instead.");
    }

    let accepted = if accepts_pdf {
        "text files and PDFs"
    } else {
        "text files"
    };

    if bytes.len() > MAX_TEXT_BYTES {
        bail!(
            "{name} is {} KB, and a text file may be at most {} KB. Attach the part that matters \
             instead.",
            bytes.len().div_ceil(1024),
            MAX_TEXT_BYTES / 1024,
        );
    }

    // Valid UTF-8 and still refused: nobody writes a NUL into text, and it is the surest sign of a
    // binary format — UTF-16, a database, an object file — whose bytes happen to decode.
    if bytes.contains(&0) {
        bail!("{name} is a binary file, not text. Only {accepted} can be attached here.");
    }

    if std::str::from_utf8(&bytes).is_err() {
        bail!(
            "{name} is not UTF-8 text. Save it as UTF-8 and attach it again; only {accepted} can \
             be attached here."
        );
    }

    Ok(Attachment {
        media_type: TEXT_MEDIA_TYPE.to_owned(),
        data: base64::engine::general_purpose::STANDARD.encode(&bytes),
        name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoded(attachment: &Attachment) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD
            .decode(&attachment.data)
            .expect("attachments are stored as base64")
    }

    #[test]
    fn utf8_text_is_attached_as_text_byte_for_byte() {
        let source = "fn main() {\n    println!(\"olá\");\n}\n".as_bytes().to_vec();
        let attachment = attach(Path::new("src/main.rs"), source.clone(), false).unwrap();

        assert_eq!(attachment.media_type, TEXT_MEDIA_TYPE);
        assert_eq!(attachment.name, "main.rs");
        assert_eq!(decoded(&attachment), source);
    }

    #[test]
    fn an_empty_file_is_still_text() {
        let attachment = attach(Path::new("empty.log"), Vec::new(), false).unwrap();
        assert_eq!(attachment.media_type, TEXT_MEDIA_TYPE);
    }

    #[test]
    fn a_nul_byte_is_refused_even_though_it_is_valid_utf8() {
        // "hi" in UTF-16LE: every byte decodes as UTF-8, and the file is still not text.
        let error = attach(Path::new("notes.txt"), b"h\0i\0".to_vec(), false)
            .expect_err("a NUL byte marks a binary file");

        let message = error.to_string();
        assert!(message.contains("notes.txt"), "{message}");
        assert!(message.contains("binary"), "{message}");
    }

    #[test]
    fn invalid_utf8_is_refused_by_name() {
        // "café" in Latin-1: a real text file, in an encoding the model would receive as garbage.
        let error = attach(Path::new("menu.csv"), b"caf\xe9".to_vec(), false)
            .expect_err("Latin-1 is not UTF-8");

        let message = error.to_string();
        assert!(message.contains("menu.csv"), "{message}");
        assert!(message.contains("UTF-8"), "{message}");
    }

    #[test]
    fn text_up_to_the_cap_is_accepted_and_one_byte_more_is_refused() {
        let at_cap = vec![b'a'; MAX_TEXT_BYTES];
        assert!(attach(Path::new("big.txt"), at_cap, false).is_ok());

        let over = vec![b'a'; MAX_TEXT_BYTES + 1];
        let error = attach(Path::new("big.txt"), over, false).expect_err("over the cap");
        let message = error.to_string();
        assert!(message.contains("big.txt"), "{message}");
        assert!(message.contains("257 KB"), "{message}");
        assert!(message.contains("256 KB"), "{message}");
    }

    #[test]
    fn a_pdf_is_recognised_by_its_first_bytes() {
        assert!(is_pdf(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3"));
        assert!(!is_pdf(b"%PDF"), "a truncated header is not a PDF");
        assert!(!is_pdf(b"notes about %PDF-1.7"), "the signature must open the file");
        assert!(!is_pdf(b""));
    }

    #[test]
    fn the_bytes_decide_whether_a_file_is_a_pdf_not_its_name() {
        let pdf = attach(Path::new("report.txt"), b"%PDF-1.4\nrest".to_vec(), true).unwrap();
        assert_eq!(pdf.media_type, PDF_MEDIA_TYPE);
        assert_eq!(pdf.name, "report.txt");

        let text = attach(Path::new("notes.pdf"), b"just words".to_vec(), true).unwrap();
        assert_eq!(text.media_type, TEXT_MEDIA_TYPE);
    }

    #[test]
    fn a_pdf_is_refused_for_a_model_that_does_not_read_them() {
        let error = attach(Path::new("paper.pdf"), b"%PDF-1.7\n".to_vec(), false)
            .expect_err("this model takes no PDFs");

        let message = error.to_string();
        assert!(message.contains("paper.pdf"), "{message}");
        assert!(message.contains("does not read PDFs"), "{message}");
    }

    #[test]
    fn a_pdf_too_large_to_send_says_how_large_it_is() {
        let mut oversized = b"%PDF-1.7\n".to_vec();
        oversized.resize(MAX_PDF_BYTES + 1, b' ');
        let error = attach(Path::new("scan.pdf"), oversized, true).expect_err("over the limit");

        let message = error.to_string();
        assert!(message.contains("scan.pdf"), "{message}");
        assert!(message.contains("24.0 MB"), "{message}");
    }

    #[test]
    fn the_pdf_limit_is_the_encoded_limit_the_providers_apply() {
        let encoded = MAX_PDF_BYTES.div_ceil(3) * 4;
        assert!(
            encoded <= MAX_PDF_BASE64_BYTES,
            "{MAX_PDF_BYTES} bytes encodes to {encoded}, over the {MAX_PDF_BASE64_BYTES} allowed"
        );
    }

    #[test]
    fn a_picture_picked_with_the_file_button_is_pointed_at_the_image_button() {
        let error = attach(Path::new("shot.png"), b"\x89PNG\r\n\x1a\n\0\0".to_vec(), true)
            .expect_err("pictures go through the image path");

        assert!(error.to_string().contains("image button"), "{error}");
    }
}
