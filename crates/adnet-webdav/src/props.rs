//! PROPFIND response — minimal RFC 4918 §14.18 multistatus.

use adnet_blobstore::Entry;

pub fn multistatus_xml(items: &[(String, &Entry)]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <D:multistatus xmlns:D=\"DAV:\">\n",
    );
    for (href, entry) in items {
        out.push_str(&format!("  <D:response>\n    <D:href>{}</D:href>\n", html_escape(href)));
        match entry {
            Entry::File { hash, size_bytes } => {
                out.push_str(&format!(
                    "    <D:propstat><D:prop>\
                     <D:resourcetype/>\
                     <D:getcontentlength>{size}</D:getcontentlength>\
                     <D:getcontenttype>application/octet-stream</D:getcontenttype>\
                     </D:prop>\
                     <D:status>HTTP/1.1 200 OK</D:status>\
                     </D:propstat>\n",
                    size = size_bytes,
                ));
                let _ = hash; // exposed via D:sourceuri in a future revision
            }
            Entry::Directory { children } => {
                out.push_str("    <D:propstat><D:prop>\
                     <D:resourcetype><D:collection/></D:resourcetype>\
                     </D:prop>\
                     <D:status>HTTP/1.1 200 OK</D:status>\
                     </D:propstat>\n");
                let _ = children;
            }
        }
        out.push_str("  </D:response>\n");
    }
    out.push_str("</D:multistatus>\n");
    out
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
    use adnet_types::ContentHash;

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
    }
}
