//! PROPFIND response — RFC 4918 §14.18 multistatus with extended properties.
//!
//! Supports standard DAV: properties:
//! - `resourcetype` (collection for dirs, empty for files)
//! - `getcontentlength` (file size)
//! - `getcontenttype` (MIME type, guessed from extension)
//! - `displayname` (basename)
//! - `getlastmodified` (current timestamp placeholder)
//! - `creationdate` (current timestamp placeholder)
//! - `getetag` (content hash)
//! - `supportedlock` (empty, no locking support)

use a3net_blobstore::Entry;

/// MIME type mapping for common file extensions.
fn mime_type_for_path(path: &str) -> &'static str {
    let ext = path
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        // Documents
        "txt" | "log" | "md" | "rst" => "text/plain",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" => "application/javascript",
        "json" => "application/json",
        "xml" => "application/xml",
        "csv" => "text/csv",
        // Images
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        // Video
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        // Audio
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "aac" => "audio/aac",
        // Archives
        "zip" => "application/zip",
        "tar" => "application/x-tar",
        "gz" | "gzip" => "application/gzip",
        "bz2" => "application/x-bzip2",
        "xz" => "application/x-xz",
        "7z" => "application/x-7z-compressed",
        "rar" => "application/vnd.rar",
        // Code
        "rs" => "text/x-rust",
        "py" => "text/x-python",
        "ts" => "application/typescript",
        "java" => "text/x-java",
        "c" => "text/x-c",
        "cpp" | "cc" | "cxx" => "text/x-c++src",
        "h" | "hpp" => "text/x-chdr",
        "go" => "text/x-go",
        "rb" => "text/x-ruby",
        "php" => "text/x-php",
        "sh" | "bash" => "text/x-shellscript",
        "yaml" | "yml" => "text/yaml",
        "toml" => "application/toml",
        "ini" => "text/plain",
        "cfg" | "conf" => "text/plain",
        // PDF
        "pdf" => "application/pdf",
        // Fonts
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        // Other
        _ => "application/octet-stream",
    }
}

/// Extract basename from a path string.
fn basename(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
}

/// Generate XML for a single entry's properties.
fn entry_props_xml(href: &str, entry: &Entry) -> String {
    let mut xml = String::new();
    let escaped_href = html_escape(href);
    let display_name = basename(href);

    xml.push_str(&format!(
        "  <D:response>\n    <D:href>{}</D:href>\n",
        escaped_href
    ));

    match entry {
        Entry::File { hash, size_bytes } => {
            let content_type = mime_type_for_path(href);
            let etag = format!("\"{}\"", hash.as_hex());

            xml.push_str("    <D:propstat><D:prop>\n");
            xml.push_str("      <D:resourcetype/>\n");
            xml.push_str(&format!(
                "      <D:getcontentlength>{}</D:getcontentlength>\n",
                size_bytes
            ));
            xml.push_str(&format!(
                "      <D:getcontenttype>{}</D:getcontenttype>\n",
                content_type
            ));
            xml.push_str(&format!(
                "      <D:displayname>{}</D:displayname>\n",
                html_escape(display_name)
            ));
            xml.push_str(&format!(
                "      <D:getetag>{}</D:getetag>\n",
                etag
            ));
            // Placeholder timestamps (would need manifest-level timestamps for real data)
            xml.push_str("      <D:creationdate ns0:=\"urn:schemas-gateway-com:datetypes\"/>\n");
            xml.push_str("      <D:getlastmodified ns1:=\"urn:schemas-gateway-com:datetypes\"/>\n");
            // No locking support
            xml.push_str("      <D:supportedlock/>\n");
            xml.push_str("    </D:prop><D:status>HTTP/1.1 200 OK</D:status>\n");
            xml.push_str("    </D:propstat>\n");
        }
        Entry::Directory { children } => {
            xml.push_str("    <D:propstat><D:prop>\n");
            xml.push_str("      <D:resourcetype><D:collection/></D:resourcetype>\n");
            xml.push_str(&format!(
                "      <D:displayname>{}</D:displayname>\n",
                html_escape(display_name)
            ));
            // Directory size is sum of children (approximate)
            let dir_size: u64 = children
                .values()
                .filter_map(|e| match e {
                    Entry::File { size_bytes, .. } => Some(*size_bytes),
                    Entry::Directory { .. } => None,
                })
                .sum();
            xml.push_str(&format!(
                "      <D:getcontentlength>{}</D:getcontentlength>\n",
                dir_size
            ));
            xml.push_str("      <D:getcontenttype>httpd/unix-directory</D:getcontenttype>\n");
            // No locking support
            xml.push_str("      <D:supportedlock/>\n");
            xml.push_str("    </D:prop><D:status>HTTP/1.1 200 OK</D:status>\n");
            xml.push_str("    </D:propstat>\n");
        }
    }

    xml.push_str("  </D:response>\n");
    xml
}

