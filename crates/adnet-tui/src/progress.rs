//! Progress bar for terminal UI.
//!
//! Provides visual progress indicators for long-running operations
//! like storage usage, network transfers, etc.

use crate::color::Color;

/// Progress bar configuration.
#[derive(Debug, Clone)]
pub struct ProgressBar {
    total: u64,
    current: u64,
    width: usize,
    prefix: Option<String>,
    suffix: Option<String>,
    show_percentage: bool,
    show_values: bool,
    filled_char: char,
    empty_char: char,
}

impl Default for ProgressBar {
    fn default() -> Self {
        Self::new()
    }
}

impl ProgressBar {
    /// Create a new progress bar.
    pub fn new() -> Self {
        Self {
            total: 100,
            current: 0,
            width: 40,
            prefix: None,
            suffix: None,
            show_percentage: true,
            show_values: true,
            filled_char: '█',
            empty_char: '░',
        }
    }

    /// Create a progress bar with a known total.
    pub fn with_total(total: u64) -> Self {
        Self::new().total(total)
    }

    /// Set the total value.
    pub fn total(mut self, total: u64) -> Self {
        self.total = total;
        self
    }

    /// Set the current value.
    pub fn current(mut self, current: u64) -> Self {
        self.current = current;
        self
    }

    /// Set the bar width (number of block characters).
    pub fn width(mut self, width: usize) -> Self {
        self.width = width;
        self
    }

    /// Set a prefix (shown before the bar).
    pub fn prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = Some(prefix.into());
        self
    }

    /// Set a suffix (shown after the bar).
    pub fn suffix(mut self, suffix: impl Into<String>) -> Self {
        self.suffix = Some(suffix.into());
        self
    }

    /// Show or hide the percentage display.
    pub fn show_percentage(mut self, show: bool) -> Self {
        self.show_percentage = show;
        self
    }

    /// Show or hide the numeric values (current/total).
    pub fn show_values(mut self, show: bool) -> Self {
        self.show_values = show;
        self
    }

    /// Set custom fill characters.
    pub fn chars(mut self, filled: char, empty: char) -> Self {
        self.filled_char = filled;
        self.empty_char = empty;
        self
    }

    /// Increment the progress.
    pub fn inc(&mut self, delta: u64) {
        self.current = (self.current + delta).min(self.total);
    }

    /// Set the current progress.
    pub fn set(&mut self, current: u64) {
        self.current = current.min(self.total);
    }

    /// Calculate the percentage (0.0 to 1.0).
    fn percentage(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        self.current as f64 / self.total as f64
    }

    /// Render the progress bar as a string.
    pub fn render(&self) -> String {
        let ratio = self.percentage();
        let filled = (ratio * self.width as f64) as usize;
        let empty = self.width - filled;

        // Build the bar
        let bar = format!(
            "{}{}",
            self.filled_char.to_string().repeat(filled),
            self.empty_char.to_string().repeat(empty)
        );

        // Color the bar based on percentage
        let bar_color = if ratio >= 0.9 {
            Color::Red.paint(&bar).ansi()
        } else if ratio >= 0.7 {
            Color::Yellow.paint(&bar).ansi()
        } else {
            Color::Green.paint(&bar).ansi()
        };

        // Format percentage
        let pct_str = if self.show_percentage {
            format!(" {:5.1}% ", ratio * 100.0)
        } else {
            String::new()
        };

        // Format values
        let values_str = if self.show_values {
            format!(
                " {}/{}",
                human_bytes(self.current),
                human_bytes(self.total)
            )
        } else {
            String::new()
        };

        // Combine parts
        let prefix_str = self.prefix.as_ref().map(|p| format!("{} ", p)).unwrap_or_default();
        let suffix_str = self.suffix.as_ref().map(|s| format!(" {}", s)).unwrap_or_default();

        format!(
            "{}{}[{}]{}{}{}",
            prefix_str,
            if self.show_percentage || self.show_values { "" } else { " " },
            bar_color,
            pct_str,
            values_str,
            suffix_str
        )
    }

    /// Check if complete.
    pub fn is_complete(&self) -> bool {
        self.current >= self.total
    }
}

impl std::fmt::Display for ProgressBar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.render())
    }
}

/// Compact human-readable byte counter: 1.23 KiB / 4.56 MiB / 7.8 GiB.
pub fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.2} {}", UNITS[u])
    }
}

/// Format a number with thousands separators.
pub fn format_number(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.insert(0, ',');
        }
        result.insert(0, c);
    }
    result
}

/// Create a storage usage display with a progress bar.
pub fn storage_bar(used: u64, total: u64, label: &str) -> String {
    let pb = ProgressBar::with_total(total)
        .current(used)
        .prefix(label)
        .width(30)
        .show_percentage(true);

    format!("{}", pb)
}

/// Create a simple spinner for indeterminate progress.
pub struct Spinner {
    frames: Vec<&'static str>,
    index: usize,
    message: String,
}

impl Default for Spinner {
    fn default() -> Self {
        Self::new()
    }
}

impl Spinner {
    /// Create a new spinner.
    pub fn new() -> Self {
        Self {
            frames: vec!["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
            index: 0,
            message: String::new(),
        }
    }

    /// Set the message.
    pub fn message(mut self, msg: impl Into<String>) -> Self {
        self.message = msg.into();
        self
    }

    /// Get the current frame.
    pub fn frame(&mut self) -> String {
        let frame = self.frames[self.index];
        self.index = (self.index + 1) % self.frames.len();
        format!("{} {}", Color::Cyan.paint(frame).ansi(), self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_progress_bar_basic() {
        let pb = ProgressBar::with_total(100).current(50);
        let rendered = pb.render();
        assert!(rendered.contains("50.0%") || rendered.contains("50%"));
    }

    #[test]
    fn test_progress_bar_complete() {
        let pb = ProgressBar::with_total(100).current(100);
        let rendered = pb.render();
        assert!(rendered.contains("100.0%") || rendered.contains("100%"));
    }

    #[test]
    fn test_human_bytes() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1024), "1.00 KiB");
        assert_eq!(human_bytes(1024 * 1024), "1.00 MiB");
        assert_eq!(human_bytes(1024u64.pow(4)), "1.00 TiB");
    }

    #[test]
    fn test_format_number() {
        assert_eq!(format_number(0), "0");
        assert_eq!(format_number(1000), "1,000");
        assert_eq!(format_number(1000000), "1,000,000");
    }

    #[test]
    fn test_spinner() {
        let mut spinner = Spinner::new().message("Loading...");
        let frame = spinner.frame();
        assert!(frame.contains("Loading..."));
    }
}
