//! Interactive input handling for TUI applications.
//!
//! Provides key parsing and input helpers for building interactive
//! terminal applications.

use std::io::{self, Read};

/// Special key codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Enter key
    Enter,
    /// Escape key
    Escape,
    /// Tab key
    Tab,
    /// Backspace key
    Backspace,
    /// Delete key
    Delete,
    /// Arrow keys
    Up,
    Down,
    Left,
    Right,
    /// Home key
    Home,
    /// End key
    End,
    /// Page Up key
    PageUp,
    /// Page Down key
    PageDown,
    /// Insert key
    Insert,
    /// Function keys F1-F12
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    /// Character key
    Char(char),
    /// Ctrl+key combinations
    Ctrl(char),
    /// Unknown/unsupported key
    Unknown,
}

impl Key {
    /// Check if this is a control key (Ctrl+...).
    pub fn is_ctrl(&self) -> bool {
        matches!(self, Key::Ctrl(_))
    }

    /// Get the character if this is a Char or Ctrl variant.
    pub fn as_char(&self) -> Option<char> {
        match self {
            Key::Char(c) => Some(*c),
            Key::Ctrl(c) => Some(*c),
            _ => None,
        }
    }

    /// Check if this is an arrow key.
    pub fn is_arrow(&self) -> bool {
        matches!(self, Key::Up | Key::Down | Key::Left | Key::Right)
    }
}

impl std::fmt::Display for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Key::Enter => write!(f, "Enter"),
            Key::Escape => write!(f, "Escape"),
            Key::Tab => write!(f, "Tab"),
            Key::Backspace => write!(f, "Backspace"),
            Key::Delete => write!(f, "Delete"),
            Key::Up => write!(f, "Up"),
            Key::Down => write!(f, "Down"),
            Key::Left => write!(f, "Left"),
            Key::Right => write!(f, "Right"),
            Key::Home => write!(f, "Home"),
            Key::End => write!(f, "End"),
            Key::PageUp => write!(f, "PageUp"),
            Key::PageDown => write!(f, "PageDown"),
            Key::Insert => write!(f, "Insert"),
            Key::F1 => write!(f, "F1"),
            Key::F2 => write!(f, "F2"),
            Key::F3 => write!(f, "F3"),
            Key::F4 => write!(f, "F4"),
            Key::F5 => write!(f, "F5"),
            Key::F6 => write!(f, "F6"),
            Key::F7 => write!(f, "F7"),
            Key::F8 => write!(f, "F8"),
            Key::F9 => write!(f, "F9"),
            Key::F10 => write!(f, "F10"),
            Key::F11 => write!(f, "F11"),
            Key::F12 => write!(f, "F12"),
            Key::Char(c) => write!(f, "'{}'", c),
            Key::Ctrl(c) => write!(f, "Ctrl+{}", c),
            Key::Unknown => write!(f, "Unknown"),
        }
    }
}