/// Build a complete multistatus XML response for PROPFIND.
///
/// `items` is a slice of (href, entry) pairs.
pub fn multistatus_xml(items: &[(String, &Entry)]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <D:multistatus xmlns:D=\"DAV:\" xmlns:ns0=\"urn:schemas-gateway-com:datetypes\" xmlns:ns1=\"urn:schemas-gateway-com:datetypes\">\n",
    );

    for (href, entry) in items {
        out.push_str(&entry_props_xml(href, entry));
    }

    out.push_str("</D:multistatus>\n");
    out
}

/// Build a multistatus XML for a single error response (for 404, etc.)
pub fn error_multistatus(href: &str, status: &str, error_message: &str) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <D:multistatus xmlns:D=\"DAV:\">\n",
    );

    out.push_str(&format!(
        "  <D:response>\n    <D:href>{}</D:href>\n    <D:status>{}</D:status>\n    <D:error><D:{} /></D:error>\n  </D:response>\n",
        html_escape(href),
        status,
        error_message
    ));

    out.push_str("</D:multistatus>\n");
    out
}

/// Parse Depth header value per RFC 4918.
/// - "0": only the resource itself
/// - "1": the resource and its immediate children
/// - "infinity": the resource and all descendants
/// Default is "infinity" for collections, "0" for non-collections.
pub fn parse_depth(depth_header: Option<&str>, is_directory: bool) -> Depth {
    let depth_str = match depth_header {
        Some(s) => s.trim().to_lowercase(),
        None => {
            return if is_directory { Depth::Infinity } else { Depth::Zero };
        }
    };

    match depth_str.as_str() {
        "0" => Depth::Zero,
        "1" => Depth::One,
        "infinity" => Depth::Infinity,
        "none" => Depth::None,
        _ => {
            if is_directory {
                Depth::Infinity
            } else {
                Depth::Zero
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// Only the resource itself (Depth: 0)
    Zero,
    /// The resource and its immediate children (Depth: 1)
    One,
    /// The resource and all descendants (Depth: infinity)
    Infinity,
    /// No resources (Depth: none)
    None,
}

impl Depth {
    pub fn as_header(&self) -> &'static str {
        match self {
            Depth::Zero => "0",
            Depth::One => "1",
            Depth::Infinity => "infinity",
            Depth::None => "none",
        }
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_types::ContentHash;

    #[test]
    fn multistatus_includes_file_and_dir() {
        let h = ContentHash::from_hex(
            "0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap();
        let file = Entry::File {
            hash: h,
            size_bytes: 42,
        };
        let dir = Entry::Directory {
            children: Default::default(),
        };
        let xml = multistatus_xml(&[
            ("/photos/a.jpg".to_string(), &file),
            ("/photos".to_string(), &dir),
        ]);
        assert!(xml.contains("<D:multistatus"));
        assert!(xml.contains("getcontentlength>42"));
        assert!(xml.contains("<D:collection/>"));
        assert!(xml.contains("/photos/a.jpg"));
        assert!(xml.contains("image/jpeg"));
    }

    #[test]
    fn mime_type_guessing() {
        assert_eq!(mime_type_for_path("/foo/bar.txt"), "text/plain");
        assert_eq!(mime_type_for_path("/foo/bar.html"), "text/html");
        assert_eq!(mime_type_for_path("/foo/bar.json"), "application/json");
        assert_eq!(mime_type_for_path("/foo/bar.jpg"), "image/jpeg");
        assert_eq!(mime_type_for_path("/foo/bar.mp4"), "video/mp4");
        assert_eq!(mime_type_for_path("/foo/bar.rs"), "text/x-rust");
        assert_eq!(mime_type_for_path("/foo/bar.unknown"), "application/octet-stream");
    }

    #[test]
    fn displayname_from_path() {
        assert_eq!(basename("/photos/summer.jpg"), "summer.jpg");
        assert_eq!(basename("/photos/"), "photos");
        assert_eq!(basename("/"), "");
    }

    #[test]
    fn depth_parsing() {
        assert_eq!(parse_depth(Some("0"), false), Depth::Zero);
        assert_eq!(parse_depth(Some("1"), true), Depth::One);
        assert_eq!(parse_depth(Some("infinity"), true), Depth::Infinity);
        assert_eq!(parse_depth(None, true), Depth::Infinity);
        assert_eq!(parse_depth(None, false), Depth::Zero);
        assert_eq!(parse_depth(Some("garbage"), true), Depth::Infinity);
    }

    #[test]
    fn error_multistatus_format() {
        let xml = error_multistatus("/missing.txt", "HTTP/1.1 404 Not Found", "not-found");
        assert!(xml.contains("<D:multistatus"));
        assert!(xml.contains("/missing.txt"));
        assert!(xml.contains("404"));
        assert!(xml.contains("not-found"));
    }
}
