//! Waiting on the language server: how long a `Ctrl-]` keeps asking, what it
//! means when the answer never comes, and what `:lsp` says in the meantime.

use std::path::Path;

use crate::{
    error::ViviError,
    lsp::{Encoding, Lsp},
};

/// rust-analyzer took about six seconds to index this repo from cold; at two
/// attempts a second this gives it a generous half a minute.
pub const GOTO_ATTEMPTS: u32 = 60;
pub const GOTO_RETRY_TICKS: usize = 6;

/// A `Ctrl-]` waiting on the language server. The server may answer "nothing
/// here" simply because it has not finished indexing, so we ask again until it
/// either finds something or we run out of patience.
pub struct PendingGoto {
    /// The request the server owes an answer to; later replies are stale.
    pub id: u64,
    /// How many times we have asked, counting up to `GOTO_ATTEMPTS`.
    pub attempts: u32,
    /// The tick to ask again on, if nothing has come back by then.
    pub retry_at: usize,
    /// The row we asked about, remembered so the jump list can go back to it.
    pub row: usize,
    /// The column we asked about.
    pub col: usize,
    /// What we are looking up, so a failure can name it.
    pub symbol: String,
}

impl PendingGoto {
    /// A lookup just asked for the first time, due to be asked again a few
    /// ticks from now if nothing comes back.
    pub fn new(id: u64, tick: usize, row: usize, col: usize, symbol: String) -> Self {
        Self { id, attempts: 1, retry_at: tick + GOTO_RETRY_TICKS, row, col, symbol }
    }

    /// The same lookup, asked once more.
    pub fn again(self, id: u64, tick: usize) -> Self {
        Self { id, attempts: self.attempts + 1, retry_at: tick + GOTO_RETRY_TICKS, ..self }
    }
}

/// Why a `Ctrl-]` came back with nothing, in terms of what actually went wrong
/// rather than collapsing every cause into "not found". The flag is `true` when
/// the explanation is longer than a status line has room for, and belongs in
/// `:messages` instead.
pub fn failure(lsp: &mut Lsp, symbol: &str, attempts: u32) -> (ViviError, bool) {
    let name = lsp.name.clone();
    let complaint = lsp.last_error();
    let (ready, indexing) = (lsp.ready, lsp.indexing);

    // A server that died or never started failed on its own account, and
    // explaining that takes more room than a status line has.
    if let Some(status) = lsp.exited() {
        (ViviError::ServerExited { status, detail: complaint }, true)
    } else if !ready {
        (ViviError::ServerNeverStarted { server: name, detail: complaint }, true)
    } else if indexing {
        (ViviError::ServerIndexing { server: name, attempts }, false)
    } else {
        // This one is about your cursor, not the server: short, and worth
        // reading where you are looking.
        (ViviError::NoDefinition(symbol.to_string()), false)
    }
}

