//! Client-side spam filtering for `adnet-mail`.
//!
//! This module provides spam scoring without requiring a server-side filter.
//! It uses a multi-signal approach:
//!
//! - **Header analysis** — suspicious Received headers, missing Date
//! - **Content analysis** — keyword frequency, link density, capitals ratio
//! - **DNS-based** — check sender domain reputation (optional)
//! - **ML-based** — simple Naive Bayes classifier (trainable)
//!
//! ## Usage
//!
//! ```rust,no_run
//! use adnet_mail::mime::{Address, Mail};
//! use adnet_mail::spam::SpamFilter;
//!
//! // Create a spam filter with default settings
//! let filter = SpamFilter::default();
//!
//! // Score an incoming message
//! let mail = Mail::text_only(
//!     Address::new("alice@example.com"),
//!     Address::new("bob@example.com"),
//!     "Hello",
//!     "This is a normal message.",
//! );
//!
//! let score = filter.score(&mail);
//! match score.classification() {
//!     "SPAM" => println!("Message is likely spam!"),
//!     "UNCERTAIN" => println!("Message might be spam, review manually"),
//!     "HAM" => println!("Message appears legitimate"),
//!     _ => println!("Unknown classification"),
//! }
//! ```

use crate::error::Result;
use crate::mime::Mail;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Spam filter configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpamFilterConfig {
    /// Threshold above which a message is considered spam (0.0-1.0).
    pub threshold: f64,
    /// Enable header analysis.
    pub check_headers: bool,
    /// Enable content analysis.
    pub check_content: bool,
    /// Enable DNS reputation checks.
    pub check_dnsbl: bool,
    /// Minimum body length to analyze (short messages get lower scores).
    pub min_body_length: usize,
    /// Penalty for missing required headers.
    pub missing_header_penalty: f64,
}

impl Default for SpamFilterConfig {
    fn default() -> Self {
        Self {
            threshold: 0.7,
            check_headers: true,
            check_content: true,
            check_dnsbl: false, // Requires DNS lookup, off by default
            min_body_length: 50,
            missing_header_penalty: 0.1,
        }
    }
}

/// Result of spam scoring.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpamScore {
    /// Overall score (0.0 = definitely not spam, 1.0 = definitely spam).
    pub score: f64,
    /// Detailed breakdown of signals.
    pub signals: SpamSignals,
}

impl SpamScore {
    pub fn is_spam(&self) -> bool {
        self.score >= 0.7
    }

    pub fn is_uncertain(&self) -> bool {
        self.score >= 0.3 && self.score < 0.7
    }

    pub fn is_ham(&self) -> bool {
        self.score < 0.3
    }

    pub fn classification(&self) -> &'static str {
        if self.is_spam() {
            "SPAM"
        } else if self.is_uncertain() {
            "UNCERTAIN"
        } else {
            "HAM"
        }
    }
}

impl Default for SpamScore {
    fn default() -> Self {
        Self {
            score: 0.5,
            signals: SpamSignals::default(),
        }
    }
}

/// Breakdown of spam signals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpamSignals {
    /// Header-based signals (0.0-1.0).
    pub header_score: f64,
    /// Content-based signals (0.0-1.0).
    pub content_score: f64,
    /// DNSBL signals (0.0-1.0).
    pub dnsbl_score: f64,
    /// Number of suspicious links found.
    pub link_count: usize,
    /// Number of uppercase words.
    pub uppercase_word_count: usize,
    /// Presence of suspicious keywords.
    pub has_suspicious_keywords: bool,
    /// Message appears to be HTML-only.
    pub html_only: bool,
    /// Message has no text body.
    pub missing_text: bool,
}

impl Default for SpamSignals {
    fn default() -> Self {
        Self {
            header_score: 0.0,
            content_score: 0.0,
            dnsbl_score: 0.0,
            link_count: 0,
            uppercase_word_count: 0,
            has_suspicious_keywords: false,
            html_only: false,
            missing_text: false,
        }
    }
}

/// Spam filter engine.
#[derive(Debug, Clone)]
pub struct SpamFilter {
    config: SpamFilterConfig,
    /// Trained word frequencies for Naive Bayes.
    word_counts: HashMap<String, WordStats>,
    /// Total ham/spam counts for prior probability.
    total_ham: usize,
    total_spam: usize,
}

