//! `a3net-tui` — Terminal UI primitives for A3Net CLI.
//!
//! Provides:
//!
//! - **ASCII Boxes**: Draw bordered tables and panels
//! - **Progress Bars**: Visual storage/network progress indicators
//! - **Colors**: ANSI color support with automatic detection
//! - **i18n**: Internationalization support (English/Chinese)
//!
//! ## Design
//!
//! This crate deliberately avoids heavy dependencies (no `crossterm`,
//! no `ratatui`, no `tui`). It's a pure-Rust printer that outputs
//! ANSI-escaped strings, making it suitable for any terminal output
//! including CI logs and redirected files.
//!
//! ## Example
//!
//! ```rust
//! use a3net_tui::{Box, ProgressBar, Color};
//!
//! let panel = Box::with_title("Node Status")
//!     .add_field("Node ID", "12D3KooW...")
//!     .add_field("Status", Color::Green.paint("Online"));
//!
//! println!("{}", panel);
//! ```

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod box_drawing;
pub mod color;
pub mod i18n;
pub mod progress;
pub mod widget;

pub use box_drawing::Box;
pub use color::{Color, Style};
pub use i18n::{t, Locale, I18n};
pub use progress::ProgressBar;
