//! Searching: `/` and `?` take a pattern from the input line, `n` and `N` go
//! to the next match of it. The pattern is plain text, matched exactly — no
//! regular expressions, no case folding — and the search wraps round the end
//! of the file, saying so when it does.

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::{
    buffer::Buffer,
    editor::{
        app::{App, Mode},
        motion::Position,
    },
    error::ViviError,
};

/// The last search, which `n` and `N` repeat.
pub struct Search {
    /// The text looked for.
    pub pattern: String,
    /// Whether it was `?` rather than `/`, so `n` keeps going that way.
    pub backward: bool,
}

impl App {
    /// `/` and `?`: open the input line for a pattern.
    pub fn start_search(&mut self, backward: bool) {
        self.mode = Mode::Search { backward };
        self.command.clear();
    }

    pub fn on_search_key(&mut self, key: KeyEvent) {
        let Mode::Search { backward } = self.mode else { return };
        match key.code {
            KeyCode::Esc => self.end_search(),
            KeyCode::Backspace => {
                if self.command.pop().is_none() {
                    self.end_search();
                }
            }
            KeyCode::Enter => {
                let pattern = std::mem::take(&mut self.command);
                self.end_search();
                self.search(pattern, backward);
            }
            KeyCode::Char(c) => self.command.push(c),
            _ => {}
        }
    }

    /// Back to where `/` was typed. A search is a motion, so a selection that
    /// was up stays up and the match extends it, as in vi.
    fn end_search(&mut self) {
        self.command.clear();
        self.mode = if self.visual_anchor.is_some() { Mode::Visual } else { Mode::Normal };
    }

    /// Look for `pattern` from the cursor, and remember it for `n`. An empty
    /// pattern is vi's shorthand for the last one, in whichever direction
    /// this one was typed.
    pub fn search(&mut self, pattern: String, backward: bool) {
        let pattern = if pattern.is_empty() {
            let Some(last) = &self.last_search else {
                self.fail(ViviError::NoPreviousSearch);
                return;
            };
            last.pattern.clone()
        } else {
            pattern
        };
        self.last_search = Some(Search { pattern, backward });
        self.search_again(false);
    }

    /// `n` and `N`: the last search once more, the same way or the other way.
    pub fn search_again(&mut self, reverse: bool) {
        let Some(last) = &self.last_search else {
            self.fail(ViviError::NoPreviousSearch);
            return;
        };
        let backward = last.backward != reverse;
        let from = (self.row, self.col);
        let found = if backward {
            find_backward(&self.buffer, from, &last.pattern)
        } else {
            find_forward(&self.buffer, from, &last.pattern)
        };
        match found {
            Some(((row, col), wrapped)) => {
                self.place_cursor(row, col);
                // Not a failure, but worth knowing: the next match is behind you.
                if wrapped {
                    self.note(if backward {
                        "search hit TOP, continuing at BOTTOM"
                    } else {
                        "search hit BOTTOM, continuing at TOP"
                    });
                }
            }
            None => self.fail(ViviError::PatternNotFound(last.pattern.clone())),
        }
    }
}

/// The first match after the cursor, wrapping round to the top and back to the
/// cursor itself. Says whether it wrapped. Comes up empty only when the pattern
/// is nowhere in the buffer at all.
pub fn find_forward(
    buffer: &Buffer,
    (row, col): Position,
    pattern: &str,
) -> Option<(Position, bool)> {
    let len = buffer.len();
    // One step past a full turn lands back on the cursor's own line, for the
    // matches at or before the cursor that the first step skipped.
    for step in 0..=len {
        let r = (row + step) % len;
        let mut cols = columns(buffer.line(r), pattern);
        let hit = match step {
            0 => cols.find(|&c| c > col),
            _ if step == len => cols.find(|&c| c <= col),
            _ => cols.next(),
        };
        if let Some(c) = hit {
            return Some(((r, c), step + row >= len));
        }
    }
    None
}

