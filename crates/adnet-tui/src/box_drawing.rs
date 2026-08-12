//! ASCII box drawing utilities for terminal UI.
//!
//! Provides functions to draw boxes, panels, and tables with
//! ASCII/Unicode characters.

use crate::color::{Color, StyledStr};

/// Border style for boxes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderStyle {
    /// Single-line ASCII box drawing characters (─ │ ┌ ┐ └ ┘)
    Single,
    /// Double-line box drawing characters (═ ║ ╔ ╗ ╚ ╝)
    Double,
    /// Simple ASCII (no special characters): + - |
    Simple,
    /// No border
    None,
}

impl Default for BorderStyle {
    fn default() -> Self {
        Self::Single
    }
}

/// Characters for different border styles.
#[derive(Debug, Clone, Copy)]
pub struct BorderChars {
    pub horizontal: &'static str,
    pub vertical: &'static str,
    pub top_left: &'static str,
    pub top_right: &'static str,
    pub bottom_left: &'static str,
    pub bottom_right: &'static str,
    pub left_tee: &'static str,
    pub right_tee: &'static str,
    pub top_tee: &'static str,
    pub bottom_tee: &'static str,
    pub cross: &'static str,
}

impl BorderChars {
    /// Get the horizontal character.
    pub fn horizontal(&self) -> &'static str {
        self.horizontal
    }
    /// Get the vertical character.
    pub fn vertical(&self) -> &'static str {
        self.vertical
    }
    /// Get the top-left corner character.
    pub fn top_left(&self) -> &'static str {
        self.top_left
    }
    /// Get the top-right corner character.
    pub fn top_right(&self) -> &'static str {
        self.top_right
    }
    /// Get the bottom-left corner character.
    pub fn bottom_left(&self) -> &'static str {
        self.bottom_left
    }
    /// Get the bottom-right corner character.
    pub fn bottom_right(&self) -> &'static str {
        self.bottom_right
    }
    /// Get the left tee character.
    pub fn left_tee(&self) -> &'static str {
        self.left_tee
    }
    /// Get the right tee character.
    pub fn right_tee(&self) -> &'static str {
        self.right_tee
    }
    /// Get the top tee character.
    pub fn top_tee(&self) -> &'static str {
        self.top_tee
    }
    /// Get the bottom tee character.
    pub fn bottom_tee(&self) -> &'static str {
        self.bottom_tee
    }
    /// Get the cross character.
    pub fn cross(&self) -> &'static str {
        self.cross
    }
}

impl BorderStyle {
    /// Get the border characters for this style.
    pub fn chars(&self) -> BorderChars {
        match self {
            BorderStyle::Single => BorderChars {
                horizontal: "─",
                vertical: "│",
                top_left: "┌",
                top_right: "┐",
                bottom_left: "└",
                bottom_right: "┘",
                left_tee: "├",
                right_tee: "┤",
                top_tee: "┬",
                bottom_tee: "┴",
                cross: "┼",
            },
            BorderStyle::Double => BorderChars {
                horizontal: "═",
                vertical: "║",
                top_left: "╔",
                top_right: "╗",
                bottom_left: "╚",
                bottom_right: "╝",
                left_tee: "╠",
                right_tee: "╣",
                top_tee: "╦",
                bottom_tee: "╩",
                cross: "╬",
            },
            BorderStyle::Simple => BorderChars {
                horizontal: "-",
                vertical: "|",
                top_left: "+",
                top_right: "+",
                bottom_left: "+",
                bottom_right: "+",
                left_tee: "+",
                right_tee: "+",
                top_tee: "+",
                bottom_tee: "+",
                cross: "+",
            },
            BorderStyle::None => BorderChars {
                horizontal: " ",
                vertical: " ",
                top_left: "",
                top_right: "",
                bottom_left: "",
                bottom_right: "",
                left_tee: "",
                right_tee: "",
                top_tee: "",
                bottom_tee: "",
                cross: " ",
            },
        }
    }
}

/// A box/panel for displaying structured data.
#[derive(Debug, Clone)]
pub struct Box {
    title: Option<String>,
    border_style: BorderStyle,
    width: Option<usize>,
    fields: Vec<(String, StyledStr)>,
    header_color: Option<Color>,
}

impl Default for Box {
    fn default() -> Self {
        Self::new()
    }
}

impl Box {
    /// Create a new empty box.
    pub fn new() -> Self {
        Self {
            title: None,
            border_style: BorderStyle::default(),
            width: None,
            fields: Vec::new(),
            header_color: None,
        }
    }

    /// Create a box with a title.
    pub fn with_title(title: impl Into<String>) -> Self {
        Self::new().title(title)
    }

    /// Set the title.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Set the border style.
    pub fn border_style(mut self, style: BorderStyle) -> Self {
        self.border_style = style;
        self
    }

    /// Set the box width (in characters).
    pub fn width(mut self, width: usize) -> Self {
        self.width = Some(width);
        self
    }

    /// Set header color.
    pub fn header_color(mut self, color: Color) -> Self {
        self.header_color = Some(color);
        self
    }

    /// Add a field (label and value).
    pub fn add_field<S: Into<StyledStr>>(mut self, label: &str, value: S) -> Self {
        self.fields.push((label.to_string(), value.into()));
        self
    }

