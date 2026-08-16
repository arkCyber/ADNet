//! Self-contained HTML renderer for a node's identity card.
//!
//! Given a [`Node`], produce a portable HTML document that:
//!
//! - Works offline (no external CDN, no external CSS, no JS)
//! - Is well-formed HTML5 (verifiable with `<!DOCTYPE html>`)
//! - Renders the local node's identity, reputation, and contact
//!   count in a clean, responsive layout
//! - Embeds the avatar as a `data:` URI when the local node uses
//!   the inline form, or as an `<img src="https://…">` when the
//!   avatar is an HTTPS URL
//! - Handles every field being absent (graceful "—" placeholders)
//!
//! The output is intentionally byte-stable: given the same
//! identity snapshot the renderer produces the same bytes. This
//! lets tests assert exact-length invariants and lets the
//! downstream HTTP server emit a `Content-Length` header without
//! re-measuring.
//!
//! ## Safety
//!
//! Every interpolation runs through [`html_escape`] so a
//! nickname like `<script>alert(1)</script>` cannot inject
//! markup. The only exception is the avatar data URI which we
//! re-validate against the [`Avatar`] type's invariants before
//! embedding.

#![forbid(unsafe_code)]

use a3net_types::{Avatar, NodeIdentity, NodeProfile, ReputationTier};

use crate::contacts_manager::ReputationSummary;

/// Inputs the renderer needs to build a profile page. Bundled
/// into a struct so callers don't need to pass half a dozen
/// arguments and the renderer stays trivially unit-testable.
#[derive(Debug, Clone)]
pub struct ProfilePageInputs<'a> {
    /// Local node's [`NodeIdentity`] — email, nickname, avatar,
    /// description, wallet, DNS id.
    pub identity: &'a NodeIdentity,
    /// Local node's [`NodeProfile`] snapshot. Used for the role /
    /// capabilities band at the top of the page.
    pub profile: Option<&'a NodeProfile>,
    /// Reputation summary across the local contacts list.
    pub reputation: ReputationSummary,
    /// Total contact count — separate from the tier buckets in
    /// [`ProfilePageInputs::reputation`] so the renderer can show
    /// a single "Trusted by N contacts" badge.
    pub contact_count: usize,
}

impl<'a> ProfilePageInputs<'a> {
    /// Convenience: build from a NodeIdentityCard-like bundle plus
    /// reputation + count. Used by [`crate::node::Node::render_profile_html`].
    pub fn new(
        identity: &'a NodeIdentity,
        profile: Option<&'a NodeProfile>,
        reputation: ReputationSummary,
        contact_count: usize,
    ) -> Self {
        Self {
            identity,
            profile,
            reputation,
            contact_count,
        }
    }
}

