//! What the editor's tests need: a live `App`, a rendered frame, and the files
//! and stub programs the real end-to-end paths read.

use std::{path::PathBuf, sync::mpsc::Receiver};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::{
    agent::{tests::stub_agent, AgentEvent, Job},
    buffer::{absolute, Buffer},
    editor::{
        app::App,
        goto::{GOTO_ATTEMPTS, GOTO_RETRY_TICKS},
    },
};

/// An unnamed buffer over `text`, with a four-line window and nothing said yet.
pub fn app(text: &str) -> App {
    let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    let mut app = App::new(Buffer { path: None, lines, is_new: false, stamp: None });
    app.view_height = 4;
    app.message = None;
    app
}

pub fn press(app: &mut App, code: KeyCode) {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
}

pub fn render(app: &mut App, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    terminal.backend().buffer().clone()
}

pub fn rows(buffer: &ratatui::buffer::Buffer) -> Vec<String> {
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

/// The most recent thing said, wherever it was said: the status line for
/// short messages, the log for the ones too long to fit there.
pub fn last_said(app: &App) -> (String, bool) {
    app.message
        .clone()
        .or_else(|| app.messages.last().cloned())
        .expect("something must have been said")
}

/// Drive the editor until a `Ctrl-]` settles, without waiting in real time.
pub fn settle(app: &mut App) {
    for _ in 0..(GOTO_ATTEMPTS as usize + 4) * (GOTO_RETRY_TICKS + 1) {
        app.tick += 1;
        app.drain_lsp();
        if app.goto.is_none() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// A finished, empty agent job. Tests set only the fields they care about.
pub fn job(rx: Receiver<AgentEvent>) -> Job {
    Job {
        rx,
        agent: "claude".into(),
        prompt: "explain".into(),
        output: Vec::new(),
        running: false,
        failed: false,
        offset: 0,
        follow: false,
        height: 1,
        open: false,
        range: None,
        shown: 0,
    }
}

/// A real file on disk, so reload tests exercise the actual read path.
pub fn temp_file(slot: &str, contents: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("vivi-reload-{slot}.txt"));
    std::fs::write(&path, contents).unwrap();
    path
}

/// Two files on disk, so jumps exercise the real open path.
pub fn tag_fixture(slot: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("vivi-tag-{slot}"));
    std::fs::create_dir_all(&dir).unwrap();
    let caller = dir.join("caller.rs");
    let target = dir.join("target.rs");
    std::fs::write(&caller, "use target;\nfn main() {\n    greet();\n}\n").unwrap();
    std::fs::write(&target, "pub fn greet() {\n    println!(\"hi\");\n}\n").unwrap();
    // Buffers normalise their paths, so the fixture must too or comparisons
    // trip over /var vs /private/var on macOS.
    (absolute(&caller), absolute(&target))
}

/// A fake language server, so failure paths are testable without a real one.
pub fn fake_server(slot: &str, script: &str) -> PathBuf {
    stub_agent(&format!("lsp-{slot}"), "fake-lsp", script)
}
