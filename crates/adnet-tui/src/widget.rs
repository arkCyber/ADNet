//! Reusable UI widgets for common patterns.
//!
//! Provides higher-level composable widgets built on top of
//! box drawing and progress bars.

use crate::color::{Color, StyledStr};
use crate::box_drawing::{Box, BorderStyle};

/// A table widget for displaying tabular data.
#[derive(Debug, Clone)]
pub struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    column_widths: Vec<usize>,
    border_style: BorderStyle,
    zebra_stripe: bool,
}

impl Default for Table {
    fn default() -> Self {
        Self::new()
    }
}

impl Table {
    /// Create a new table.
    pub fn new() -> Self {
        Self {
            headers: Vec::new(),
            rows: Vec::new(),
            column_widths: Vec::new(),
            border_style: BorderStyle::Single,
            zebra_stripe: true,
        }
    }

    /// Create a table with headers.
    pub fn with_headers<I, S>(headers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let headers: Vec<String> = headers.into_iter().map(|s| s.into()).collect();
        let column_widths = headers.iter().map(|h| h.len()).collect();
        Self {
            headers,
            column_widths,
            ..Default::default()
        }
    }

    /// Set border style.
    pub fn border_style(mut self, style: BorderStyle) -> Self {
        self.border_style = style;
        self
    }

    /// Enable or disable zebra striping.
    pub fn zebra_stripe(mut self, enabled: bool) -> Self {
        self.zebra_stripe = enabled;
        self
    }

    /// Add a row.
    pub fn add_row<I, S>(&mut self, row: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let row: Vec<String> = row.into_iter().map(|s| s.into()).collect();
        // Update column widths
        for (i, cell) in row.iter().enumerate() {
            let len = cell.len();
            if i >= self.column_widths.len() {
                self.column_widths.push(len);
            } else if len > self.column_widths[i] {
                self.column_widths[i] = len;
            }
        }
        self.rows.push(row);
    }

    /// Render the table.
    pub fn render(&self) -> String {
        if self.headers.is_empty() && self.rows.is_empty() {
            return String::new();
        }

        let c = self.border_style.chars();
        let total_width: usize = self.column_widths.iter().sum::<usize>() + 3 * self.column_widths.len();
        let sep = format!(" {} ", c.vertical());

        let mut lines = Vec::new();

        // Top border
        lines.push(format!(
            "{}{}{}",
            c.top_left(),
            c.horizontal().repeat(total_width),
            c.top_right()
        ));

        // Header row
        if !self.headers.is_empty() {
            let header_cells: Vec<String> = self.headers
                .iter()
                .zip(self.column_widths.iter())
                .map(|(h, &w)| {
                    let padded = format!("{:<w$}", h);
                    Color::Cyan.paint(&padded).ansi()
                })
                .collect();
            lines.push(format!(
                "{}{}{}{}",
                c.vertical(),
                header_cells.join(&sep),
                c.vertical(),
                ""
            ));

            // Header separator
            let sep_line: String = self.column_widths.iter()
                .map(|&w| c.horizontal().repeat(w))
                .collect::<Vec<_>>()
                .join(&format!(" {} ", c.cross()));
            lines.push(format!(
                "{}{}{}{}{}",
                c.top_tee(),
                c.horizontal().repeat(1),
                sep_line,
                c.horizontal().repeat(1),
                c.bottom_tee()
            ));
        }

        // Data rows
        for (row_idx, row) in self.rows.iter().enumerate() {
            let cells: Vec<String> = row
                .iter()
                .zip(self.column_widths.iter())
                .map(|(cell, &w)| {
                    let padded = format!("{:<w$}", cell);
                    if self.zebra_stripe && row_idx % 2 == 1 {
                        // Dim color for zebra stripes
                        StyledStr::plain(&padded).dim().ansi()
                    } else {
                        padded
                    }
                })
                .collect();
            lines.push(format!(
                "{}{}{}{}",
                c.vertical(),
                cells.join(&sep),
                c.vertical(),
                ""
            ));
        }

        // Bottom border
        lines.push(format!(
            "{}{}{}",
            c.bottom_left(),
            c.horizontal().repeat(total_width),
            c.bottom_right()
        ));

        lines.join("\n")
    }
}

impl std::fmt::Display for Table {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.render())
    }
}

/// A widget that shows node status with colored indicators.
pub fn status_widget(status: &str) -> StyledStr {
    match status.to_lowercase().as_str() {
        "online" | "ok" | "running" => Color::Green.paint(status),
        "warning" | "warn" => Color::Yellow.paint(status),
        "error" | "critical" | "failed" => Color::Red.paint(status),
        "offline" | "stopped" | "disabled" => StyledStr::plain(status).dim(),
        _ => StyledStr::plain(status),
    }
}

/// A widget that shows an alert with appropriate icon and color.
pub fn alert_widget(level: &str, message: &str) -> String {
    let (color, icon) = match level.to_lowercase().as_str() {
        "critical" => (Color::Red, "⛔"),
        "error" => (Color::Red, "❌"),
        "warn" | "warning" => (Color::Yellow, "⚠"),
        "info" => (Color::Cyan, "ℹ"),
        _ => (Color::White, "•"),
    };

    format!(
        "{} {} {}",
        color.paint(icon).bold(),
        color.paint(level.to_uppercase()),
        message
    )
}

/// A section header widget.
pub fn section_header(title: &str) -> String {
    format!(
        "{}\n{}",
        Color::Cyan.paint(title).bold(),
        Color::Cyan.paint("─".repeat(40))
    )
}

/// A help text widget.
pub fn help_text(commands: &[(&str, &str)]) -> String {
    let mut table = Table::with_headers(["Command", "Description"]);
    for (cmd, desc) in commands {
        table.add_row([*cmd, *desc]);
    }
    table.render()
}

/// Create a summary widget showing key metrics.
pub fn metrics_summary(items: &[(&str, &str, &str)]) -> String {
    let mut panel = Box::with_title("Metrics Summary");
    for (name, value, unit) in items {
        let text = format!("{} {}", value, unit);
        panel = panel.add_field(name, text);
    }
    panel.render()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_table_empty() {
        let t = Table::new();
        assert!(t.render().is_empty());
    }

    #[test]
    fn test_table_with_data() {
        let mut t = Table::with_headers(["Name", "Value"]);
        t.add_row(["CPU", "45%"]);
        t.add_row(["Memory", "2.1 GiB"]);
        let rendered = t.render();
        assert!(rendered.contains("Name"));
        assert!(rendered.contains("CPU"));
    }

    #[test]
    fn test_status_widget() {
        let s = status_widget("Online");
        assert!(s.ansi().contains("Online"));

        let s = status_widget("Error");
        assert!(s.ansi().contains("Error"));
    }

    #[test]
    fn test_alert_widget() {
        let w = alert_widget("warning", "Storage nearly full");
        assert!(w.contains("⚠"));
        assert!(w.contains("Storage nearly full"));
    }
}