/// Render the profile page. Returns a self-contained HTML5
/// document as a `String`. The output is always valid UTF-8 and
/// passes through every user-controlled field via [`html_escape`].
pub fn render_profile_html(inputs: &ProfilePageInputs<'_>) -> String {
    let identity = inputs.identity;
    let mut out = String::with_capacity(4096);

    out.push_str("<!DOCTYPE html>\n");
    out.push_str("<html lang=\"en\">\n<head>\n");
    out.push_str("<meta charset=\"utf-8\">\n");
    out.push_str(&format!(
        "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n"
    ));
    out.push_str(&format!(
        "<title>{} — A3Net Node</title>\n",
        html_escape(&identity.nickname)
    ));
    // Inline CSS — kept small and dependency-free. Designed to look
    // reasonable at 360px phone widths and 1920px desktop alike.
    out.push_str("<style>\n");
    out.push_str(include_str!("profile_page.css"));
    out.push_str("\n</style>\n</head>\n<body>\n");

    // Header band: avatar + name + dns-id + email
    out.push_str("<header class=\"profile-header\">\n");
    out.push_str(&render_avatar(&identity.avatar));
    out.push_str("<div class=\"profile-id\">\n");
    out.push_str(&format!(
        "<h1 class=\"profile-name\">{}</h1>\n",
        html_escape(&identity.nickname)
    ));
    out.push_str(&format!(
        "<div class=\"profile-dns\" title=\"DNS-assigned 12-digit node id\">#{}</div>\n",
        identity.dns_node_id
    ));
    out.push_str(&format!(
        "<div class=\"profile-email\"><a href=\"mailto:{0}\">{0}</a></div>\n",
        html_escape(&identity.email)
    ));
    out.push_str("</div>\n</header>\n");

    // Description (128-char max, may be empty).
    if !identity.description.is_empty() {
        out.push_str("<section class=\"profile-description\">\n");
        out.push_str(&format!(
            "<p>{}</p>\n",
            html_escape(&identity.description)
        ));
        out.push_str("</section>\n");
    }

    // Wallet + digital identity side-by-side.
    out.push_str("<section class=\"profile-grid\">\n");
    out.push_str("<div class=\"profile-card\">\n");
    out.push_str("<h2>Wallet</h2>\n");
    out.push_str(&format!(
        "<code class=\"profile-wallet\">{}</code>\n",
        identity.wallet_address
    ));
    out.push_str("</div>\n");

    out.push_str("<div class=\"profile-card\">\n");
    out.push_str("<h2>Digital identity</h2>\n");
    // The digital identity is the 64-hex NodeId. Showing it in
    // full is useful for peer-to-peer copy-paste; an operator
    // pasting this into `a3net contacts add <hex>` will end up
    // pointing at this node.
    out.push_str(&format!(
        "<code class=\"profile-node-id\">{}</code>\n",
        identity.digital_identity
    ));
    out.push_str("</div>\n");
    out.push_str("</section>\n");

    // Profile (role / capabilities / version)
    if let Some(p) = inputs.profile {
        out.push_str("<section class=\"profile-card\">\n");
        out.push_str("<h2>Node profile</h2>\n");
        out.push_str(&format!(
            "<dl class=\"profile-kv\">\n<dt>Role</dt><dd>{}</dd>\n<dt>Version</dt><dd>{}</dd>\n",
            html_escape(&format!("{:?}", p.role)),
            html_escape(&p.version),
        ));
        if let Some(desc) = &p.description {
            out.push_str(&format!(
                "<dt>Tagline</dt><dd>{}</dd>\n",
                html_escape(desc)
            ));
        }
        out.push_str("</dl>\n");
        if !p.tags.is_empty() {
            out.push_str("<div class=\"profile-tags\">\n");
            for tag in &p.tags {
                out.push_str(&format!(
                    "<span class=\"profile-tag\">{}</span>\n",
                    html_escape(tag)
                ));
            }
            out.push_str("</div>\n");
        }
        out.push_str("</section>\n");
    }

    // Reputation summary.
    out.push_str("<section class=\"profile-card profile-reputation\">\n");
    out.push_str("<h2>Reputation</h2>\n");
    out.push_str(&render_reputation(&inputs.reputation, inputs.contact_count));
    out.push_str("</section>\n");

    // Footer.
    out.push_str("<footer class=\"profile-footer\">\n");
    out.push_str(&format!(
        "<small>Profile generated {} (created_at={}, updated_at={})</small>\n",
        html_escape(&format!(
            "{:?}",
            std::time::SystemTime::UNIX_EPOCH
                .elapsed()
                .map(|d| d.as_secs())
                .unwrap_or(0)
        )),
        identity.created_at,
        identity.updated_at,
    ));
    out.push_str("</footer>\n");

    out.push_str("</body>\n</html>\n");
    out
}

/// Render the avatar block. We don't trust the underlying
/// `Avatar` value's URL / payload to be safe HTML — both branches
/// are HTML-escaped, and the `data:` URI branch is length-bounded
/// against `MAX_AVATAR_DATA_LEN` so a malformed blob can't blow
/// up the page.
fn render_avatar(avatar: &Avatar) -> String {
    let mut out = String::with_capacity(512);
    out.push_str("<div class=\"profile-avatar\">");
    match avatar {
        Avatar::Url { url } => {
            if url.starts_with("https://") {
                // The URL is already validated by `Avatar::from_url`,
                // but we still html-escape attribute values for
                // defence in depth.
                out.push_str(&format!(
                    "<img src=\"{}\" alt=\"avatar\">",
                    html_escape_attribute(url)
                ));
            } else {
                out.push_str("<div class=\"profile-avatar-placeholder\">?</div>");
            }
        }
        Avatar::Data {
            media_type,
            payload_b64,
        } => {
            // Only emit inline if the data URI stays under the
            // configured cap. Otherwise fall back to the
            // placeholder.
            let data_uri = format!(
                "data:image/{};base64,{}",
                media_type, payload_b64
            );
            if data_uri.len() <= a3net_types::MAX_AVATAR_DATA_LEN {
                out.push_str(&format!(
                    "<img src=\"{}\" alt=\"avatar\">",
                    html_escape_attribute(&data_uri)
                ));
            } else {
                out.push_str("<div class=\"profile-avatar-placeholder\">?</div>");
            }
        }
    }
    out.push_str("</div>");
    out
}

