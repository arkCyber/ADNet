//! Animation utilities for terminal UI.
//!
//! Provides animated text effects like typing animation,
//! blinking, and fade effects.

use std::time::Duration;

/// Type out text character by character.
/// Returns an iterator that yields each frame.
pub struct Typewriter<'a> {
    text: &'a str,
    chars_per_tick: usize,
    current: usize,
    done: bool,
}

impl<'a> Typewriter<'a> {
    /// Create a new typewriter effect.
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            chars_per_tick: 1,
            current: 0,
            done: false,
        }
    }

    /// Set characters per tick (speed).
    pub fn speed(mut self, chars: usize) -> Self {
        self.chars_per_tick = chars.max(1);
        self
    }

    /// Get current visible text.
    pub fn visible(&self) -> &str {
        &self.text[..self.current.min(self.text.len())]
    }

    /// Check if animation is complete.
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Advance one tick.
    pub fn tick(&mut self) {
        if self.done {
            return;
        }
        self.current = (self.current + self.chars_per_tick).min(self.text.len());
        if self.current >= self.text.len() {
            self.done = true;
        }
    }

    /// Reset animation to beginning.
    pub fn reset(&mut self) {
        self.current = 0;
        self.done = false;
    }

    /// Restart with new text.
    pub fn restart(&mut self, text: &'a str) {
        self.text = text;
        self.reset();
    }
}

impl<'a> Iterator for Typewriter<'a> {
    type Item = String;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        self.tick();
        Some(self.visible().to_string())
    }
}

impl<'a> std::fmt::Display for Typewriter<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.visible())
    }
}

/// Blinking text effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlinkState {
    Visible,
    Hidden,
}

/// A blinking text generator.
pub struct BlinkText<'a> {
    text: &'a str,
    blink_rate_ms: u64,
    state: BlinkState,
}

impl<'a> BlinkText<'a> {
    /// Create a new blinking text effect.
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            blink_rate_ms: 500,
            state: BlinkState::Visible,
        }
    }

    /// Set blink rate in milliseconds.
    pub fn rate(mut self, ms: u64) -> Self {
        self.blink_rate_ms = ms;
        self
    }

    /// Get blink rate.
    pub fn blink_rate(&self) -> Duration {
        Duration::from_millis(self.blink_rate_ms)
    }

    /// Get current state.
    pub fn state(&self) -> BlinkState {
        self.state
    }

    /// Toggle blink state.
    pub fn toggle(&mut self) {
        self.state = match self.state {
            BlinkState::Visible => BlinkState::Hidden,
            BlinkState::Hidden => BlinkState::Visible,
        };
    }

    /// Show text.
    pub fn show(&mut self) {
        self.state = BlinkState::Visible;
    }

    /// Hide text.
    pub fn hide(&mut self) {
        self.state = BlinkState::Hidden;
    }

    /// Get current display text.
    pub fn display(&self) -> &str {
        match self.state {
            BlinkState::Visible => self.text,
            BlinkState::Hidden => "",
        }
    }
}

impl<'a> std::fmt::Display for BlinkText<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display())
    }
}

/// Progress text that cycles through a spinner or dots.
pub struct SpinnerText<'a> {
    frames: Vec<&'a str>,
    message: &'a str,
    index: usize,
}

impl<'a> SpinnerText<'a> {
    /// Create with default frames.
    pub fn new(message: &'a str) -> Self {
        Self::with_frames(message, vec!["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
    }

    /// Create with custom frames.
    pub fn with_frames(message: &'a str, frames: Vec<&'a str>) -> Self {
        Self {
            frames,
            message,
            index: 0,
        }
    }

    /// Advance to next frame.
    pub fn tick(&mut self) -> String {
        let frame = self.frames[self.index];
        self.index = (self.index + 1) % self.frames.len();
        format!("{} {}", frame, self.message)
    }

    /// Reset to first frame.
    pub fn reset(&mut self) {
        self.index = 0;
    }

    /// Change message.
    pub fn set_message(&mut self, message: &'a str) {
        self.message = message;
    }

    /// Get total frame count.
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }
}

impl<'a> std::fmt::Display for SpinnerText<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.frames[self.index], self.message)
    }
}

/// Marquee (scrolling) text effect.
pub struct Marquee<'a> {
    text: String,
    width: usize,
    position: usize,
    speed: usize,
    padding: &'a str,
}

impl<'a> Marquee<'a> {
    /// Create a new marquee effect.
    pub fn new(text: &'a str, width: usize) -> Self {
        Self {
            text: text.to_string(),
            width,
            position: 0,
            speed: 1,
            padding: "   ",
        }
    }

    /// Set scroll speed.
    pub fn speed(mut self, chars_per_tick: usize) -> Self {
        self.speed = chars_per_tick.max(1);
        self
    }

    /// Set padding between repeats.
    pub fn padding(mut self, padding: &'a str) -> Self {
        self.padding = padding;
        self
    }

    /// Get current visible portion.
    pub fn visible(&self) -> String {
        let full = format!("{}{}{}", self.text, self.padding, self.text);
        let len = full.len();
        
        if len <= self.width {
            return full;
        }

        let start = self.position % len;
        let mut result = String::new();
        
        for i in 0..self.width {
            let idx = (start + i) % len;
            result.push(full.chars().nth(idx).unwrap_or(' '));
        }
        
        result
    }

    /// Advance one tick.
    pub fn tick(&mut self) {
        self.position = (self.position + self.speed) % (self.text.len() + self.padding.len());
    }

    /// Reset to beginning.
    pub fn reset(&mut self) {
        self.position = 0;
    }

    /// Check if animation can be seen (text longer than width).
    pub fn is_active(&self) -> bool {
        self.text.len() > self.width
    }
}