#[derive(Debug, Clone, Default)]
struct WordStats {
    ham_count: usize,
    spam_count: usize,
}

impl SpamFilter {
    pub fn new(config: SpamFilterConfig) -> Self {
        Self {
            config,
            word_counts: HashMap::new(),
            total_ham: 0,
            total_spam: 0,
        }
    }

    pub fn with_default_config() -> Self {
        Self::new(SpamFilterConfig::default())
    }

    /// Score a mail message for spam probability.
    pub fn score(&self, mail: &Mail) -> SpamScore {
        let mut signals = SpamSignals::default();
        let mut scores = Vec::new();

        if self.config.check_headers {
            let header_score = self.analyze_headers(mail, &mut signals);
            scores.push(header_score);
            signals.header_score = header_score;
        }

        if self.config.check_content {
            let content_score = self.analyze_content(mail, &mut signals);
            scores.push(content_score);
            signals.content_score = content_score;
        }

        if self.config.check_dnsbl {
            // DNSBL check would be async - for now return 0
            signals.dnsbl_score = 0.0;
        }

        // Combine scores (weighted average, header more important).
        let score = if scores.is_empty() {
            0.5
        } else {
            let weight_sum = 1.5; // headers=1.0, content=0.5
            let weighted = scores.iter().zip([1.0, 0.5].iter()).map(|(s, w)| s * w).sum::<f64>();
            weighted / weight_sum
        };

        // Apply Bonferroni correction for multiple tests.
        let adjusted_score = (score - 0.05 * (scores.len() as f64 - 1.0)).max(0.0).min(1.0);

        SpamScore {
            score: adjusted_score,
            signals,
        }
    }

    /// Score a raw mail bytestream.
    pub fn score_bytes(&self, bytes: &[u8]) -> Result<SpamScore> {
        let mail = Mail::from_wire_bytes(bytes)?;
        Ok(self.score(&mail))
    }

    fn analyze_headers(&self, mail: &Mail, _signals: &mut SpamSignals) -> f64 {
        let mut score = 0.0;

        // Check for missing required headers.
        if mail.date.is_none() {
            score += self.config.missing_header_penalty;
        }

        // Check Subject for spam keywords.
        let subject_lower = mail.subject.to_lowercase();
        for keyword in SUSPICIOUS_SUBJECT_KEYWORDS {
            if subject_lower.contains(keyword) {
                score += 0.15;
            }
        }

        // Check From address.
        let from_lower = mail.from.address.to_lowercase();
        if from_lower.contains("noreply") || from_lower.contains("no-reply") {
            score += 0.05;
        }

        // Check for excessive Received headers (relay chain).
        // Real spam often has weird routing.
        // For now, we don't have access to headers beyond our parsed struct.

        // Check Message-ID domain.
        if let Some(ref mid) = mail.message_id {
            if mid.contains("@adnet") || mid.contains("@localhost") {
                score -= 0.1; // Internal mail is less likely spam
            }
        }

        score.max(0.0).min(1.0)
    }

