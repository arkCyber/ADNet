//! Terminal control utilities for cursor and screen manipulation.
//!
//! Provides ANSI escape sequences for:
//! - Cursor positioning and movement
//! - Screen clearing
//! - Scroll regions
//! - Alternative screen buffer

/// Move cursor to (row, col) position (1-indexed).
pub fn goto(row: u16, col: u16) -> String {
    format!("\x1b[{};{}H", row, col)
}

/// Move cursor up N rows.
pub fn move_up(n: u16) -> String {
    format!("\x1b[{}A", n)
}

/// Move cursor down N rows.
pub fn move_down(n: u16) -> String {
    format!("\x1b[{}B", n)
}

/// Move cursor right N columns.
pub fn move_right(n: u16) -> String {
    format!("\x1b[{}C", n)
}

/// Move cursor left N columns.
pub fn move_left(n: u16) -> String {
    format!("\x1b[{}D", n)
}

/// Save cursor position.
pub fn save_cursor() -> String {
    "\x1b[s".to_string()
}

/// Restore cursor position.
pub fn restore_cursor() -> String {
    "\x1b[u".to_string()
}

/// Hide cursor.
pub fn hide_cursor() -> String {
    "\x1b[?25l".to_string()
}

/// Show cursor.
pub fn show_cursor() -> String {
    "\x1b[?25h".to_string()
}

/// Clear entire screen.
pub fn clear_screen() -> String {
    "\x1b[2J".to_string()
}

/// Clear screen and move cursor to home.
pub fn clear_all() -> String {
    "\x1b[2J\x1b[H".to_string()
}

/// Clear from cursor to end of line.
pub fn clear_line_end() -> String {
    "\x1b[0K".to_string()
}

/// Clear from cursor to beginning of line.
pub fn clear_line_start() -> String {
    "\x1b[1K".to_string()
}

/// Clear entire current line.
pub fn clear_line() -> String {
    "\x1b[2K".to_string()
}

/// Clear from cursor to bottom of screen.
pub fn clear_down() -> String {
    "\x1b[J".to_string()
}

/// Clear from cursor to top of screen.
pub fn clear_up() -> String {
    "\x1b[1J".to_string()
}

/// Enable alternative screen buffer.
pub fn enter_alternate_screen() -> String {
    "\x1b[?1049h".to_string()
}

/// Disable alternative screen buffer (return to normal).
pub fn exit_alternate_screen() -> String {
    "\x1b[?1049l".to_string()
}

/// Reset terminal to default state.
pub fn reset_terminal() -> String {
    "\x1b[c".to_string()
}

/// Set scroll region (top_row, bottom_row). 1-indexed.
pub fn set_scroll_region(top: u16, bottom: u16) -> String {
    format!("\x1b[{};{}r", top, bottom)
}

/// Reset scroll region to full screen.
pub fn reset_scroll_region() -> String {
    "\x1b[r".to_string()
}

/// Scroll screen up N lines.
pub fn scroll_up(n: u16) -> String {
    format!("\x1b[{}S", n)
}

/// Scroll screen down N lines.
pub fn scroll_down(n: u16) -> String {
    format!("\x1b[{}T", n)
}

/// Get cursor position (query). Returns escape sequence to send.
/// Use with terminal that supports reporting (DCS/CSI u).
pub fn query_cursor_position() -> String {
    "\x1b[6n".to_string()
}

/// Enable bracketed paste mode.
pub fn enable_bracketed_paste() -> String {
    "\x1b[?2004h".to_string()
}

/// Disable bracketed paste mode.
pub fn disable_bracketed_paste() -> String {
    "\x1b[?2004l".to_string()
}

/// Request terminal resize notification.
pub fn request_terminal_size() -> String {
    "\x1b[18t".to_string()
}

/// A guard that manages cursor visibility.
/// Automatically shows cursor when dropped.
pub struct CursorGuard;

impl CursorGuard {
    /// Create a new guard that hides the cursor.
    /// Cursor will be shown when dropped.
    pub fn new() -> Self {
        print!("{}", hide_cursor());
        Self
    }

    /// Hide cursor immediately without guard.
    pub fn hide() {
        print!("{}", hide_cursor());
    }

    /// Show cursor immediately without guard.
    pub fn show() {
        print!("{}", show_cursor());
    }
}

impl Default for CursorGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CursorGuard {
    fn drop(&mut self) {
        print!("{}", show_cursor());
    }
}