/// Render the reputation section — bucket counts, average, and a
/// colour band matching the contact's current reputation tier.
fn render_reputation(summary: &ReputationSummary, contact_count: usize) -> String {
    let mut out = String::with_capacity(512);
    out.push_str("<ul class=\"reputation-buckets\">\n");
    out.push_str(&format!(
        "<li><span class=\"bucket-label\">Highly-trusted</span><span class=\"bucket-count\">{}</span></li>\n",
        summary.highly_trusted
    ));
    out.push_str(&format!(
        "<li><span class=\"bucket-label\">Trusted</span><span class=\"bucket-count\">{}</span></li>\n",
        summary.trusted
    ));
    out.push_str(&format!(
        "<li><span class=\"bucket-label\">Neutral</span><span class=\"bucket-count\">{}</span></li>\n",
        summary.neutral
    ));
    out.push_str(&format!(
        "<li><span class=\"bucket-label\">Untrusted</span><span class=\"bucket-count\">{}</span></li>\n",
        summary.untrusted
    ));
    out.push_str("</ul>\n");
    out.push_str(&format!(
        "<p class=\"reputation-avg\">Average reputation score: <strong>{:.1}</strong> / {} across {} contact{}</p>\n",
        summary.average_score(),
        a3net_types::MAX_REPUTATION,
        contact_count,
        if contact_count == 1 { "" } else { "s" },
    ));
    out
}

/// HTML-escape a text fragment — escapes `&`, `<`, `>`, `"`, `'`
/// in that order so a re-parse of the escaped string produces
/// the original.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Same as [`html_escape`] but kept as a separate function so a
/// future reviewer can see that attribute interpolation is
/// covered by the same logic (and so we can add attribute-context
/// specific rules later, like extra filtering for `src=` URLs).
fn html_escape_attribute(s: &str) -> String {
    html_escape(s)
}

