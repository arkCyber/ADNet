//! SVG QR code generation.
//!
//! We use the [`qrcodegen`] crate (the same library chatmail@core uses)
//! and emit a self-contained SVG that:
//!
//! - has a square viewBox (caller chooses the size),
//! - has white background + black modules,
//! - has no external asset references,
//! - is safe to inline into a HTML document or send to a printer.
//!
//! The generator is intentionally small — chatmail@core's
//! `qr_code_generator.rs` produces a fancy 515×630 SVG with an avatar
//! overlay and a Delta-Chat footer logo. We strip both: avatars are an
//! identity concern (callers add them via `with_overlay_svg`), and the
//! footer is a chatmail branding artefact we don't want to redistribute.
//! This keeps the output interoperable (the QR payload is identical)
//! while letting higher crates compose their own branded cards.

use qrcodegen::{QrCode, QrCodeEcc};

/// Re-export of the specific `qrcodegen` enum we use for
/// error-correction level. Operators reading `QrConfigToml` would
/// otherwise need to depend on `qrcodegen` directly.
pub use qrcodegen::QrCodeEcc as QrErrorCorrectionLevel;

use crate::error::{QrError, Result};

/// Visual settings for [`create_qr_svg`].
#[derive(Debug, Clone)]
pub struct QrStyle {
    /// Pixel size of the viewBox; chatmail uses 512. Default: 512.
    pub canvas_size: u32,
    /// Module pixel size (the QR itself, leaving room for quiet zone).
    /// Default: 416 (matches chatmail).
    pub qr_size: u32,
    /// Foreground (module) colour. Default: `#000000`.
    pub fg: &'static str,
    /// Background colour. Default: `#ffffff`.
    pub bg: &'static str,
    /// Minimum error-correction level. Default: `Quartile` (matches
    /// chatmail; high enough that ~25% of the modules can be obscured
    /// before the code becomes unreadable).
    pub ecc: QrCodeEcc,
}

impl Default for QrStyle {
    fn default() -> Self {
        Self {
            canvas_size: 512,
            qr_size: 416,
            fg: "#000000",
            bg: "#ffffff",
            ecc: QrCodeEcc::Quartile,
        }
    }
}

/// Render `content` as a self-contained SVG.
///
/// # Errors
///
/// - [`QrError::ContentTooLarge`] when `content` exceeds
///   [`crate::error::MAX_QR_CONTENT`] bytes.
/// - [`QrError::Generation`] when `qrcodegen` itself fails (e.g. the
///   content is too large for the chosen ECC level — see
///   [`qrcodegen::QrCode::encode_text`] for details).
pub fn create_qr_svg(content: &str) -> Result<String> {
    create_qr_svg_with_style(content, &QrStyle::default())
}

/// Same as [`create_qr_svg`] but with explicit visual settings.
pub fn create_qr_svg_with_style(content: &str, style: &QrStyle) -> Result<String> {
    if content.len() > crate::error::MAX_QR_CONTENT {
        return Err(QrError::ContentTooLarge {
            actual: content.len(),
            limit: crate::error::MAX_QR_CONTENT,
        });
    }
    let qr = QrCode::encode_text(content, style.ecc)
        .map_err(|e| QrError::Generation(format!("encode_text: {e:?}")))?;

    let all = style.canvas_size as f32;
    let qr_size = style.qr_size as f32;
    let offset = (all - qr_size) / 2.0;
    let modules = qr.size() as f32;
    let scale = qr_size / modules;

    let mut path = String::with_capacity((qr.size() * qr.size() / 2) as usize);
    for y in 0..qr.size() {
        for x in 0..qr.size() {
            if qr.get_module(x, y) {
                path.push_str(&format!("M{x},{y}h1v1h-1z"));
            }
        }
    }

    // We deliberately do NOT use the `tagger` crate chatmail@core
    // depends on: it's a 70kLoC XML builder, and emitting 9 hardcoded
    // SVG elements by hand is simpler and avoids pulling in another
    // dependency.
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {all} {all}" xmlns:xlink="http://www.w3.org/1999/xlink"><rect x="0" y="0" width="{all}" height="{all}" style="fill:{bg}"/><g transform="translate({offset},{offset})"><path style="fill:{fg}" d="{path}" transform="scale({scale})"/></g></svg>"#,
        all = all,
        bg = style.bg,
        fg = style.fg,
        offset = offset,
        path = path,
        scale = scale,
    );
    Ok(svg)
}