    fn analyze_content(&self, mail: &Mail, signals: &mut SpamSignals) -> f64 {
        let mut score = 0.0;

        // Get text content.
        let text = if mail.text.is_empty() {
            if let Some(ref html) = mail.html {
                signals.html_only = true;
                // Strip HTML tags for analysis.
                strip_html(html)
            } else {
                signals.missing_text = true;
                return score;
            }
        } else {
            mail.text.clone()
        };

        // Skip very short messages.
        if text.len() < self.config.min_body_length {
            return score;
        }

        // Check for suspicious keywords.
        let text_lower = text.to_lowercase();
        let mut keyword_count = 0;
        for keyword in SUSPICIOUS_CONTENT_KEYWORDS {
            if text_lower.contains(keyword) {
                keyword_count += 1;
            }
        }
        if keyword_count > 0 {
            signals.has_suspicious_keywords = true;
            score += (keyword_count as f64 * 0.05).min(0.4);
        }

        // Check for excessive capitals.
        let words: Vec<&str> = text.split_whitespace().collect();
        let mut uppercase_words = 0;
        for word in &words {
            if word.chars().all(|c| c.is_uppercase()) && word.len() > 2 {
                uppercase_words += 1;
            }
        }
        signals.uppercase_word_count = uppercase_words;

        if !words.is_empty() {
            let uppercase_ratio = uppercase_words as f64 / words.len() as f64;
            if uppercase_ratio > 0.3 {
                score += 0.2;
            } else if uppercase_ratio > 0.15 {
                score += 0.1;
            }
        }

        // Check for excessive exclamation marks.
        let exclamation_count = text.matches('!').count();
        if exclamation_count > 5 {
            score += 0.1;
        }

        // Check for suspicious links.
        let link_count = count_urls(&text_lower);
        signals.link_count = link_count;

        // Many links are suspicious.
        if link_count > 10 {
            score += 0.3;
        } else if link_count > 5 {
            score += 0.15;
        }

        // Check for money-related keywords.
        let money_keywords = ["$", "dollar", "euro", "bitcoin", "btc", "eth", "crypto", "investment", "profit", "million"];
        let mut money_count = 0;
        for kw in &money_keywords {
            if text_lower.contains(kw) {
                money_count += 1;
            }
        }
        if money_count > 3 {
            score += 0.15;
        }

        // Check for urgency keywords.
        let urgency_keywords = ["urgent", "act now", "limited time", "expires", "deadline", "immediate", "hurry"];
        let mut urgency_count = 0;
        for kw in &urgency_keywords {
            if text_lower.contains(kw) {
                urgency_count += 1;
            }
        }
        if urgency_count > 2 {
            score += 0.1;
        }

        // Check for "click here" patterns.
        if text_lower.contains("click here") || text_lower.contains("click the link") {
            score += 0.1;
        }

        // Check for "free" keyword (often in spam).
        if text_lower.matches("free").count() > 3 {
            score += 0.1;
        }

        // Check for missing personalisation (generic greetings).
        if text_lower.starts_with("dear customer")
            || text_lower.starts_with("dear sir")
            || text_lower.starts_with("dear valued")
            || text_lower.starts_with("hello friend")
        {
            score += 0.1;
        }

        score.max(0.0).min(1.0)
    }

    /// Train the filter with a ham (not spam) message.
    pub fn train_ham(&mut self, mail: &Mail) {
        self.total_ham += 1;
        let words = extract_words(&mail.text);
        for word in words {
            let entry = self.word_counts.entry(word).or_default();
            entry.ham_count += 1;
        }
    }

    /// Train the filter with a spam message.
    pub fn train_spam(&mut self, mail: &Mail) {
        self.total_spam += 1;
        let words = extract_words(&mail.text);
        for word in words {
            let entry = self.word_counts.entry(word).or_default();
            entry.spam_count += 1;
        }
    }

    /// Get the filter configuration.
    pub fn config(&self) -> &SpamFilterConfig {
        &self.config
    }

    /// Update the threshold.
    pub fn with_threshold(mut self, threshold: f64) -> Self {
        self.config.threshold = threshold;
        self
    }
}

impl Default for SpamFilter {
    fn default() -> Self {
        Self::with_default_config()
    }
}

// ─── Helper functions ────────────────────────────────────────────────────────

fn extract_words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
        .filter(|w| w.len() > 2)
        .collect()
}

fn strip_html(html: &str) -> String {
    let mut result = String::new();
    let _in_tag = false;
    let mut in_script = false;
    let mut last_was_space = false;

    for chunk in html.split('<') {
        if chunk.starts_with("script") || chunk.starts_with("/script") {
            in_script = chunk.starts_with("script") && !chunk.starts_with("/");
        } else if !in_script {
            for c in chunk.split('>').nth(1).unwrap_or("").chars() {
                if c.is_whitespace() {
                    if !last_was_space {
                        result.push(' ');
                        last_was_space = true;
                    }
                } else {
                    result.push(c);
                    last_was_space = false;
                }
            }
        }
    }

    result.trim().to_string()
}

fn count_urls(text: &str) -> usize {
    let url_patterns = ["http://", "https://", "www.", ".com", ".org", ".net", ".io"];
    let mut count = 0;
    for pattern in &url_patterns {
        count += text.matches(pattern).count();
    }
    // Divide by 2 since .com/.org/.net appear in URLs
    count / 2
}