/// The last match before the cursor, wrapping round to the bottom and back.
pub fn find_backward(
    buffer: &Buffer,
    (row, col): Position,
    pattern: &str,
) -> Option<(Position, bool)> {
    let len = buffer.len();
    for step in 0..=len {
        let r = (row + len - step % len) % len;
        let cols = columns(buffer.line(r), pattern);
        let hit = match step {
            0 => cols.filter(|&c| c < col).last(),
            _ if step == len => cols.filter(|&c| c >= col).last(),
            _ => cols.last(),
        };
        if let Some(c) = hit {
            return Some(((r, c), step > row));
        }
    }
    None
}

/// Every column the pattern starts at, in characters, overlapping matches
/// included — `aa` occurs twice in `aaa`, and `n` should visit both.
fn columns<'a>(line: &'a str, pattern: &'a str) -> impl Iterator<Item = usize> + 'a {
    line.char_indices()
        .enumerate()
        .filter_map(move |(col, (byte, _))| line[byte..].starts_with(pattern).then_some(col))
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyCode;

    use crate::editor::{
        app::Mode,
        harness::{app, press},
        App,
    };

    #[test]
    fn slash_finds_the_next_match_and_n_walks_on_round_the_file() {
        let mut app = app("foo bar\nbaz foo\nfoo");
        search(&mut app, "/foo");
        assert!(app.mode == Mode::Normal, "the input line is gone");
        assert_eq!((app.row, app.col), (1, 4), "the match after the cursor, not the one under it");
        assert!(app.message.is_none(), "landing is its own announcement");

        press(&mut app, KeyCode::Char('n'));
        assert_eq!((app.row, app.col), (2, 0));

        // Off the bottom and round to the top, and it says so — dimmed, since
        // nothing went wrong.
        press(&mut app, KeyCode::Char('n'));
        assert_eq!((app.row, app.col), (0, 0));
        let (text, is_error) = app.message.clone().unwrap();
        assert!(!is_error && text == "search hit BOTTOM, continuing at TOP", "{text}");

        // N goes the other way, and wraps the other way.
        press(&mut app, KeyCode::Char('N'));
        assert_eq!((app.row, app.col), (2, 0));
        let (text, _) = app.message.clone().unwrap();
        assert_eq!(text, "search hit TOP, continuing at BOTTOM");
        press(&mut app, KeyCode::Char('N'));
        assert_eq!((app.row, app.col), (1, 4));
        assert!(app.message.is_none(), "no wrap, nothing to say");

        // The wanted column follows the match, like any other motion.
        press(&mut app, KeyCode::Char('j'));
        assert_eq!((app.row, app.col), (2, 2), "clamped to the short line, from column 4");
    }

    #[test]
    fn question_mark_searches_backward_and_n_keeps_going_that_way() {
        let mut app = app("foo bar\nbaz foo\nfoo");
        app.place_cursor(1, 6);

        search(&mut app, "?foo");
        assert_eq!((app.row, app.col), (1, 4), "the match before the cursor on the same line");
        press(&mut app, KeyCode::Char('n'));
        assert_eq!((app.row, app.col), (0, 0), "n follows the direction of the search");
        press(&mut app, KeyCode::Char('n'));
        assert_eq!((app.row, app.col), (2, 0), "and wraps to the bottom");
        assert_eq!(app.message.clone().unwrap().0, "search hit TOP, continuing at BOTTOM");
        press(&mut app, KeyCode::Char('N'));
        assert_eq!((app.row, app.col), (0, 0), "N is forward when the search was backward");
    }

    #[test]
    fn a_pattern_that_is_not_there_is_an_error_and_moves_nothing() {
        let mut app = app("one\ntwo\nthree");
        app.place_cursor(1, 1);
        search(&mut app, "/four");
        assert_eq!((app.row, app.col), (1, 1));
        let (text, is_error) = app.message.clone().unwrap();
        assert!(is_error && text == "pattern not found: four", "{text}");

        // And it is still the last search, so n complains the same way rather
        // than pretending there was never one.
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.message.clone().unwrap().0, "pattern not found: four");
    }

    #[test]
    fn n_before_any_search_says_there_was_none() {
        let mut app = app("one\ntwo");
        for key in ['n', 'N'] {
            press(&mut app, KeyCode::Char(key));
            let (text, is_error) = app.message.clone().unwrap();
            assert!(is_error && text == "no previous search", "{key}: {text}");
            assert_eq!((app.row, app.col), (0, 0), "{key} moved the cursor");
        }
        search(&mut app, "/");
        assert_eq!(app.message.clone().unwrap().0, "no previous search");
    }

    #[test]
    fn an_empty_pattern_repeats_the_last_one_in_the_new_direction() {
        let mut app = app("foo\nfoo\nfoo");
        search(&mut app, "/foo");
        assert_eq!(app.row, 1);
        // `?` then Enter: the same pattern, but backwards from here on.
        search(&mut app, "?");
        assert_eq!(app.row, 0);
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.row, 2, "n now goes backward, wrapping to the bottom");
        search(&mut app, "/");
        assert_eq!(app.row, 0, "and `/` turns it round again");
    }

    #[test]
    fn escape_abandons_the_line_but_not_the_last_search() {
        let mut app = app("foo\nbar\nfoo");
        search(&mut app, "/foo");
        assert_eq!(app.row, 2);

        press(&mut app, KeyCode::Char('/'));
        for c in "bar".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        assert!(matches!(app.mode, Mode::Search { backward: false }));
        assert_eq!(app.command, "bar");
        press(&mut app, KeyCode::Esc);
        assert!(app.mode == Mode::Normal);
        assert!(app.command.is_empty(), "nothing left over for the next `/`");
        assert_eq!(app.row, 2, "cancelling does not move");

        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.row, 0, "n still repeats `foo`, not the abandoned `bar`");

        // Backspacing past the start leaves the same way.
        press(&mut app, KeyCode::Char('?'));
        press(&mut app, KeyCode::Char('x'));
        press(&mut app, KeyCode::Backspace);
        assert!(matches!(app.mode, Mode::Search { backward: true }), "one character back");
        press(&mut app, KeyCode::Backspace);
        assert!(app.mode == Mode::Normal, "and one more leaves");
    }

    #[test]
    fn a_search_from_a_selection_extends_it() {
        let mut app = app("foo\nbar\nbaz\nfoo");
        press(&mut app, KeyCode::Char('V'));
        press(&mut app, KeyCode::Char('/'));
        assert_eq!(app.selection(), Some((0, 0)), "the highlight stays up while you type");
        for c in "foo".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        assert!(app.mode == Mode::Visual, "a search is a motion, so the selection survives it");
        assert_eq!(app.selection(), Some((0, 3)));

        // And a cancelled one goes back to the selection too.
        press(&mut app, KeyCode::Char('/'));
        press(&mut app, KeyCode::Esc);
        assert!(app.mode == Mode::Visual);
    }

    #[test]
    fn matches_may_overlap_and_columns_count_characters() {
        let mut app = app("aaa\näö aa");
        search(&mut app, "/aa");
        assert_eq!((app.row, app.col), (0, 1), "the second `aa` in `aaa` starts one along");
        press(&mut app, KeyCode::Char('n'));
        assert_eq!((app.row, app.col), (1, 3), "a column, not a byte offset, past the umlauts");
        press(&mut app, KeyCode::Char('n'));
        assert_eq!((app.row, app.col), (0, 0), "round to the first");
    }

    #[test]
    fn the_only_match_being_under_the_cursor_is_a_full_turn() {
        // Vi's answer too: the cursor stays put, and the wrap message says why
        // pressing `n` is not doing anything.
        let mut app = app("one\ntwo");
        app.place_cursor(1, 0);
        search(&mut app, "/two");
        assert_eq!((app.row, app.col), (1, 0));
        assert_eq!(app.message.clone().unwrap().0, "search hit BOTTOM, continuing at TOP");
        press(&mut app, KeyCode::Char('N'));
        assert_eq!((app.row, app.col), (1, 0));
        assert_eq!(app.message.clone().unwrap().0, "search hit TOP, continuing at BOTTOM");
    }

    /// Type a whole search — `/foo`, `?foo`, or a bare `/` — and press Enter.
    fn search(app: &mut App, line: &str) {
        for c in line.chars() {
            press(app, KeyCode::Char(c));
        }
        press(app, KeyCode::Enter);
    }
}
