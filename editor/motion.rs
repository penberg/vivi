//! Where a motion lands. Every function here answers the same question — given
//! this buffer and this cursor, what is the next position — and hands it back
//! for the caller to move to. Nothing is moved here.

use crate::{
    buffer::Buffer,
    text::{char_class, first_non_blank, last_col},
};

/// A cursor position: a row in the buffer, and a column in characters.
pub type Position = (usize, usize);

/// `w` — the start of the next word, wrapping onto the next line at the end.
pub fn word_forward(buffer: &Buffer, row: usize, col: usize) -> Position {
    let chars: Vec<char> = buffer.line(row).chars().collect();
    let mut i = col;
    if i < chars.len() {
        let class = char_class(chars[i]);
        while i < chars.len() && char_class(chars[i]) == class {
            i += 1;
        }
    }
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    if i >= chars.len() && row + 1 < buffer.len() {
        let row = row + 1;
        return (row, first_non_blank(buffer.line(row)));
    }
    (row, i.min(last_col(buffer.line(row))))
}

/// `b` — the start of the word before the cursor, wrapping onto the line above.
pub fn word_backward(buffer: &Buffer, row: usize, col: usize) -> Position {
    if col == 0 {
        if row == 0 {
            return (0, 0);
        }
        let row = row - 1;
        return (row, last_col(buffer.line(row)));
    }
    let chars: Vec<char> = buffer.line(row).chars().collect();
    let mut i = col - 1;
    while i > 0 && chars[i].is_whitespace() {
        i -= 1;
    }
    let class = char_class(chars[i]);
    while i > 0 && char_class(chars[i - 1]) == class {
        i -= 1;
    }
    (row, i)
}

/// The nearest position the cursor may actually rest on, which is what a jump
/// into a file we have only just read has to be put through.
pub fn clamp(buffer: &Buffer, row: usize, col: usize) -> Position {
    let row = row.min(buffer.len() - 1);
    (row, col.min(last_col(buffer.line(row))))
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::editor::harness::{app, press};

    #[test]
    fn vertical_motion_remembers_column() {
        let mut app = app("long line here\nab\nanother long one");
        app.set_col(10);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.col, 1, "clamped to the short line");
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.col, 10, "restored on the long line");
    }

    #[test]
    fn dollar_sticks_to_end_of_line() {
        let mut app = app("hello\nhi\ngoodbye");
        press(&mut app, KeyCode::Char('$'));
        assert_eq!(app.col, 4);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.col, 1);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.col, 6);
    }

    #[test]
    fn gg_and_g_jump_to_first_and_last_line() {
        let mut app = app("one\ntwo\n  three");
        press(&mut app, KeyCode::Char('G'));
        assert_eq!((app.row, app.col), (2, 2), "G lands on first non-blank");
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.row, 0);
    }

    #[test]
    fn motion_clamps_at_buffer_edges() {
        let mut app = app("a\nb");
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.row, 0);
        for _ in 0..10 {
            press(&mut app, KeyCode::Char('j'));
        }
        assert_eq!(app.row, 1);
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(app.col, 0);
    }

    #[test]
    fn word_motions_cross_classes_and_lines() {
        let mut app = app("foo.bar baz\nqux");
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(app.col, 3, "stops at punctuation");
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(app.col, 4);
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(app.col, 8);
        press(&mut app, KeyCode::Char('w'));
        assert_eq!((app.row, app.col), (1, 0), "wraps to the next line");
        press(&mut app, KeyCode::Char('b'));
        assert_eq!((app.row, app.col), (0, 10));
    }

    #[test]
    fn paging_moves_by_half_and_whole_screens() {
        let text: Vec<String> = (0..20).map(|i| format!("line {i}")).collect();
        let mut app = app(&text.join("\n"));
        app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(app.row, 2, "half of a 4-line view");
        app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(app.row, 0);
    }

    #[test]
    fn ctrl_e_scrolls_without_losing_the_cursor() {
        let text: Vec<String> = (0..20).map(|i| format!("line {i}")).collect();
        let mut app = app(&text.join("\n"));
        app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert_eq!(app.row_offset, 1);
        assert_eq!(app.row, 1, "cursor is dragged along by the viewport");
    }
}
