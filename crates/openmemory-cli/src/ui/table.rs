//! Aligned key/value table.
//!
//! ```text
//!      data dir  ~/.openmemory/data/default
//!  schema vers.  7
//!      entities  142
//! ```
//!
//! Key column is right-aligned to a common width, then two spaces of
//! gutter, then the value (left-aligned, muted). Section headings are
//! accent-colored and sit flush-left in the parent indent.

use std::io::Write;

use super::style::{ACCENT_DIM, FG, MUTED};
use super::{paint, writeln_swallow};

/// Left margin for every row, heading, and blank line. Fixed because
/// every caller wants the same indent and a configurable margin would
/// be a knob without a user.
const INDENT: &str = "  ";

#[derive(Debug, Clone)]
struct Row {
    key: String,
    value: String,
}

#[derive(Debug, Clone)]
enum Element {
    Heading(String),
    Row(Row),
    Blank,
}

#[derive(Debug, Clone, Default)]
pub struct KvTable {
    elements: Vec<Element>,
}

impl KvTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a row.
    pub fn row(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.elements.push(Element::Row(Row {
            key: key.into(),
            value: value.into(),
        }));
        self
    }

    /// Append a sub-section heading.
    pub fn heading(mut self, label: impl Into<String>) -> Self {
        self.elements.push(Element::Heading(label.into()));
        self
    }

    /// Append a blank padding line.
    pub fn blank(mut self) -> Self {
        self.elements.push(Element::Blank);
        self
    }

    /// Render. The key column width is the max key width across the
    /// whole table so headings don't disturb alignment.
    pub fn render<W: Write>(&self, w: &mut W) {
        let key_width = self
            .elements
            .iter()
            .filter_map(|e| match e {
                Element::Row(r) => Some(r.key.chars().count()),
                _ => None,
            })
            .max()
            .unwrap_or(0);

        for el in &self.elements {
            match el {
                Element::Blank => writeln_swallow(w, ""),
                Element::Heading(label) => {
                    writeln_swallow(w, &format!("{INDENT}{}", paint(ACCENT_DIM, label)));
                }
                Element::Row(r) => {
                    let pad = key_width.saturating_sub(r.key.chars().count());
                    let line = format!(
                        "{INDENT}{}{}  {}",
                        " ".repeat(pad),
                        paint(FG, &r.key),
                        paint(MUTED, &r.value),
                    );
                    writeln_swallow(w, &line);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(t: &KvTable) -> String {
        let mut buf = Vec::new();
        t.render(&mut buf);
        anstream::adapter::strip_str(&String::from_utf8(buf).unwrap()).to_string()
    }

    #[test]
    fn aligns_key_column_to_widest_key() {
        let t = KvTable::new()
            .row("a", "1")
            .row("longer", "2")
            .row("mid", "3");
        let out = render(&t);
        let lines: Vec<&str> = out.lines().collect();
        // Each row has the value starting at the same column.
        let positions: Vec<usize> = lines
            .iter()
            .map(|l| l.find(char::is_numeric).unwrap())
            .collect();
        assert!(positions.windows(2).all(|w| w[0] == w[1]));
    }

    #[test]
    fn heading_does_not_offset_alignment() {
        let t = KvTable::new()
            .row("alpha", "1")
            .heading("section")
            .row("beta", "2");
        let out = render(&t);
        assert!(out.contains("section"));
        assert!(out.contains("alpha"));
        assert!(out.contains("beta"));
    }

    #[test]
    fn blank_emits_empty_line() {
        let t = KvTable::new().row("a", "1").blank().row("b", "2");
        let out = render(&t);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[1].is_empty());
    }
}