/// Read a single key press from stdin.
/// Blocks until a key is pressed.
pub fn read_key() -> io::Result<Key> {
    let mut buf = [0u8; 1];
    io::stdin().read_exact(&mut buf)?;

    let byte = buf[0];

    // Handle escape sequences
    if byte == 0x1b {
        // Could be escape key or start of escape sequence
        let mut seq: Vec<u8> = vec![byte];
        
        // Try to read additional bytes
        for _ in 0..3 {
            let mut short_buf = [0u8; 1];
            match io::stdin().read(&mut short_buf) {
                Ok(0) | Ok(2..) => break, // EOF or unexpected
                Ok(1) => {
                    seq.push(short_buf[0]);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }

        return Ok(parse_escape_sequence(&seq));
    }

    let c = byte as char;

    // Handle regular characters
    if c == '\r' || c == '\n' {
        return Ok(Key::Enter);
    }
    if c == '\t' {
        return Ok(Key::Tab);
    }
    if c == '\x7f' || c == '\x08' {
        return Ok(Key::Backspace);
    }

    // Handle control characters
    if c.is_control() && c != ' ' {
        return Ok(Key::Unknown);
    }

    // Handle Ctrl combinations (Ctrl+A = 1, Ctrl+B = 2, etc.)
    if byte < 32 {
        return Ok(Key::Ctrl((byte + 64) as char));
    }

    Ok(Key::Char(c))
}

/// Parse an escape sequence into a Key.
fn parse_escape_sequence(seq: &[u8]) -> Key {
    if seq.len() == 1 {
        return Key::Escape;
    }

    // Common CSI sequences start with [ or O
    let bytes = &seq[1..];
    
    if bytes.is_empty() {
        return Key::Escape;
    }

    match bytes[0] {
        b'[' => parse_csi(&bytes[1..]),
        b'O' => parse_sco(&bytes[1..]),
        _ => Key::Escape,
    }
}

/// Parse CSI (Control Sequence Introducer) sequences.
fn parse_csi(bytes: &[u8]) -> Key {
    // CSI sequences end with a letter and may have parameter bytes (0x30-0x3F)
    // and intermediate bytes (0x20-0x2F) in between

    // Find the final byte
    if bytes.is_empty() {
        return Key::Escape;
    }

    let final_byte = bytes[bytes.len() - 1];
    let params = &bytes[..bytes.len() - 1];

    match final_byte {
        b'A' => Key::Up,      // CSI A = Up
        b'B' => Key::Down,     // CSI B = Down  
        b'C' => Key::Right,    // CSI C = Right
        b'D' => Key::Left,     // CSI D = Left
        b'H' => Key::Home,     // CSI H = Home
        b'F' => Key::End,      // CSI F = End
        b'~' => parse_sixel(params), // SS3 style
        b'P' => Key::F1,       // DCS or other
        b'Q' => Key::F2,
        b'R' => Key::F3,
        b'S' => Key::F4,
        _ => {
            // Check for common sequences
            match (final_byte as char, params) {
                ('A', _) => Key::Up,
                ('B', _) => Key::Down,
                ('C', _) => Key::Right,
                ('D', _) => Key::Left,
                ('H', _) => Key::Home,
                ('F', _) => Key::End,
                ('1', p) if p.ends_with(&[b'~']) => Key::Home,
                ('4', p) if p.ends_with(&[b'~']) => Key::End,
                ('5', p) if p.ends_with(&[b'~']) => Key::PageUp,
                ('6', p) if p.ends_with(&[b'~']) => Key::PageDown,
                ('2', p) if p.ends_with(&[b'~']) => Key::Insert,
                ('3', p) if p.ends_with(&[b'~']) => Key::Delete,
                ('1',  _) => Key::Home,
                ('2',  _) => Key::Insert,
                ('3',  _) => Key::Delete,
                ('4',  _) => Key::End,
                ('5',  _) => Key::PageUp,
                ('6',  _) => Key::PageDown,
                ('7',  _) => Key::Home,
                ('8',  _) => Key::End,
                _ => Key::Unknown,
            }
        }
    }
}

/// Parse SS3 (Single Shift Select) sequences.
fn parse_sco(bytes: &[u8]) -> Key {
    if bytes.is_empty() {
        return Key::Unknown;
    }

    match bytes[0] {
        b'A' => Key::Up,
        b'B' => Key::Down,
        b'C' => Key::Right,
        b'D' => Key::Left,
        b'H' => Key::Home,
        b'F' => Key::End,
        b'P' => Key::F1,
        b'Q' => Key::F2,
        b'R' => Key::F3,
        b'S' => Key::F4,
        _ => Key::Unknown,
    }
}

/// Parse sixel-based sequences.
fn parse_sixel(params: &[u8]) -> Key {
    // Extract numeric parameter
    let num: u8 = params.iter()
        .filter(|&&b| b >= b'0' && b <= b'9')
        .fold(0u8, |acc, &b| acc * 10 + (b - b'0'));

    match num {
        1 => Key::Home,
        2 => Key::Insert,
        3 => Key::Delete,
        4 => Key::End,
        5 => Key::PageUp,
        6 => Key::PageDown,
        7 => Key::Home,
        8 => Key::End,
        11 => Key::F1,
        12 => Key::F2,
        13 => Key::F3,
        14 => Key::F4,
        15 => Key::F5,
        17 => Key::F6,
        18 => Key::F7,
        19 => Key::F8,
        20 => Key::F9,
        21 => Key::F10,
        23 => Key::F11,
        24 => Key::F12,
        _ => Key::Unknown,
    }
}

/// Read a line of input with optional echo.
pub fn read_line() -> io::Result<String> {
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    // Remove trailing newline
    if line.ends_with('\n') {
        line.pop();
    }
    if line.ends_with('\r') {
        line.pop();
    }
    Ok(line)
}

/// Read a line with password masking (shows asterisks).
pub fn read_password() -> io::Result<String> {
    let mut password = String::new();
    
    // Disable echo
    print!("\x1b[?25l"); // Hide cursor
    print!("\x1b[8m");  // Invisible text (if supported)
    io::Write::flush(&mut io::stdout())?;

    loop {
        let mut buf = [0u8; 1];
        io::stdin().read_exact(&mut buf)?;
        
        let c = buf[0] as char;
        
        if c == '\r' || c == '\n' {
            break;
        }
        
        if c == '\x7f' || c == '\x08' {
            // Backspace
            if !password.is_empty() {
                password.pop();
                print!("\x08 \x08"); // Move back, print space, move back again
                io::Write::flush(&mut io::stdout())?;
            }
            continue;
        }
        
        if c.is_control() {
            continue;
        }
        
        password.push(c);
        print!("*");
        io::Write::flush(&mut io::stdout())?;
    }
    
    // Re-enable echo
    print!("\x1b[?25h"); // Show cursor
    print!("\x1b[0m");   // Reset styling
    println!();
    
    Ok(password)
}

/// Read a yes/no confirmation from user.
pub fn read_yes_no() -> io::Result<bool> {
    loop {
        print!(" [y/N] ");
        io::Write::flush(&mut io::stdout())?;
        
        let mut buf = [0u8; 1];
        io::stdin().read_exact(&mut buf)?;
        let c = buf[0] as char;
        println!();
        
        match c.to_ascii_lowercase() {
            'y' => return Ok(true),
            'n' | '\r' | '\n' => return Ok(false),
            _ => continue,
        }
    }
}

/// Read a single character without requiring Enter.
pub fn read_char() -> io::Result<char> {
    let mut buf = [0u8; 1];
    io::stdin().read_exact(&mut buf)?;
    Ok(buf[0] as char)
}

/// Prompt user and read input with a default value.
pub fn prompt_with_default(prompt: &str, default: &str) -> io::Result<String> {
    print!("{} [{}]: ", prompt, default);
    io::Write::flush(&mut io::stdout())?;
    
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    line = line.trim().to_string();
    
    if line.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(line)
    }
}

/// Read an integer from user with validation.
pub fn read_int(prompt: &str, default: i64) -> io::Result<i64> {
    loop {
        let input = prompt_with_default(prompt, &default.to_string())?;
        
        match input.parse::<i64>() {
            Ok(n) => return Ok(n),
            Err(_) => {
                print!("Invalid number. ");
                continue;
            }
        }
    }
}

/// Check if stdin has input available (non-blocking).
pub fn has_input() -> bool {
    let mut buf = [0u8; 1];
    match std::io::stdin().read(&mut buf) {
        Ok(0) => false,
        Ok(_) => true,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_display() {
        assert_eq!(Key::Enter.to_string(), "Enter");
        assert_eq!(Key::Up.to_string(), "Up");
        assert_eq!(Key::Char('a').to_string(), "'a'");
        assert_eq!(Key::Ctrl('C').to_string(), "Ctrl+C");
    }

    #[test]
    fn test_key_is_arrow() {
        assert!(Key::Up.is_arrow());
        assert!(Key::Down.is_arrow());
        assert!(Key::Left.is_arrow());
        assert!(Key::Right.is_arrow());
        assert!(!Key::Enter.is_arrow());
    }

    #[test]
    fn test_key_as_char() {
        assert_eq!(Key::Char('x').as_char(), Some('x'));
        assert_eq!(Key::Ctrl('C').as_char(), Some('C'));
        assert_eq!(Key::Enter.as_char(), None);
    }

    #[test]
    fn test_key_is_ctrl() {
        assert!(Key::Ctrl('C').is_ctrl());
        assert!(!Key::Char('c').is_ctrl());
    }

    #[test]
    fn test_parse_escape_sequence_single() {
        assert_eq!(parse_escape_sequence(&[b'\x1b']), Key::Escape);
    }

    #[test]
    fn test_parse_csi_up() {
        // CSI A = Up
        assert_eq!(parse_csi(&[b'A']), Key::Up);
        assert_eq!(parse_csi(&[b'B']), Key::Down);
        assert_eq!(parse_csi(&[b'C']), Key::Right);
        assert_eq!(parse_csi(&[b'D']), Key::Left);
    }
}