/// A guard for alternative screen buffer.
/// Automatically exits when dropped.
pub struct AlternateScreenGuard;

impl AlternateScreenGuard {
    /// Enter alternate screen buffer.
    /// Automatically exits when dropped.
    pub fn new() -> Self {
        print!("{}", enter_alternate_screen());
        Self
    }
}

impl Default for AlternateScreenGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AlternateScreenGuard {
    fn drop(&mut self) {
        print!("{}", exit_alternate_screen());
    }
}

/// Print terminal control string.
pub fn print_ctrl(s: &str) {
    print!("{}", s);
}

/// Println terminal control string with newline.
pub fn println_ctrl(s: &str) {
    println!("{}", s);
}

/// Overwrite current line with new content.
/// Uses carriage return to go to line start, clears line, then prints.
pub fn overwrite_line(s: &str) {
    print!("\r{}\x1b[K", s);
}

/// Overwrite current position with new content (no clear).
pub fn overwrite(s: &str) {
    print!("\r{}", s);
}

/// Print with automatic cursor hiding during output.
pub fn print_hidden<T: std::fmt::Display>(t: T) {
    print!("{}{}", hide_cursor(), t);
}

/// Terminal size query result.
#[derive(Debug, Clone, Copy)]
pub struct TerminalSize {
    pub rows: u16,
    pub cols: u16,
}

impl TerminalSize {
    /// Get current terminal size from environment or default.
    pub fn from_env() -> Self {
        // Try to get from environment
        if let (Some(cols), Some(rows)) = (
            std::env::var("COLUMNS").ok().and_then(|s| s.parse().ok()),
            std::env::var("LINES").ok().and_then(|s| s.parse().ok()),
        ) {
            return Self { rows, cols };
        }

        // Default fallback
        Self { rows: 24, cols: 80 }
    }

    /// Default terminal size.
    pub fn default() -> Self {
        Self { rows: 24, cols: 80 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_goto() {
        assert_eq!(goto(1, 1), "\x1b[1;1H");
        assert_eq!(goto(10, 20), "\x1b[10;20H");
    }

    #[test]
    fn test_move_directions() {
        assert_eq!(move_up(5), "\x1b[5A");
        assert_eq!(move_down(3), "\x1b[3B");
        assert_eq!(move_right(2), "\x1b[2C");
        assert_eq!(move_left(4), "\x1b[4D");
    }

    #[test]
    fn test_save_restore_cursor() {
        assert_eq!(save_cursor(), "\x1b[s");
        assert_eq!(restore_cursor(), "\x1b[u");
    }

    #[test]
    fn test_cursor_visibility() {
        assert_eq!(hide_cursor(), "\x1b[?25l");
        assert_eq!(show_cursor(), "\x1b[?25h");
    }

    #[test]
    fn test_clear_operations() {
        assert_eq!(clear_screen(), "\x1b[2J");
        assert_eq!(clear_all(), "\x1b[2J\x1b[H");
        assert_eq!(clear_line(), "\x1b[2K");
        assert_eq!(clear_line_end(), "\x1b[0K");
        assert_eq!(clear_line_start(), "\x1b[1K");
    }

    #[test]
    fn test_alternate_screen() {
        assert_eq!(enter_alternate_screen(), "\x1b[?1049h");
        assert_eq!(exit_alternate_screen(), "\x1b[?1049l");
    }

    #[test]
    fn test_scroll_region() {
        assert_eq!(set_scroll_region(1, 20), "\x1b[1;20r");
        assert_eq!(reset_scroll_region(), "\x1b[r");
    }

    #[test]
    fn test_scroll() {
        assert_eq!(scroll_up(2), "\x1b[2S");
        assert_eq!(scroll_down(3), "\x1b[3T");
    }

    #[test]
    fn test_bracketed_paste() {
        assert_eq!(enable_bracketed_paste(), "\x1b[?2004h");
        assert_eq!(disable_bracketed_paste(), "\x1b[?2004l");
    }

    #[test]
    fn test_terminal_size() {
        let size = TerminalSize::from_env();
        assert!(size.rows > 0);
        assert!(size.cols > 0);
    }

    #[test]
    fn test_terminal_size_default() {
        let size = TerminalSize::default();
        assert_eq!(size.rows, 24);
        assert_eq!(size.cols, 80);
    }
}
