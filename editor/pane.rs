//! The scrollable panes: the document you read — `:help`, `:messages` — and the
//! arithmetic it and the agent's output pane both scroll by.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A document to read: `:help`, `:messages`. Not an agent job — it has no
/// state to report and nothing to stream, and it takes the whole screen because
/// there is no reason to look at the file at the same time.
pub struct Reader {
    /// What the pane calls itself.
    pub title: &'static str,
    /// The document, a line at a time.
    pub lines: Vec<String>,
    /// First visible display line.
    pub offset: usize,
    /// Rows of text on screen, set when it is drawn.
    pub height: usize,
    /// Rows after wrapping, which is what `offset` counts. Also set when it is
    /// drawn, since it depends on how wide the pane is.
    pub shown: usize,
}

impl Reader {
    pub fn new(title: &'static str, lines: Vec<String>) -> Self {
        Self { title, lines, offset: 0, height: 1, shown: 0 }
    }
}

/// What a keypress means to an open pane.
pub enum Scroll {
    /// `q` or `Esc`: put it away.
    Close,
    /// `Ctrl-W`: the window prefix, waiting for the key that says what to do.
    Prefix,
    /// Move this many display lines, which may be none — an unrecognised key
    /// still counts as the user taking hold of the pane.
    By(isize),
}

/// Read a keypress as a scroll: a line at a time, half a pane with `Ctrl-D` and
/// `Ctrl-U`, either end with `g` and `G`. `total` counts display lines after
/// wrapping, which is what an offset indexes.
pub fn scroll_key(key: KeyEvent, height: usize, offset: usize, total: usize) -> Scroll {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let page = (height / 2).max(1) as isize;
    Scroll::By(match key.code {
        KeyCode::Esc | KeyCode::Char('q') => return Scroll::Close,
        KeyCode::Char('w') if ctrl => return Scroll::Prefix,
        KeyCode::Char('j') | KeyCode::Down => 1,
        KeyCode::Char('k') | KeyCode::Up => -1,
        KeyCode::Char('d') if ctrl => page,
        KeyCode::Char('u') if ctrl => -page,
        KeyCode::Char('g') => -(offset as isize),
        KeyCode::Char('G') => total as isize,
        _ => 0,
    })
}

/// The furthest a pane may be scrolled: the last page, not past the end.
pub fn last_page(height: usize, total: usize) -> usize {
    total.saturating_sub(height)
}

/// Where a pane sits after scrolling, kept inside its own contents.
pub fn scrolled(offset: usize, delta: isize, height: usize, total: usize) -> usize {
    (offset as isize + delta).clamp(0, last_page(height, total) as isize) as usize
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::{
        agent::AgentEvent,
        editor::{
            app::Mode,
            harness::{app, job, press, render},
            App,
        },
    };

    #[test]
    fn ctrl_w_moves_between_the_buffer_and_the_pane() {
        let mut app = app("one\ntwo\nthree");
        let ctrl_w =
            |app: &mut App| app.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));

        // With no pane there is nowhere to go, and it says so.
        ctrl_w(&mut app);
        press(&mut app, KeyCode::Char('w'));
        let (text, is_error) = app.message.clone().unwrap();
        assert!(!is_error, "nothing to switch to is not a failure: {text}");
        assert_eq!(text, "no output to show");

        let mut reply = job(mpsc::channel().1);
        reply.output = vec!["a reply".into()];
        app.job = Some(reply);

        // Ctrl-W w opens the pane and moves into it...
        ctrl_w(&mut app);
        press(&mut app, KeyCode::Char('w'));
        assert!(app.mode == Mode::Output && app.job.as_ref().unwrap().open);

        // ...and again moves back, leaving the pane up.
        ctrl_w(&mut app);
        press(&mut app, KeyCode::Char('w'));
        assert!(app.mode == Mode::Normal, "back in the buffer");
        assert!(app.job.as_ref().unwrap().open, "the pane stays on screen");

        // Ctrl-W c closes it, keeping the output for next time.
        ctrl_w(&mut app);
        press(&mut app, KeyCode::Char('c'));
        assert!(app.mode == Mode::Normal);
        assert!(!app.job.as_ref().unwrap().open);
        assert_eq!(app.job.as_ref().unwrap().output, ["a reply"]);
    }

    #[test]
    fn output_pane_scrolls_and_closes() {
        let mut app = app("code");
        let mut reply = job(mpsc::channel().1);
        reply.output = (0..50).map(|i| format!("reply {i}")).collect();
        reply.height = 4;
        reply.open = true;
        app.job = Some(reply);
        app.mode = Mode::Output;

        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.job.as_ref().unwrap().offset, 1);
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(app.job.as_ref().unwrap().offset, 46, "clamped to the last page");
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.job.as_ref().unwrap().offset, 45);

        press(&mut app, KeyCode::Char('q'));
        assert!(app.mode == Mode::Normal);
        let job = app.job.as_ref().expect("the job outlives the pane, so :out can reopen it");
        assert!(!job.open);
        assert_eq!(job.offset, 45, "reopening returns you to where you were");

        app.run_command("out");
        assert!(app.job.as_ref().unwrap().open);
        assert!(app.mode == Mode::Output);
    }

    #[test]
    fn streaming_output_follows_the_tail_until_the_user_scrolls() {
        let (tx, rx) = mpsc::channel();
        let mut app = app("code");
        let mut streaming = job(rx);
        streaming.running = true;
        streaming.follow = true;
        streaming.height = 2;
        streaming.open = true;
        app.job = Some(streaming);
        app.mode = Mode::Output;

        for i in 0..6 {
            tx.send(AgentEvent::Line(format!("line {i}"))).unwrap();
        }
        app.drain_agent();
        render(&mut app, 20, 10);
        // The pane sizes itself from the frame, so read the height back off the job.
        let height = app.job.as_ref().unwrap().height;
        assert_eq!(app.job.as_ref().unwrap().offset, 6 - height, "pinned to the newest output");

        press(&mut app, KeyCode::Char('k'));
        assert!(!app.job.as_ref().unwrap().follow, "scrolling up stops following");
        tx.send(AgentEvent::Done(None)).unwrap();
        app.drain_agent();
        render(&mut app, 20, 10);
        assert_eq!(app.job.as_ref().unwrap().offset, 5 - height, "stays where the user left it");
        assert!(!app.job.as_ref().unwrap().running);
    }
}