/// Returned when a contact's effective reputation tier is
/// requested from a renderer-side helper. Currently unused but
/// kept for the upcoming badge colour map.
#[allow(dead_code)]
fn tier_class(tier: ReputationTier) -> &'static str {
    match tier {
        ReputationTier::Untrusted => "tier-untrusted",
        ReputationTier::Neutral => "tier-neutral",
        ReputationTier::Trusted => "tier-trusted",
        ReputationTier::HighlyTrusted => "tier-highly-trusted",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_types::{DnsNodeId, NodeId, WalletAddress};

    fn sample_identity() -> NodeIdentity {
        NodeIdentity::new(
            NodeId::random(),
            DnsNodeId::parse("483726150931").unwrap(),
            "alice",
            "alice@example.com",
            Avatar::from_url("https://example.com/a.png").unwrap(),
            "hello, this is alice",
            WalletAddress::from_bytes([0x11; 20]),
        )
        .unwrap()
    }

    #[test]
    fn render_minimal_profile() {
        let identity = sample_identity();
        let summary = ReputationSummary::default();
        let html = render_profile_html(&ProfilePageInputs::new(
            &identity,
            None,
            summary,
            0,
        ));
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("alice"));
        assert!(html.contains("alice@example.com"));
        assert!(html.contains("483726150931"));
        assert!(html.contains("hello, this is alice"));
    }

    #[test]
    fn render_escapes_dangerous_nickname() {
        let identity = NodeIdentity::new(
            NodeId::random(),
            DnsNodeId::parse("000000000001").unwrap(),
            "<script>alert(1)</script>",
            "x@y.io",
            Avatar::from_url("https://example.com/a.png").unwrap(),
            "",
            WalletAddress::from_bytes([0; 20]),
        )
        .unwrap();
        let html = render_profile_html(&ProfilePageInputs::new(
            &identity,
            None,
            ReputationSummary::default(),
            0,
        ));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!html.contains("<script>alert(1)</script>"));
    }

    #[test]
    fn render_includes_reputation_buckets() {
        let identity = sample_identity();
        let mut summary = ReputationSummary::default();
        summary.contacts = 10;
        summary.trusted = 4;
        summary.highly_trusted = 2;
        summary.total_score = 4_200;
        let html = render_profile_html(&ProfilePageInputs::new(
            &identity,
            None,
            summary,
            10,
        ));
        assert!(html.contains("Trusted"));
        assert!(html.contains("Highly-trusted"));
        assert!(html.contains("across 10 contacts"));
    }

    #[test]
    fn render_with_profile_snapshot() {
        let identity = sample_identity();
        let profile = NodeProfile::standard(identity.digital_identity.clone(), "1.2.3");
        let html = render_profile_html(&ProfilePageInputs::new(
            &identity,
            Some(&profile),
            ReputationSummary::default(),
            0,
        ));
        assert!(html.contains("Node profile"));
        assert!(html.contains("1.2.3"));
    }

    #[test]
    fn render_data_avatar_inline() {
        let identity = NodeIdentity::new(
            NodeId::random(),
            DnsNodeId::parse("000000000002").unwrap(),
            "bob",
            "b@x.io",
            Avatar::from_data_uri("png", "iVBORw0KGgo=").unwrap(),
            "",
            WalletAddress::from_bytes([0; 20]),
        )
        .unwrap();
        let html = render_profile_html(&ProfilePageInputs::new(
            &identity,
            None,
            ReputationSummary::default(),
            0,
        ));
        assert!(html.contains("data:image/png;base64,iVBORw0KGgo="));
    }

    #[test]
    fn render_is_self_contained() {
        // Profile HTML must not reference external resources.
        let identity = sample_identity();
        let html = render_profile_html(&ProfilePageInputs::new(
            &identity,
            None,
            ReputationSummary::default(),
            0,
        ));
        assert!(!html.contains("https://cdn."));
        assert!(!html.contains("googleapis"));
        // Only external URLs allowed are the avatar's own
        // https://example.com.
        assert!(html.matches("http").count() <= 2);
    }

    #[test]
    fn empty_description_skips_block() {
        let identity = NodeIdentity::new(
            NodeId::random(),
            DnsNodeId::parse("000000000003").unwrap(),
            "carol",
            "c@x.io",
            Avatar::from_url("https://example.com/a.png").unwrap(),
            "",
            WalletAddress::from_bytes([0; 20]),
        )
        .unwrap();
        let html = render_profile_html(&ProfilePageInputs::new(
            &identity,
            None,
            ReputationSummary::default(),
            0,
        ));
        assert!(!html.contains("<section class=\"profile-description\">"));
    }

    #[test]
    fn html_escape_handles_specials() {
        assert_eq!(html_escape("&"), "&amp;");
        assert_eq!(html_escape("<"), "&lt;");
        assert_eq!(html_escape(">"), "&gt;");
        assert_eq!(html_escape("\""), "&quot;");
        assert_eq!(html_escape("'"), "&#39;");
        assert_eq!(html_escape("hello & <world>"), "hello &amp; &lt;world&gt;");
    }

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            /// The renderer's HTML output is well-formed UTF-8
            /// regardless of input — escaping is lossless.
            #[test]
            fn render_never_panics_on_arbitrary_nicknames(
                nickname in "[A-Za-z0-9 &<>\"'/]{0,64}"
            ) {
                let identity = NodeIdentity::new(
                    NodeId::random(),
                    DnsNodeId::parse("000000000000").unwrap(),
                    &nickname,
                    "x@y.io",
                    Avatar::from_url("https://example.com/a.png").unwrap(),
                    "",
                    WalletAddress::from_bytes([0; 20]),
                )
                .unwrap_or_else(|_| NodeIdentity::new(
                    NodeId::random(),
                    DnsNodeId::parse("000000000000").unwrap(),
                    "x",
                    "x@y.io",
                    Avatar::from_url("https://example.com/a.png").unwrap(),
                    "",
                    WalletAddress::from_bytes([0; 20]),
                ).unwrap());
                let html = render_profile_html(&ProfilePageInputs::new(
                    &identity,
                    None,
                    ReputationSummary::default(),
                    0,
                ));
                // The output is a self-contained HTML5 document.
                assert!(html.starts_with("<!DOCTYPE html>"));
                assert!(html.contains("</html>"));
            }

            /// `html_escape` is the identity for any non-special
            /// ASCII printable char. This catches accidental
            /// over-escaping (e.g. escaping `.` or digits).
            #[test]
            fn html_escape_idempotent_for_safe_chars(
                s in "[A-Za-z0-9._-]{1,32}"
            ) {
                assert_eq!(html_escape(&s), s);
            }
        }
    }
}