impl<'a> std::fmt::Display for Marquee<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:width$}", self.visible(), width = self.width)
    }
}

/// Pulse (fade in/out) effect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PulseState {
    FadeIn,
    FadeOut,
}

/// Generate a pulse effect with varying intensity.
pub struct Pulse {
    steps: usize,
    current_step: usize,
    state: PulseState,
}

impl Pulse {
    /// Create a new pulse effect with given number of steps.
    pub fn new(steps: usize) -> Self {
        Self {
            steps: steps.max(1),
            current_step: 0,
            state: PulseState::FadeIn,
        }
    }

    /// Get current intensity (0.0 to 1.0).
    pub fn intensity(&self) -> f32 {
        let total = self.steps * 2 - 1;
        let pos = match self.state {
            PulseState::FadeIn => self.current_step,
            PulseState::FadeOut => self.steps - 1 + self.current_step,
        };
        pos as f32 / total as f32
    }

    /// Get ANSI color code for current intensity.
    pub fn color_code(&self) -> String {
        let intensity = self.intensity();
        // Map to a color range (e.g., dim to bright green)
        let code = (30.0 + (intensity * 2.0 * 3.0)) as u8;
        format!("\x1b[{}m", code.min(32))
    }

    /// Advance one step.
    pub fn tick(&mut self) {
        self.current_step += 1;
        if self.current_step >= self.steps {
            self.current_step = 0;
            self.state = match self.state {
                PulseState::FadeIn => PulseState::FadeOut,
                PulseState::FadeOut => PulseState::FadeIn,
            };
        }
    }

    /// Reset to beginning.
    pub fn reset(&mut self) {
        self.current_step = 0;
        self.state = PulseState::FadeIn;
    }

    /// Get current state.
    pub fn state(&self) -> PulseState {
        self.state
    }
}

/// Generate countdown animation.
pub struct Countdown {
    from: u32,
    current: u32,
    tick_duration_ms: u64,
}

impl Countdown {
    /// Create a countdown from a number.
    pub fn new(from: u32) -> Self {
        Self {
            from,
            current: from,
            tick_duration_ms: 1000,
        }
    }

    /// Set tick duration in milliseconds.
    pub fn tick_duration_ms(mut self, ms: u64) -> Self {
        self.tick_duration_ms = ms;
        self
    }

    /// Get tick duration.
    pub fn tick_duration(&self) -> Duration {
        Duration::from_millis(self.tick_duration_ms)
    }

    /// Get current number.
    pub fn current(&self) -> u32 {
        self.current
    }

    /// Advance countdown.
    pub fn tick(&mut self) -> Option<u32> {
        if self.current == 0 {
            return None;
        }
        self.current -= 1;
        Some(self.current)
    }

    /// Check if complete.
    pub fn is_done(&self) -> bool {
        self.current == 0
    }

    /// Reset countdown.
    pub fn reset(&mut self) {
        self.current = self.from;
    }
}

impl std::fmt::Display for Countdown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_typewriter() {
        let mut tw = Typewriter::new("Hello");
        assert_eq!(tw.visible(), "");
        tw.tick();
        assert!(tw.visible().starts_with("H"));
        tw.tick();
        tw.tick();
        tw.tick();
        tw.tick();
        assert!(tw.is_done());
    }

    #[test]
    fn test_typewriter_speed() {
        let mut tw = Typewriter::new("Hello").speed(2);
        tw.tick();
        assert_eq!(tw.visible(), "He");
    }

    #[test]
    fn test_typewriter_display() {
        let mut tw = Typewriter::new("Hi");
        tw.tick();
        assert_eq!(format!("{}", tw), "H");
    }

    #[test]
    fn test_blink() {
        let mut blink = BlinkText::new("test");
        assert_eq!(blink.state(), BlinkState::Visible);
        assert_eq!(blink.display(), "test");
        blink.toggle();
        assert_eq!(blink.state(), BlinkState::Hidden);
        assert_eq!(blink.display(), "");
    }

    #[test]
    fn test_marquee() {
        let mut m = Marquee::new("Hello World", 5);
        let first = m.visible();
        assert_eq!(first.len(), 5);
        m.tick();
        let second = m.visible();
        // Should be different after tick
        assert!(first != second || !m.is_active());
    }

    #[test]
    fn test_marquee_short_text() {
        let m = Marquee::new("Hi", 10);
        assert!(!m.is_active());
        // When text is shorter than width, it should show text + padding
        let visible = m.visible();
        assert!(visible.starts_with("Hi"));
    }

    #[test]
    fn test_pulse() {
        let mut pulse = Pulse::new(3);
        assert_eq!(pulse.state(), PulseState::FadeIn);
        assert_eq!(pulse.intensity(), 0.0);
        pulse.tick();
        assert!((pulse.intensity() - 1.0/5.0).abs() < 0.01);
        pulse.tick();
        pulse.tick();
        pulse.tick();
        assert_eq!(pulse.state(), PulseState::FadeOut);
    }

    #[test]
    fn test_countdown() {
        let mut c = Countdown::new(3);
        assert_eq!(c.current(), 3);
        assert_eq!(c.tick(), Some(2));
        assert_eq!(c.tick(), Some(1));
        assert_eq!(c.tick(), Some(0));
        assert_eq!(c.tick(), None);
        assert!(c.is_done());
    }

    #[test]
    fn test_spinner_text() {
        let mut s = SpinnerText::new("Loading");
        let first = s.tick();
        assert!(first.contains("Loading"));
        let second = s.tick();
        assert!(second.contains("Loading"));
        // Different frames
        assert_ne!(first.split_whitespace().next(), second.split_whitespace().next());
    }
}