    /// Add multiple fields from an iterator.
    pub fn add_fields<'a, I, S>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = (&'a str, S)>,
        S: Into<StyledStr>,
    {
        for (label, value) in fields {
            self = self.add_field(label, value);
        }
        self
    }

    /// Render the box as a string.
    pub fn render(&self) -> String {
        let c = self.border_style.chars();

        // Calculate dimensions
        let max_label_len = self.fields.iter()
            .map(|(l, _)| display_width(l))
            .max()
            .unwrap_or(0);
        let max_value_len = self.fields.iter()
            .map(|(_, v)| display_width(v.plain_text()))
            .max()
            .unwrap_or(0);

        // Minimum width calculation
        let min_content_width = max_label_len + 2 + max_value_len;
        let title_width = self.title.as_ref()
            .map(|t| display_width(t) + 2)
            .unwrap_or(0);

        let content_width = [self.width, Some(min_content_width), Some(title_width), Some(60)]
            .into_iter()
            .flatten()
            .max()
            .unwrap();

        let inner_width = content_width - 2;
        let mut lines = Vec::new();

        // Top border with optional title
        if let Some(title) = &self.title {
            let title_with_padding = format!(" {} ", title);
            let padding = content_width.saturating_sub(display_width(&title_with_padding));
            let left_pad = padding / 2;
            let right_pad = padding - left_pad;

            lines.push(format!(
                "{}{}{}{}{}",
                c.top_left,
                c.horizontal.repeat(left_pad),
                if let Some(color) = &self.header_color {
                    color.paint(&title_with_padding).ansi()
                } else {
                    title_with_padding.clone()
                },
                c.horizontal.repeat(right_pad),
                c.top_right
            ));
        } else {
            lines.push(format!(
                "{}{}{}",
                c.top_left,
                c.horizontal.repeat(content_width - 2),
                c.top_right
            ));
        }

        // Content lines
        for (label, value) in &self.fields {
            let value_str = value.ansi();
            let label_width = max_label_len.min(inner_width.saturating_sub(2));
            let value_width = inner_width.saturating_sub(label_width).saturating_sub(2);

            // Truncate label and value if needed
            let display_label = truncate(label, label_width);
            let _display_value = truncate(value.plain_text(), value_width);

            let line = format!(
                "{} {:label_width$} │ {}",
                c.vertical,
                display_label,
                if display_width(&value_str) > value_width {
                    // Re-truncate for styled value
                    StyledStr::plain(&truncate(value.plain_text(), value_width)).ansi()
                } else {
                    value_str
                }
            );
            lines.push(line);
        }

        // Bottom border
        lines.push(format!(
            "{}{}{}",
            c.bottom_left,
            c.horizontal.repeat(content_width - 2),
            c.bottom_right
        ));

        lines.join("\n")
    }
}

impl std::fmt::Display for Box {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.render())
    }
}

/// Calculate display width of a string (accounting for ANSI escape sequences).
fn display_width(s: &str) -> usize {
    // Remove ANSI escape sequences for width calculation
    let stripped = strip_ansi(s);
    // Count visible characters (simple UTF-8 width assuming all chars are width 1 or 2)
    stripped.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum::<usize>().min(stripped.len())
}

/// Strip ANSI escape sequences from a string.
fn strip_ansi(s: &str) -> String {
    let mut result = String::new();
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip until we find 'm'
            while let Some(&nc) = chars.peek() {
                chars.next();
                if nc == 'm' {
                    break;
                }
            }
        } else {
            result.push(c);
        }
    }

    result
}

/// Truncate a string to fit within a width.
fn truncate(s: &str, width: usize) -> String {
    let mut result = String::new();
    let mut current_width = 0;

    for c in s.chars() {
        let char_width = if c.is_ascii() { 1 } else { 2 };
        if current_width + char_width > width {
            break;
        }
        result.push(c);
        current_width += char_width;
    }

    if current_width < width && result.len() < s.len() {
        // Add ellipsis
        if result.len() >= 1 {
            result.pop();
            result.push('…');
        }
    }

    result
}

/// Create a simple status panel.
pub fn status_panel(title: &str, items: &[(&str, &str)]) -> Box {
    let mut panel = Box::with_title(title);
    for (label, value) in items {
        panel = panel.add_field(label, *value);
    }
    panel
}

/// Create an alert box with appropriate coloring.
pub fn alert_box(level: &str, message: &str) -> String {
    let (color, icon) = match level.to_lowercase().as_str() {
        "critical" | "error" => (Color::Red, "⛔"),
        "warn" | "warning" => (Color::Yellow, "⚠"),
        "info" => (Color::Cyan, "ℹ"),
        _ => (Color::White, "•"),
    };

    let icon_str = color.paint(icon);
    format!("{} {}", icon_str, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_box() {
        let b = Box::new()
            .title("Test")
            .add_field("Key", "Value");
        let rendered = b.render();
        assert!(rendered.contains("┌"));
        assert!(rendered.contains("Test"));
    }

    #[test]
    fn test_box_without_title() {
        let b = Box::new().add_field("A", "B");
        let rendered = b.render();
        assert!(rendered.contains("┌"));
        assert!(rendered.contains("└"));
    }

    #[test]
    fn test_display_width() {
        assert_eq!(display_width("hello"), 5);
        assert_eq!(display_width("中文"), 4); // 2 chars * 2 width each
    }

    #[test]
    fn test_truncate() {
        // Just verify the function works without panicking
        assert!(truncate("hello", 3).contains("hel"));
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("a", 1), "a");
        assert!(!truncate("abc", 1).is_empty());
    }
}