/// Variant of [`create_qr_svg`] that produces a chatmail-compatible
/// 515×630 SVG with a centred card and a text description. Used by
/// the SecureJoin / Backup flows that want to mimic chatmail's
/// branding without redistributing the upstream footer logo.
///
/// `description` is split on whitespace and wrapped to ~32 chars per
/// line; a smaller font is used if the description has more than two
/// lines.
pub fn create_qr_card_svg(content: &str, description: &str) -> Result<String> {
    if content.len() > crate::error::MAX_QR_CONTENT {
        return Err(QrError::ContentTooLarge {
            actual: content.len(),
            limit: crate::error::MAX_QR_CONTENT,
        });
    }
    let qr = QrCode::encode_text(content, QrCodeEcc::Quartile)
        .map_err(|e| QrError::Generation(format!("encode_text: {e:?}")))?;

    const WIDTH: f32 = 515.0;
    const HEIGHT: f32 = 630.0;
    const QR_SIZE: f32 = 400.0;
    const QR_TRANSLATE_UP: f32 = 40.0;
    const FONT_SIZE_BIG: f32 = 27.0;
    const FONT_SIZE_SMALL: f32 = 19.0;

    let lines = wrap_text(description, 32);
    let (font_size, y_shift) = if lines.len() <= 2 {
        (FONT_SIZE_BIG, 0.0)
    } else {
        (FONT_SIZE_SMALL, -10.0)
    };

    let qr_modules = qr.size() as f32;
    let scale = QR_SIZE / qr_modules;
    let mut path = String::with_capacity((qr.size() * qr.size() / 2) as usize);
    for y in 0..qr.size() {
        for x in 0..qr.size() {
            if qr.get_module(x, y) {
                path.push_str(&format!("M{x},{y}h1v1h-1z"));
            }
        }
    }
    let qr_translate_x = (WIDTH - QR_SIZE) / 2.0;
    let qr_translate_y = (HEIGHT - QR_SIZE) / 2.0 - QR_TRANSLATE_UP;
    let text_y_base = ((HEIGHT - QR_SIZE) / 2.0) + QR_SIZE;

    let mut text_svg = String::new();
    for (count, line) in lines.iter().enumerate() {
        let y = (count as f32 * (font_size * 1.2)) + text_y_base + y_shift;
        text_svg.push_str(&format!(
            r#"<text y="{y}" x="{x}" text-anchor="middle" style="font-family:sans-serif;font-weight:bold;font-size:{font_size}px;fill:#000000;stroke:none">{line}</text>"#,
            y = y,
            x = WIDTH / 2.0,
            font_size = font_size,
            line = html_escape(line),
        ));
    }

    let svg_card = String::from(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 "#)
        + &format!("{WIDTH} {HEIGHT}")
        + r#"" xmlns:xlink="http://www.w3.org/1999/xlink"><rect x="2" y="2" rx="40" stroke=""#
        + r#"#c6c6c6" stroke-width="2" width=""#
        + &format!("{}", WIDTH - 4.0)
        + r#"" height=""#
        + &format!("{}", HEIGHT - 4.0)
        + r#"" style="fill:"#
        + "#f2f2f2"
        + r#""/><g transform="translate("#
        + &format!(
            "{qr_tx},{qr_ty}",
            qr_tx = qr_translate_x,
            qr_ty = qr_translate_y
        )
        + r#")"><path style="fill:"#
        + "#000000"
        + &format!(r#"" d="{path}" transform="scale({scale})"/></g>"#)
        + &text_svg
        + "</svg>";
    Ok(svg_card)
}

fn wrap_text(text: &str, max_chars: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.len() + 1 + word.len() <= max_chars {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_minimal_svg() {
        let svg = create_qr_svg("hello world").unwrap();
        assert!(svg.starts_with("<svg"));
        assert!(svg.contains("</svg>"));
        assert!(svg.contains("xmlns"));
    }

    #[test]
    fn svg_escapes_special_chars() {
        // The content is embedded as `d=` attribute data; `<` / `>` /
        // `&` would break the SVG. qrcodegen encodes modules as
        // path commands which happen to be ASCII-safe, but we still
        // sanity-check that the surrounding SVG is well-formed.
        let svg = create_qr_svg("test & < > \"quotes\"").unwrap();
        assert!(svg.starts_with("<svg"));
        assert!(svg.contains("viewBox"));
        // The `d=` attribute data is a path of `Mx,yh1v1h-1z` blocks —
        // there should be no raw `<` / `>` / `&` in there.
        let path = svg
            .split_once("d=\"")
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(p, _)| p.to_string())
            .unwrap_or_default();
        assert!(!path.contains('<'));
        assert!(!path.contains('>'));
        assert!(!path.contains('&'));
    }

    #[test]
    fn card_svg_has_text() {
        let svg = create_qr_card_svg("https://example.com", "Scan me please").unwrap();
        assert!(svg.starts_with("<svg"));
        assert!(svg.contains("Scan me please"));
        assert!(svg.contains("font-size"));
    }

    #[test]
    fn oversized_content_is_rejected() {
        let huge = "x".repeat(crate::error::MAX_QR_CONTENT + 1);
        assert!(matches!(
            create_qr_svg(&huge),
            Err(crate::error::QrError::ContentTooLarge { .. })
        ));
    }

    #[test]
    fn custom_style_is_honoured() {
        let svg = create_qr_svg_with_style(
            "hello",
            &QrStyle {
                canvas_size: 256,
                qr_size: 200,
                fg: "#112233",
                bg: "#ffeeaa",
                ecc: QrCodeEcc::Medium,
            },
        )
        .unwrap();
        assert!(svg.contains("viewBox=\"0 0 256 256\""));
        assert!(svg.contains("fill:#ffeeaa"));
        assert!(svg.contains("fill:#112233"));
    }

    #[test]
    fn wrap_text_balances_lines() {
        let lines = wrap_text("a b c d e f g h i j k l m n o p q r s t u v w x y z", 8);
        for line in &lines {
            assert!(line.len() <= 8, "line too long: {line:?}");
        }
        let rejoined = lines.join(" ");
        assert_eq!(
            rejoined,
            "a b c d e f g h i j k l m n o p q r s t u v w x y z"
        );
    }
}