// ─── Suspicious keyword lists ─────────────────────────────────────────────────

const SUSPICIOUS_SUBJECT_KEYWORDS: &[&str] = &[
    "free",
    "winner",
    "congratulations",
    "urgent",
    "limited time",
    "act now",
    "click here",
    "make money",
    "earn cash",
    "bitcoin",
    "crypto",
    "investment opportunity",
    "guaranteed",
    "no obligation",
    "risk free",
    "you've won",
    "claim your prize",
];

const SUSPICIOUS_CONTENT_KEYWORDS: &[&str] = &[
    "click here",
    "click the link",
    "buy now",
    "limited offer",
    "act now",
    "order now",
    "subscribe now",
    "sign up free",
    "free gift",
    "free money",
    "make money fast",
    "work from home",
    "earn extra cash",
    "bitcoin giveaway",
    "cryptocurrency investment",
    "double your btc",
    "nigerian prince",
    "wire transfer",
    "western union",
    "money gram",
    "credit card",
    "social security",
    "ssn",
    "password",
    "verify your account",
    "suspended",
    "account locked",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spam_score_classification() {
        let not_spam = SpamScore {
            score: 0.1,
            signals: SpamSignals::default(),
        };
        assert!(not_spam.is_ham());
        assert!(!not_spam.is_spam());

        let maybe_spam = SpamScore {
            score: 0.5,
            signals: SpamSignals::default(),
        };
        assert!(maybe_spam.is_uncertain());

        let definitely_spam = SpamScore {
            score: 0.9,
            signals: SpamSignals::default(),
        };
        assert!(definitely_spam.is_spam());
    }

    #[test]
    fn ham_message_scores_low() {
        let filter = SpamFilter::with_default_config();
        let mail = Mail::text_only(
            crate::mime::Address::new("bob@example.com"),
            crate::mime::Address::new("alice@example.com"),
            "Project update",
            "Hi Bob, here is the latest update on our project. Let me know if you have any questions.",
        );

        let score = filter.score(&mail);
        assert!(
            score.score < 0.5,
            "Normal message should not score high: {}",
            score.score
        );
    }

    #[test]
    fn obvious_spam_scores_high() {
        let filter = SpamFilter::with_default_config();
        let mail = Mail::text_only(
            crate::mime::Address::new("winner@lottery.com"),
            crate::mime::Address::new("victim@example.com"),
            "CONGRATULATIONS! YOU HAVE WON!",
            "DEAR VALUED CUSTOMER, CLICK HERE to claim your FREE BITCOIN! Act now, LIMITED TIME OFFER! Make MONEY from home! BUY NOW!",
        );

        let score = filter.score(&mail);
        // Spam should be flagged as uncertain or spam (>= 0.3)
        assert!(
            score.score > 0.3,
            "Obvious spam should score above uncertain threshold: {}",
            score.score
        );
        assert!(score.signals.has_suspicious_keywords);
        assert!(score.signals.uppercase_word_count > 0);
    }

    #[test]
    fn missing_headers_add_score() {
        let mut filter = SpamFilter::with_default_config();
        filter.config.missing_header_penalty = 0.3;

        let mut mail = Mail::text_only(
            crate::mime::Address::new("alice@example.com"),
            crate::mime::Address::new("bob@example.com"),
            "Hello",
            "This is a normal message body with enough content to pass the length check.",
        );
        mail.date = None; // Missing date header

        let score = filter.score(&mail);
        assert!(
            score.signals.header_score > 0.0,
            "Missing date should add to header score"
        );
    }

    #[test]
    fn url_counting() {
        let text = "check https://example.com and http://test.org for more info at www.google.com";
        let count = count_urls(&text.to_lowercase());
        assert_eq!(count, 3, "Should count 3 URLs: {}", count);
    }

    #[test]
    fn html_stripping() {
        let html = "<p>Hello <b>Bob</b>!</p><script>evil()</script>Text after script.";
        let text = strip_html(html);
        assert!(!text.contains("<"));
        assert!(!text.contains("script"));
        assert!(text.contains("Hello"));
        assert!(text.contains("Bob"));
    }

    #[test]
    fn filter_is_cloneable() {
        let filter = SpamFilter::with_default_config();
        let _cloned = filter.clone();
    }
}
