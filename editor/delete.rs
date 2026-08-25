//! Deleting lines — the one edit the editor makes itself. A delete changes
//! only the buffer; `:w` is what puts it on disk.

use crate::{buffer::LineRange, editor::app::App};

impl App {
    /// Delete the lines in `range`, in the buffer. The file keeps them until
    /// `:w` writes it, and `[+]` on the status line says so.
    pub fn delete_lines(&mut self, (start, end): LineRange) {
        // The agent may be about to rewrite the very lines being deleted, and
        // the range it was handed is marked on screen; pulling lines out from
        // under it would make both wrong.
        if self.agent_running() {
            self.note("an agent is still working");
            return;
        }
        let removed = self.buffer.delete_lines(start, end);
        if removed == 0 {
            self.note("the buffer is already empty");
            return;
        }
        // The language server is synced from the buffer, not the disk: `gd`
        // must answer about the lines you see.
        if let Some(path) = self.buffer.path.clone() {
            if let Some(lsp) = &mut self.lsp {
                lsp.did_open(&path, &self.buffer.text());
            }
        }
        self.goto_line(start);
        // One line vanishing is visible on its own; a larger cut gets a count.
        if removed > 1 {
            self.note(format!("{removed} fewer lines"));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::{
        buffer::Buffer,
        editor::{
            app::{App, Mode},
            harness::{app, job, press, temp_file},
        },
    };

    #[test]
    fn dd_deletes_in_the_buffer_and_w_writes_it() {
        let path = temp_file("dd", "one\ntwo\nthree\n");
        let mut app = App::new(Buffer::from_file(&path).unwrap());
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('d'));

        assert_eq!(app.buffer.lines, ["one", "three"]);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "one\ntwo\nthree\n",
            "the delete stays in the buffer until :w"
        );
        assert!(app.buffer.modified, "and the buffer knows it is ahead of the file");
        assert_eq!(app.row, 1, "the cursor stays put, on the line that moved up");
        assert!(app.message.is_none(), "one line vanishing speaks for itself");

        app.run_command("w");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\nthree\n");
        assert!(!app.buffer.modified, ":w settles the buffer");
        let (text, is_error) = app.message.clone().unwrap();
        assert!(!is_error && text.contains("written"), "{text}");

        // Our own write must not be mistaken for someone else's edit.
        app.message = None;
        app.poll_file();
        assert!(app.message.is_none(), "no phantom reload after our own write");
    }

    #[test]
    fn a_lone_d_is_a_prefix_not_a_delete() {
        let path = temp_file("d-prefix", "one\ntwo\nthree\n");
        let mut app = App::new(Buffer::from_file(&path).unwrap());

        // `d` then a motion: nothing is deleted, and the motion still moves.
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.buffer.len(), 3, "d followed by a motion deletes nothing");
        assert_eq!(app.row, 1, "and the motion still moves");

        // A pending `d` must not hijack Ctrl-D either.
        press(&mut app, KeyCode::Char('d'));
        app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(app.buffer.len(), 3, "Ctrl-D still pages");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\nthree\n");
    }

    #[test]
    fn visual_d_deletes_the_selection() {
        let path = temp_file("visual-d", "one\ntwo\nthree\nfour\n");
        let mut app = App::new(Buffer::from_file(&path).unwrap());
        press(&mut app, KeyCode::Char('V'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('d'));

        assert_eq!(app.buffer.lines, ["three", "four"]);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "one\ntwo\nthree\nfour\n",
            "nothing reaches the disk without :w"
        );
        assert!(app.buffer.modified);
        assert!(app.mode == Mode::Normal, "deleting the selection ends it");
        assert_eq!(app.selection(), None);
        assert_eq!(app.last_selection, None, "'<,'> must not point at deleted lines");
        let (text, is_error) = app.message.clone().unwrap();
        assert!(!is_error && text == "2 fewer lines", "{text}");
    }

    #[test]
    fn the_ex_command_deletes_a_range() {
        let path = temp_file("ex-delete", "one\ntwo\nthree\nfour\n");
        let mut app = App::new(Buffer::from_file(&path).unwrap());
        app.run_command("2,3d");
        assert_eq!(app.buffer.lines, ["one", "four"]);

        // With no range it takes the line the cursor is on, like vi.
        app.goto_line(1);
        app.run_command("delete");
        assert_eq!(app.buffer.lines, ["one"]);

        // And `%` empties the buffer down to vi's single empty line.
        app.run_command("%d");
        assert_eq!(app.buffer.lines, [""]);
        app.run_command("w");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "\n");

        // Deleting what is not there is not a failure, but it says so.
        app.run_command("d");
        let (text, is_error) = app.message.clone().unwrap();
        assert!(!is_error && text.contains("already empty"), "{text}");
        assert!(!app.buffer.modified, "and it does not dirty the buffer");
    }

    #[test]
    fn deleting_the_last_line_pulls_the_cursor_up() {
        let path = temp_file("dd-last", "one\ntwo\n");
        let mut app = App::new(Buffer::from_file(&path).unwrap());
        press(&mut app, KeyCode::Char('G'));
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.buffer.lines, ["one"]);
        assert_eq!((app.row, app.col), (0, 0), "the cursor cannot stay past the end");
    }

    #[test]
    fn a_delete_is_refused_while_the_agent_works() {
        // The agent was handed a range of lines; deleting under it would make
        // both its marked range and its coming rewrite wrong.
        let mut app = app("one\ntwo\nthree");
        let mut running = job(mpsc::channel().1);
        running.running = true;
        app.job = Some(running);

        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.buffer.len(), 3, "nothing is deleted");
        let (text, is_error) = app.message.clone().unwrap();
        assert!(!is_error && text.contains("still working"), "{text}");
    }
}
