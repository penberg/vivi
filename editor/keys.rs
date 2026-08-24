//! The key bindings, as a table: what each key does.

/// One key binding, in the same spirit as the ex command table: `:help` and
/// `MANUAL.md` are both checked against it, so they cannot drift apart.
pub struct Binding {
    pub group: &'static str,
    pub keys: &'static str,
    pub help: &'static str,
}

pub const BINDINGS: [Binding; 20] = [
    Binding { group: "moving", keys: "h j k l", help: "left, down, up, right (arrows work too)" },
    Binding { group: "moving", keys: "w b", help: "next / previous word" },
    Binding { group: "moving", keys: "0 ^ $", help: "start / first non-blank / end of line" },
    Binding { group: "moving", keys: "gg G", help: "first / last line" },
    Binding { group: "moving", keys: "Ctrl-D Ctrl-U", help: "half a screen down / up" },
    Binding { group: "moving", keys: "Ctrl-F Ctrl-B", help: "a screen down / up" },
    Binding { group: "moving", keys: "Ctrl-E Ctrl-Y", help: "scroll a line, keeping the cursor" },
    Binding {
        group: "jumping",
        keys: "gd",
        help: "jump to the definition of the symbol under the cursor",
    },
    Binding {
        group: "jumping",
        keys: "Ctrl-]",
        help: "the same, when the terminal can send that chord",
    },
    Binding { group: "jumping", keys: "Ctrl-T", help: "unwind one jump" },
    Binding { group: "selecting", keys: "V", help: "start a linewise selection" },
    Binding { group: "selecting", keys: ":", help: "in a selection, prefills the range `\'<,\'>`" },
    Binding { group: "selecting", keys: "Esc", help: "cancel a selection, or dismiss an error" },
    Binding { group: "deleting", keys: "dd", help: "delete the current line (:w writes it back)" },
    Binding { group: "deleting", keys: "d", help: "in a selection, delete the selected lines" },
    Binding { group: "panes", keys: "Ctrl-W w", help: "move between the buffer and the pane" },
    Binding { group: "panes", keys: "Ctrl-W c", help: "close the pane" },
    Binding { group: "panes", keys: "j k Ctrl-D Ctrl-U", help: "scroll the open pane" },
    Binding { group: "panes", keys: "g G", help: "top / bottom of the pane" },
    Binding { group: "panes", keys: "q", help: "close the pane" },
];

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::{
        buffer::Buffer,
        editor::{
            app::Mode,
            harness::{app, press, tag_fixture},
            App,
        },
    };

    #[test]
    fn visual_mode_selects_lines_in_both_directions() {
        let mut app = app("one\ntwo\nthree\nfour");
        assert_eq!(app.selection(), None);

        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('V'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selection(), Some((1, 2)));

        // Selecting upwards past the anchor keeps the range ordered.
        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.selection(), Some((0, 1)));

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.selection(), None);
        assert!(app.mode == Mode::Normal);
    }

    #[test]
    fn the_window_prefix_does_not_swallow_w() {
        // `w` alone is still a word motion.
        let mut app = app("foo bar baz");
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(app.col, 4, "plain w still moves by a word");
    }

    #[test]
    fn g_then_d_does_not_break_gg_or_ctrl_d() {
        let text: Vec<String> = (0..40).map(|i| format!("line {i}")).collect();
        let mut app = app(&text.join("\n"));

        app.goto_line(20);
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.row, 0, "gg still goes to the top");

        // A pending `g` must not hijack Ctrl-D.
        press(&mut app, KeyCode::Char('g'));
        app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(app.row, 2, "Ctrl-D still pages");
    }

    #[test]
    fn gd_jumps_without_needing_ctrl_bracket() {
        let (caller, _) = tag_fixture("gd");
        let mut app = App::new(Buffer::from_file(&caller).unwrap());
        app.place_cursor(1, 9); // on `{`, so it stops before starting a server

        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('d'));
        let (text, is_error) = app.message.clone().expect("gd must reach goto_definition");
        assert!(!is_error && text.contains("no symbol"), "{text}");
    }

    #[test]
    fn ctrl_bracket_arrives_in_both_terminal_dialects() {
        // 0x1D reaches us as Ctrl-'5' on an ordinary terminal and as Ctrl-']'
        // under the kitty keyboard protocol. Both must trigger the jump.
        for code in [KeyCode::Char(']'), KeyCode::Char('5')] {
            let (caller, _) = tag_fixture("dialects");
            let mut app = App::new(Buffer::from_file(&caller).unwrap());
            app.place_cursor(1, 9); // on `{`, so it stops before starting a server

            app.on_key(KeyEvent::new(code, KeyModifiers::CONTROL));
            let (text, is_error) = app
                .message
                .clone()
                .unwrap_or_else(|| panic!("{code:?} did not reach goto_definition"));
            assert!(!is_error && text.contains("no symbol"), "{code:?}: {text}");
        }
    }

    #[test]
    fn plain_5_is_not_a_jump() {
        let (caller, _) = tag_fixture("plain-5");
        let mut app = App::new(Buffer::from_file(&caller).unwrap());
        app.place_cursor(1, 9);
        press(&mut app, KeyCode::Char('5'));
        assert!(app.goto.is_none(), "'5' without Ctrl must not jump");
        assert!(app.message.is_none(), "and it says nothing about it");
    }

    #[test]
    fn an_unbound_key_is_swallowed() {
        let mut app = app("hello\nworld");
        let (row, col) = (app.row, app.col);

        press(&mut app, KeyCode::Char('i'));
        app.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
        // The key a Nordic layout actually delivers for Ctrl-]: a bare '9'.
        press(&mut app, KeyCode::Char('9'));

        assert!(app.message.is_none(), "an unbound key is not worth a message");
        assert_eq!((app.row, app.col), (row, col), "nor does it move the cursor");
    }

    #[test]
    fn errors_survive_the_next_keystroke() {
        let mut app = app("hello\nworld");

        app.message = Some(("something went wrong".to_string(), true));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(
            app.message.clone().unwrap().0,
            "something went wrong",
            "an error must outlive the next key, or it is never read"
        );

        // Informational messages do get out of the way.
        app.message = Some(("just so you know".to_string(), false));
        press(&mut app, KeyCode::Char('j'));
        assert!(app.message.is_none());

        // And Esc dismisses a stuck error.
        app.message = Some(("something went wrong".to_string(), true));
        press(&mut app, KeyCode::Esc);
        assert!(app.message.is_none());
    }
}
