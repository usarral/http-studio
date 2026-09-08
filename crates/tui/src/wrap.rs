//! Laying logical lines out into the rows that fit in a pane.
//!
//! This module is **pure**: it knows neither ratatui nor the application
//! state. It takes text and a width, and says where to cut it.
//!
//! It exists because without it the interface lied: `Paragraph` clips what
//! does not fit, while the terminal cursor, placed by hand, kept advancing.
//! The result was a cursor wandering over the neighbouring pane while text was
//! being typed on an invisible line.
//!
//! # Invariant
//!
//! A line's spans are **contiguous and exhaustive**: one's `end` is the next
//! one's `start`, and the last reaches the end of the text. That is what makes
//! locating the cursor free of special cases, because every position in the
//! buffer belongs to exactly one row.

use unicode_width::UnicodeWidthChar;

/// The span of a logical line that occupies one row on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualRow {
    /// Index of the logical line it comes from.
    pub line: usize,
    /// Byte where the span starts within the line.
    pub start: usize,
    /// Byte where the span ends, exclusive.
    pub end: usize,
}

/// Lays every line out into rows of at most `width` columns.
///
/// It is generic over the text so that whoever already holds the lines by
/// reference need not clone them: the layout is redone on every frame.
#[must_use]
pub fn rows<S: AsRef<str>>(lines: &[S], width: usize) -> Vec<VisualRow> {
    let mut out = Vec::with_capacity(lines.len());
    for (index, text) in lines.iter().enumerate() {
        wrap_line(index, text.as_ref(), width, &mut out);
    }
    out
}

/// Lays a single line out, appending its spans to `out`.
///
/// The cut goes at the last space that fits, and only when there is none is a
/// word split in half: a long token (a URL, a JWT) is precisely what nobody
/// wants to see chopped up at the whim of the width.
fn wrap_line(index: usize, text: &str, width: usize, out: &mut Vec<VisualRow>) {
    // A width of zero would produce empty rows forever.
    let width = width.max(1);
    let mut start = 0;

    loop {
        let mut used = 0;
        let mut opportunity = None;
        let mut end = text.len();

        for (offset, character) in text[start..].char_indices() {
            let at = start + offset;
            let advance = character.width().unwrap_or(0);

            // `used > 0` avoids an empty row when a single character (an
            // ideograph in a narrow pane) no longer fits on its own.
            if used + advance > width && used > 0 {
                end = opportunity.filter(|cut| *cut > start).unwrap_or(at);
                break;
            }

            used += advance;
            if character.is_whitespace() {
                // The cut goes *after* the space so it is not lost: the
                // spans have to cover the whole line.
                opportunity = Some(at + character.len_utf8());
            }
        }

        out.push(VisualRow {
            line: index,
            start,
            end,
        });

        if end >= text.len() {
            return;
        }
        start = end;
    }
}

/// The row holding position `col` of line `line`.
///
/// Returns 0 if the line has not been laid out, which only happens with an
/// empty buffer.
#[must_use]
pub fn row_of(rows: &[VisualRow], line: usize, col: usize) -> usize {
    rows.iter()
        .rposition(|row| row.line == line && row.start <= col)
        .unwrap_or(0)
}

/// The columns a text occupies on screen.
#[must_use]
pub fn width_of(text: &str) -> usize {
    text.chars().map(|c| c.width().unwrap_or(0)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrapped(text: &str, width: usize) -> Vec<String> {
        let lines = vec![text.to_owned()];
        rows(&lines, width)
            .iter()
            .map(|row| lines[row.line][row.start..row.end].to_owned())
            .collect()
    }

    #[test]
    fn what_fits_stays_on_one_row() {
        assert_eq!(wrapped("GET /x", 20), vec!["GET /x"]);
    }

    #[test]
    fn an_empty_line_still_occupies_a_row() {
        // The blank line separating headers from body is meaningful in
        // `.http`: were it to vanish, the editor could not show it.
        assert_eq!(wrapped("", 20), vec![""]);
    }

    #[test]
    fn cuts_at_the_last_space_that_fits() {
        assert_eq!(wrapped("one two three", 8), vec!["one two ", "three"]);
    }

    #[test]
    fn splits_a_word_only_when_there_are_no_spaces() {
        assert_eq!(
            wrapped("https://very.long", 8),
            vec!["https://", "very.lon", "g"]
        );
    }

    #[test]
    fn the_spans_cover_the_whole_line() {
        let text = "Authorization: Bearer {{token}} and some more text";
        assert_eq!(wrapped(text, 12).concat(), text);
    }

    #[test]
    fn counts_columns_and_not_bytes() {
        // "ñ" takes two bytes and one column: cutting by bytes would fit
        // less text than actually fits.
        assert_eq!(wrapped("ñññññ", 5), vec!["ñññññ"]);
    }

    #[test]
    fn a_width_of_one_column_does_not_hang() {
        assert_eq!(wrapped("abc", 1), vec!["a", "b", "c"]);
    }

    #[test]
    fn locates_the_cursors_row() {
        let lines = vec!["one two three".to_owned()];
        let rows = rows(&lines, 8);

        assert_eq!(row_of(&rows, 0, 0), 0);
        assert_eq!(row_of(&rows, 0, 7), 0);
        assert_eq!(row_of(&rows, 0, 8), 1);
        // The end of the line falls on the last row, not past it.
        assert_eq!(row_of(&rows, 0, 13), 1);
    }

    #[test]
    fn the_cursors_row_counts_the_preceding_lines() {
        let lines = vec!["one two three".to_owned(), "another".to_owned()];
        let rows = rows(&lines, 8);

        assert_eq!(rows.len(), 3);
        assert_eq!(row_of(&rows, 1, 0), 2);
    }
}