/// `:lsp` — what the server is up to, or, before there is one, which server a
/// jump would start and whether it is even installed.
pub fn describe(lsp: Option<&mut Lsp>, path: &Path) -> String {
    let Some(lsp) = lsp else {
        return match Lsp::command_for(path) {
            Ok((program, _)) => format!("not started; Ctrl-] would run {}", program.display()),
            Err(e) => e.to_string(),
        };
    };
    let state = if let Some(exited) = lsp.exited() {
        exited
    } else if !lsp.ready {
        "starting".to_string()
    } else if lsp.indexing {
        "indexing".to_string()
    } else {
        "ready".to_string()
    };
    let encoding = if lsp.encoding == Encoding::Utf8 { "utf-8" } else { "utf-16" };
    let root = Lsp::project_root(path);
    let mut text = format!("{} · {state} · {encoding} · root {}", lsp.name, root.display());
    if let Some(complaint) = lsp.last_error() {
        text.push_str(&format!(" · {complaint}"));
    }
    text
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::{
        agent::tests::temp_env,
        buffer::Buffer,
        editor::{
            harness::{fake_server, last_said, settle, tag_fixture},
            App,
        },
        text::symbol_at,
    };

    #[test]
    fn ctrl_bracket_needs_a_symbol_under_the_cursor() {
        let (caller, _) = tag_fixture("no-symbol");
        let mut app = App::new(Buffer::from_file(&caller).unwrap());
        app.place_cursor(1, 9); // the `{` of `fn main() {`

        app.on_key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL));
        let (text, is_error) = app.message.clone().unwrap();
        assert!(!is_error, "not a failure: {text}");
        assert!(text.contains("no symbol under the cursor"), "{text}");
        assert!(app.lsp.is_none(), "and no server was started for nothing");
    }

    #[test]
    fn a_missing_language_server_is_reported_once() {
        let (caller, _) = tag_fixture("no-server");
        let mut app = App::new(Buffer::from_file(&caller).unwrap());

        temp_env(&[("VIVI_LSP", "definitely-not-a-real-binary")], || {
            app.on_key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL));
            let (text, is_error) = last_said(&app);
            assert!(is_error && text.contains("not found on PATH"), "{text}");
            assert!(app.goto.is_none(), "nothing is left pending");
            assert_eq!(app.lsp_outcome(), Some(false), "and the status line shows a red mark");

            // A second try must not spawn anything either.
            app.message = None;
            app.on_key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL));
            assert!(app.lsp.is_none());
        });
    }

    #[test]
    fn an_unknown_file_type_says_so() {
        let dir = std::env::temp_dir().join("vivi-tag-cobol");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("legacy.cobol");
        std::fs::write(&path, "IDENTIFICATION DIVISION.\n").unwrap();
        let mut app = App::new(Buffer::from_file(&path).unwrap());

        temp_env(&[("VIVI_LSP", "")], || {
            app.on_key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL));
        });
        let (text, is_error) = last_said(&app);
        assert!(is_error && text.contains("no language server known"), "{text}");
        assert!(text.contains("cobol"), "it names the extension: {text}");
    }

    #[test]
    fn a_server_that_dies_is_reported_with_its_complaint() {
        let server = fake_server("dies", "#!/bin/sh\necho 'cannot find Cargo.toml' >&2\nexit 1\n");
        let (caller, _) = tag_fixture("dies");
        let mut app = App::new(Buffer::from_file(&caller).unwrap());
        app.place_cursor(2, 4);

        temp_env(&[("VIVI_LSP", &server.display().to_string())], || {
            app.on_key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL));
            settle(&mut app);
        });

        let (text, is_error) = last_said(&app);
        assert!(is_error, "{text}");
        assert!(text.contains("exited with status 1"), "it says the server died: {text}");
        assert!(text.contains("cannot find Cargo.toml"), "and why: {text}");
        assert!(app.lsp.is_none(), "the dead server is dropped so a retry is possible");
        assert_eq!(app.lsp_outcome(), Some(false), "and the status line shows a red mark");
    }

    #[test]
    fn a_server_that_never_answers_is_reported_as_such() {
        // Accepts the connection, says nothing, keeps stdin open.
        let server = fake_server("mute", "#!/bin/sh\necho 'warming up' >&2\ncat > /dev/null\n");
        let (caller, _) = tag_fixture("mute");
        let mut app = App::new(Buffer::from_file(&caller).unwrap());
        app.place_cursor(2, 4);

        temp_env(&[("VIVI_LSP", &server.display().to_string())], || {
            app.on_key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL));

            // Wait for the server's stderr to actually reach us — it arrives on
            // its own thread, and the point of this test is that we quote it.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while std::time::Instant::now() < deadline
                && app.lsp.as_ref().and_then(|l| l.last_error()).is_none()
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
                app.drain_lsp();
            }

            // No reply ever arrives, so nothing settles it; force the timeout.
            let pending = app.goto.take().expect("still pending, nothing answered");
            app.goto_failed(&pending.symbol, pending.attempts);
        });

        let (text, is_error) = last_said(&app);
        assert!(is_error, "{text}");
        assert!(text.contains("never finished starting"), "{text}");
        assert!(text.contains("warming up"), "it quotes the server's own words: {text}");
        assert_eq!(app.lsp_outcome(), Some(false), "and the status line shows a red mark");
    }

    #[test]
    fn a_symbol_the_server_cannot_place_names_the_symbol() {
        let (caller, _) = tag_fixture("unknown-symbol");
        let mut app = App::new(Buffer::from_file(&caller).unwrap());
        app.place_cursor(2, 4);

        // Pretend a ready, idle server answered "nothing here" often enough.
        temp_env(&[("VIVI_LSP", "")], || {
            if let Ok(mut lsp) = Lsp::start(&caller) {
                lsp.ready = true;
                lsp.indexing = false;
                app.lsp = Some(lsp);
                app.goto_failed("greet", GOTO_ATTEMPTS);
                let (text, is_error) = app.message.clone().unwrap();
                assert!(is_error, "{text}");
                assert!(text.contains("no definition for greet"), "{text}");
            }
        });
    }

    #[test]
    fn lsp_command_explains_the_state_before_anything_is_started() {
        let (caller, _) = tag_fixture("status");
        let mut app = App::new(Buffer::from_file(&caller).unwrap());

        temp_env(&[("VIVI_LSP", "")], || app.run_command("lsp"));
        let (text, _) = app.message.clone().unwrap();
        assert!(text.contains("not started") || text.contains("not found"), "{text}");

        temp_env(&[("VIVI_LSP", "definitely-not-a-real-binary")], || app.run_command("lsp"));
        let (text, _) = app.message.clone().unwrap();
        assert!(text.contains("not found on PATH"), "{text}");
    }

    /// The whole feature, from keypress to cursor: open a file, put the cursor
    /// on a symbol defined in another file, press Ctrl-], and end up there.
    #[test]
    #[ignore = "starts rust-analyzer and waits for it to index the crate"]
    fn ctrl_bracket_jumps_to_a_definition_in_another_file() {
        let path = std::fs::canonicalize("editor/ui.rs").unwrap();
        let mut app = App::new(Buffer::from_file(&path).unwrap());

        let (row, line) = app
            .buffer
            .lines
            .iter()
            .enumerate()
            .find(|(_, l)| l.contains("let text = expand_tabs("))
            .map(|(i, l)| (i, l.clone()))
            .expect("the call site this test is anchored to");
        app.place_cursor(row, line.find("expand_tabs").unwrap() + 2);
        let from = (app.row, app.col);

        // Exactly what an ordinary terminal delivers for Ctrl-].
        app.on_key(KeyEvent::new(KeyCode::Char('5'), KeyModifiers::CONTROL));
        assert!(app.goto.is_some(), "the keypress started a lookup");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while app.goto.is_some() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
            app.tick += 1;
            app.drain_lsp();
        }
        assert!(app.goto.is_none(), "the lookup never settled");

        assert!(app.message.is_none(), "a jump that worked says nothing: {:?}", app.message);
        assert!(
            app.buffer.path.as_ref().is_some_and(|p| p.ends_with("text.rs")),
            "landed in {:?}",
            app.buffer.path
        );
        assert!(
            app.buffer.line(app.row).contains("fn expand_tabs"),
            "landed on {:?}",
            app.buffer.line(app.row)
        );
        assert_eq!(
            symbol_at(app.buffer.line(app.row), app.col),
            "expand_tabs",
            "the cursor sits on the name itself"
        );

        // And Ctrl-T comes straight back to where we were.
        app.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert!(app.buffer.path.as_ref().is_some_and(|p| p.ends_with("ui.rs")));
        assert_eq!((app.row, app.col), from, "back to the exact starting position");
    }
}
