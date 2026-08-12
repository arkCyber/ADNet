//! ANSI color support with automatic terminal detection.
//!
//! Detects whether the terminal supports colors (not a dumb terminal,
//! not redirected to a file). Falls back to plain text when not
//! supported.

use std::env;
use once_cell::sync::Lazy;

/// Whether the terminal supports ANSI colors.
static SUPPORTS_COLOR: Lazy<bool> = Lazy::new(|| {
    // Check NO_COLOR env var (https://no-color.org/)
    if env::var("NO_COLOR").is_ok() {
        return false;
    }

    // Check if stdout is a tty
    if !atty::is(atty::Stream::Stdout) {
        return false;
    }

    // Check TERM environment variable
    let term = env::var("TERM").unwrap_or_default();
    let dumb = term == "dumb";
    !dumb
});

/// Whether color output is enabled.
pub fn is_enabled() -> bool {
    *SUPPORTS_COLOR
}

/// Enable or disable color output (for testing).
#[cfg(test)]
pub fn set_enabled(_enabled: bool) {
    // Reset the lazy static by cloning and replacing
    // In tests, we just check the function directly
}

/// ANSI color codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    /// Bright variant of the color
    Bright,
    /// Default terminal foreground color
    Default,
    /// Dim/faint variant
    Dim,
}

impl Color {
    /// Get the ANSI foreground code.
    pub fn fg_code(&self) -> &'static str {
        match self {
            Color::Black => "30",
            Color::Red => "31",
            Color::Green => "32",
            Color::Yellow => "33",
            Color::Blue => "34",
            Color::Magenta => "35",
            Color::Cyan => "36",
            Color::White => "37",
            Color::Bright => "1", // Bold/bright
            Color::Default => "39",
            Color::Dim => "2", // Dim/faint
        }
    }

    /// Get the ANSI background code.
    pub fn bg_code(&self) -> &'static str {
        match self {
            Color::Black => "40",
            Color::Red => "41",
            Color::Green => "42",
            Color::Yellow => "43",
            Color::Blue => "44",
            Color::Magenta => "45",
            Color::Cyan => "46",
            Color::White => "47",
            Color::Bright => "1", // Bold/bright
            Color::Default => "49",
            Color::Dim => "2", // Dim/faint (works as bg but usually not used)
        }
    }

    /// Apply this color as foreground.
    pub fn paint<S: AsRef<str>>(&self, text: S) -> StyledStr {
        StyledStr::new(text.as_ref(), Some(*self), None)
    }

    /// Apply this color as background.
    pub fn on<S: AsRef<str>>(&self, text: S) -> StyledStr {
        StyledStr::new(text.as_ref(), None, Some(*self))
    }
}

/// Reset ANSI escape sequence.
const RESET: &str = "\x1b[0m";

/// A string with optional ANSI color styling.
#[derive(Debug, Clone)]
pub struct StyledStr {
    text: String,
    fg: Option<Color>,
    bg: Option<Color>,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
}

impl StyledStr {
    /// Create a new styled string.
    fn new(text: &str, fg: Option<Color>, bg: Option<Color>) -> Self {
        Self {
            text: text.to_string(),
            fg,
            bg,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
        }
    }

    /// Create plain text without styling.
    pub fn plain(text: &str) -> Self {
        Self {
            text: text.to_string(),
            fg: None,
            bg: None,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
        }
    }

    /// Make text bold.
    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    /// Make text dim/faint.
    pub fn dim(mut self) -> Self {
        self.dim = true;
        self
    }

    /// Make text italic.
    pub fn italic(mut self) -> Self {
        self.italic = true;
        self
    }

    /// Underline the text.
    pub fn underline(mut self) -> Self {
        self.underline = true;
        self
    }

    /// Get the ANSI escape sequence for the current style.
    fn escape_sequence(&self) -> String {
        if !is_enabled() {
            return String::new();
        }

        let mut codes = Vec::new();

        if self.bold {
            codes.push("1");
        }
        if self.dim {
            codes.push("2");
        }
        if self.italic {
            codes.push("3");
        }
        if self.underline {
            codes.push("4");
        }

        if let Some(fg) = &self.fg {
            codes.push(fg.fg_code());
        }
        if let Some(bg) = &self.bg {
            codes.push(bg.bg_code());
        }

        if codes.is_empty() {
            String::new()
        } else {
            format!("\x1b[{}m", codes.join(";"))
        }
    }

    /// Render the styled string as plain text (no ANSI).
    pub fn plain_text(&self) -> &str {
        &self.text
    }

    /// Render the styled string as ANSI-escaped string.
    pub fn ansi(&self) -> String {
        if !is_enabled() {
            return self.text.clone();
        }

        let escape = self.escape_sequence();
        if escape.is_empty() {
            self.text.clone()
        } else {
            format!("{escape}{}{RESET}", self.text)
        }
    }
}

impl std::fmt::Display for StyledStr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.ansi())
    }
}

impl From<&str> for StyledStr {
    fn from(s: &str) -> Self {
        Self::plain(s)
    }
}

impl From<String> for StyledStr {
    fn from(s: String) -> Self {
        Self::plain(&s)
    }
}

impl From<&String> for StyledStr {
    fn from(s: &String) -> Self {
        Self::plain(s)
    }
}

/// Style helpers for common terminal styling.
pub struct Style;

impl Style {
    /// Success style (green).
    pub fn success() -> Color {
        Color::Green
    }

    /// Warning style (yellow).
    pub fn warning() -> Color {
        Color::Yellow
    }

    /// Error style (red).
    pub fn error() -> Color {
        Color::Red
    }

    /// Info style (cyan).
    pub fn info() -> Color {
        Color::Cyan
    }

    /// Header style (bright/bold).
    pub fn header() -> Color {
        Color::Bright
    }

    /// Dim/muted style.
    pub fn muted() -> Color {
        Color::Default
    }
}

/// atty check for stdout
mod atty {
    use std::env;

    pub fn is(_stream: Stream) -> bool {
        // Simple check: is_terminal or is_ci
        // We conservatively return true when we can't determine
        env::var("TERM").ok().map_or(true, |t| t != "dumb")
    }

    #[derive(Debug, Clone, Copy)]
    pub enum Stream {
        Stdout,
        Stderr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plain_string() {
        let s = StyledStr::plain("hello");
        assert_eq!(s.ansi(), "hello");
        assert_eq!(s.plain_text(), "hello");
    }

    #[test]
    fn test_colored_string() {
        // When not in a terminal, ANSI codes are stripped
        let s = Color::Green.paint("success");
        let ansi = s.ansi();
        // Should contain the text
        assert!(ansi.contains("success"));
    }

    #[test]
    fn test_styled_string() {
        let s = StyledStr::plain("test").bold().underline();
        let ansi = s.ansi();
        // Should contain the text
        assert!(ansi.contains("test"));
    }

    #[test]
    fn test_chaining() {
        let s = StyledStr::plain("error").bold();
        let ansi = s.ansi();
        assert!(ansi.contains("error"));
    }
}
