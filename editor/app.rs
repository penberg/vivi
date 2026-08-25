//! The editor: its state, everything it does with that state, and the loop
//! that keeps it moving. Every other module under `editor` is a piece this one
//! reaches for — a table, a scrap of arithmetic, a way to draw something — and
//! none of them know that `App` exists.

use std::{
    path::Path,
    time::{Duration, Instant},
};

use ratatui::{
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::Rect,
    DefaultTerminal, Frame,
};

use crate::{
    agent::{self, Agent, AgentEvent, Job},
    buffer::{file_stamp, Buffer, LineRange},
    editor::{
        cmd::{self, Command},
        goto::{self, PendingGoto, GOTO_ATTEMPTS},
        jump::{self, Jump},
        motion,
        pane::{self, Reader, Scroll},
        ui,
    },
    error::ViviError,
    lsp::{Encoding, Location, Lsp, LspEvent},
    text::{
        byte_index, display_width, first_non_blank, from_lsp_character, last_col, symbol_at,
        to_lsp_character,
    },
};

/// Wake often enough that no frame is missed while something is running.
/// What the language server is called on the status line.
///
/// `symbols` is the word editors already use for this — "go to symbol", symbol
/// search, LSP's own `workspace/symbol` — so it needs no explaining. It names
/// the thing all three states are about (building the symbol index, having it,
/// failing to), rather than one use of it: `definitions` would be wrong the
/// moment references or hover arrive. `lsp` names a wire protocol and `index`
/// begs the question "of what".
const LSP_LABEL: &str = "symbols";

const BUSY_POLL: Duration = Duration::from_millis(40);
const IDLE_POLL: Duration = Duration::from_millis(400);

/// Braille dots: one cell wide, and it turns rather than flickers.
pub const SPINNER: [&str; 8] = ["⠋", "⠙", "⠸", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// One frame every 80ms — a full turn in about two thirds of a second, which
/// reads as working rather than stalled.
pub const SPINNER_FRAME: Duration = Duration::from_millis(80);

#[derive(PartialEq)]
pub enum Mode {
    /// Keys move the cursor and act on the buffer.
    Normal,
    /// Typing a `:` line, which runs when you press Enter.
    Command,
    /// Linewise selection, anchored at `visual_anchor`.
    Visual,
    /// Reading the agent's reply; keys scroll the output pane.
    Output,
}

/// Everything the editor knows: the file, where you are in it, what is on
/// screen, and whatever is running in the background.
pub struct App {
    /// The file being edited.
    pub buffer: Buffer,
    /// What a keypress means right now.
    pub mode: Mode,
    /// The `:` line as typed, without the colon. Holds a pending prefix — `g`,
    /// `^W` — while we wait for the key that finishes it.
    pub command: String,
    /// What the status line is saying, and whether it is an error.
    pub message: Option<(String, bool)>,
    /// Cursor row, in buffer coordinates.
    pub row: usize,
    /// Cursor column, in buffer coordinates.
    pub col: usize,
    /// Column the cursor wants to be at when moving vertically.
    pub want_col: usize,
    /// First visible line.
    pub row_offset: usize,
    /// First visible display column.
    pub col_offset: usize,
    /// Height of the text area, remembered from the last frame for paging.
    pub view_height: usize,
    /// Where the linewise selection started, in Visual mode.
    pub visual_anchor: Option<usize>,
    /// The last visual selection, which `'<,'>` refers to.
    pub last_selection: Option<LineRange>,
    /// The agent we have asked something, and whatever it has said back.
    pub job: Option<Job>,
    /// The document being read — `:help`, `:messages` — while it is on screen.
    pub reader: Option<Reader>,
    /// The language server, started as the editor opens.
    pub lsp: Option<Lsp>,
    /// Whether the server failed to start, so we say so once rather than trying
    /// again on every keypress.
    pub lsp_error: bool,
    /// How the last jump ended: `None` before the first one, `Some(true)` if it
    /// landed, `Some(false)` if the server failed us. Drives the status icon.
    pub lsp_outcome: Option<bool>,
    /// The lookup the server owes us an answer to, and its retries.
    pub goto: Option<PendingGoto>,
    /// Where we have jumped from, innermost last.
    pub jumps: Vec<Jump>,
    /// Frames drawn since we started, which retries count out in.
    pub tick: usize,
    /// When we started, so the spinner turns on the clock rather than on however
    /// often the event loop happens to wake up.
    pub started: Instant,
    /// Every message we have shown, for `:messages`.
    pub messages: Vec<(String, bool)>,
    /// Set by `:q`, read by the event loop on its way round.
    pub quit: bool,
}

impl App {
    pub fn new(buffer: Buffer) -> Self {
        let message = buffer.is_new.then(|| {
            let path = buffer.path.clone().unwrap_or_default();
            (ViviError::NotFound(path).to_string(), true)
        });
        Self {
            buffer,
            mode: Mode::Normal,
            command: String::new(),
            message,
            row: 0,
            col: 0,
            want_col: 0,
            row_offset: 0,
            col_offset: 0,
            view_height: 1,
            visual_anchor: None,
            last_selection: None,
            job: None,
            reader: None,
            lsp: None,
            lsp_error: false,
            lsp_outcome: None,
            goto: None,
            jumps: Vec::new(),
            tick: 0,
            started: Instant::now(),
            messages: Vec::new(),
            quit: false,
        }
    }

    pub fn run(mut self, terminal: &mut DefaultTerminal) -> Result<(), ViviError> {
        self.warm_up_lsp();
        while !self.quit {
            terminal.draw(|frame| self.draw(frame))?;

            let wait = if self.busy() { BUSY_POLL } else { IDLE_POLL };
            if event::poll(wait)? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        self.on_key(key);
                    }
                }
            }
            self.tick += 1;
            self.drain_agent();
            self.drain_lsp();
            self.poll_file();
            self.log_message();
        }
        Ok(())
    }

    /// Whether anything in the background needs the screen to keep moving.
    /// Only about how fast we poll — not about whether you may start work.
    fn busy(&self) -> bool {
        self.agent_running() || self.goto.is_some() || self.lsp_activity().is_some()
    }

    /// The spinner frame for right now.
    pub fn spinner(&self) -> &'static str {
        let frame = self.started.elapsed().as_millis() / SPINNER_FRAME.as_millis();
        SPINNER[frame as usize % SPINNER.len()]
    }

    // --- keys -------------------------------------------------------------

    pub fn on_key(&mut self, key: KeyEvent) {
        match self.mode {
            Mode::Normal | Mode::Visual => self.on_normal_key(key),
            Mode::Command => self.on_command_key(key),
            Mode::Output => self.on_output_key(key),
        }
    }

    fn on_normal_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let half = (self.view_height / 2).max(1);
        let page = self.view_height.saturating_sub(2).max(1);

        // A pending prefix only survives until the next keypress.
        let pending = std::mem::take(&mut self.command);
        let (pending_g, pending_d, pending_window) =
            (pending == "g", pending == "d", pending == "^W");
        // Informational messages are noise once you have moved on, but an error
        // must survive until something replaces it — otherwise the next
        // keystroke erases the only explanation you were going to get.
        if self.message.as_ref().is_some_and(|(_, is_error)| !is_error) {
            self.message = None;
        }

        match key.code {
            KeyCode::Char('d') if ctrl => self.move_rows(half as isize),
            KeyCode::Char('u') if ctrl => self.move_rows(-(half as isize)),
            KeyCode::Char('f') if ctrl => self.move_rows(page as isize),
            KeyCode::Char('b') if ctrl => self.move_rows(-(page as isize)),
            KeyCode::Char('e') if ctrl => self.scroll_lines(1),
            KeyCode::Char('y') if ctrl => self.scroll_lines(-1),
            // Vim's window commands, because the output pane is a split: `Ctrl-W w`
            // moves between them and `Ctrl-W c` closes one. `Ctrl-W Ctrl-W`
            // works too, as it does in vim.
            KeyCode::Char('w') if pending_window => self.switch_window(),
            KeyCode::Char('c') if pending_window => self.close_window(),
            KeyCode::Char('w') if ctrl => self.command = "^W".to_string(),

            // Vim's tag keys: jump into a symbol, and unwind back out.
            // Ctrl-] is the byte 0x1D, which crossterm decodes as Ctrl-'5'
            // (it maps 0x1C..=0x1F onto '4'..='7'). Terminals speaking the
            // kitty keyboard protocol report the ']' we actually pressed, so
            // accept both or the key does nothing on an ordinary terminal.
            KeyCode::Char(']') | KeyCode::Char('5') if ctrl => self.goto_definition(),
            KeyCode::Char('t') if ctrl => self.pop_jump(),
            KeyCode::PageDown => self.move_rows(page as isize),
            KeyCode::PageUp => self.move_rows(-(page as isize)),

            KeyCode::Char('j') | KeyCode::Down | KeyCode::Enter => self.move_rows(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_rows(-1),
            KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace => self.move_cols(-1),
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Char(' ') => self.move_cols(1),

            KeyCode::Char('0') | KeyCode::Home => self.set_col(0),
            KeyCode::Char('^') => self.set_col(first_non_blank(self.buffer.line(self.row))),
            KeyCode::Char('$') | KeyCode::End => self.set_col(usize::MAX),

            KeyCode::Char('w') => self.move_word_forward(),
            KeyCode::Char('b') => self.move_word_backward(),

            KeyCode::Char('G') => self.goto_line(self.buffer.len() - 1),
            KeyCode::Char('g') if pending_g => self.goto_line(0),
            // `]` is AltGr-something on plenty of keyboard layouts, and some
            // terminals swallow Ctrl-] outright, so `gd` is the binding that
            // always works.
            KeyCode::Char('d') if pending_g => self.goto_definition(),
            KeyCode::Char('g') => self.command = "g".to_string(),

            // Deleting is linewise, like selection: `dd` takes the current
            // line, and in a selection one `d` takes the selected lines.
            KeyCode::Char('d') if self.mode == Mode::Visual => {
                let range = self.selection().unwrap_or((self.row, self.row));
                self.mode = Mode::Normal;
                self.visual_anchor = None;
                // The lines `'<,'>` pointed at are gone; a range that silently
                // meant something else would be worse than none.
                self.last_selection = None;
                self.delete_lines(range);
            }
            KeyCode::Char('d') if pending_d => self.delete_lines((self.row, self.row)),
            KeyCode::Char('d') => self.command = "d".to_string(),

            KeyCode::Char(':') => {
                self.mode = Mode::Command;
                self.command.clear();
                // Vim drops the highlight here; we keep it up while you type.
                if let Some(range) = self.selection() {
                    self.last_selection = Some(range);
                    self.command.push_str("'<,'>");
                }
            }

            KeyCode::Char('V') | KeyCode::Char('v') => {
                if self.mode == Mode::Visual {
                    self.leave_visual();
                } else {
                    self.mode = Mode::Visual;
                    self.visual_anchor = Some(self.row);
                }
            }
            KeyCode::Esc => {
                self.message = None;
                self.leave_visual();
            }
            // Anything else is simply not bound. `:help` lists what is.
            _ => {}
        }
    }

    /// The selected line range, inclusive.
    pub fn selection(&self) -> Option<LineRange> {
        let anchor = self.visual_anchor?;
        Some((anchor.min(self.row), anchor.max(self.row)))
    }

    pub fn leave_visual(&mut self) {
        if let Some(range) = self.selection() {
            self.last_selection = Some(range);
        }
        self.mode = Mode::Normal;
        self.visual_anchor = None;
    }

    // --- moving -----------------------------------------------------------

    pub fn move_rows(&mut self, delta: isize) {
        let last = self.buffer.len() - 1;
        let row = self.row as isize + delta;
        self.row = row.clamp(0, last as isize) as usize;
        self.col = self.want_col.min(last_col(self.buffer.line(self.row)));
    }

    pub fn move_cols(&mut self, delta: isize) {
        let last = last_col(self.buffer.line(self.row)) as isize;
        self.col = (self.col as isize + delta).clamp(0, last) as usize;
        self.want_col = self.col;
    }

    pub fn set_col(&mut self, col: usize) {
        self.col = col.min(last_col(self.buffer.line(self.row)));
        self.want_col = if col == usize::MAX { usize::MAX } else { self.col };
    }

    pub fn goto_line(&mut self, row: usize) {
        self.row = row.min(self.buffer.len() - 1);
        self.col = first_non_blank(self.buffer.line(self.row));
        self.want_col = self.col;
    }

    /// Put the cursor exactly here, as far as the buffer allows. What a jump
    /// lands with, where `goto_line` is what a motion uses.
    pub fn place_cursor(&mut self, row: usize, col: usize) {
        (self.row, self.col) = motion::clamp(&self.buffer, row, col);
        self.want_col = self.col;
    }

    /// Scroll the view without moving the cursor, dragging it along if needed.
    pub fn scroll_lines(&mut self, delta: isize) {
        let last = self.buffer.len() - 1;
        self.row_offset = (self.row_offset as isize + delta).clamp(0, last as isize) as usize;
        self.row = self.row.clamp(
            self.row_offset,
            (self.row_offset + self.view_height.saturating_sub(1)).min(last),
        );
        self.col = self.want_col.min(last_col(self.buffer.line(self.row)));
    }

    fn move_word_forward(&mut self) {
        (self.row, self.col) = motion::word_forward(&self.buffer, self.row, self.col);
        self.want_col = self.col;
    }

    fn move_word_backward(&mut self) {
        (self.row, self.col) = motion::word_backward(&self.buffer, self.row, self.col);
        self.want_col = self.col;
    }

    // --- ex commands ------------------------------------------------------

    fn on_command_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.command.clear();
            }
            KeyCode::Backspace => {
                if self.command.pop().is_none() {
                    self.mode = Mode::Normal;
                }
            }
            KeyCode::Enter => {
                let command = std::mem::take(&mut self.command);
                self.mode = Mode::Normal;
                self.run_command(&command);
            }
            KeyCode::Char(c) => self.command.push(c),
            _ => {}
        }
    }

    pub fn run_command(&mut self, input: &str) {
        self.visual_anchor = None;

        let (range, rest) = match self.parse_range(input.trim()) {
            Ok(parsed) => parsed,
            Err(e) => {
                self.fail(e);
                return;
            }
        };
        let (word, args) = match rest.find(char::is_whitespace) {
            Some(i) => (&rest[..i], rest[i..].trim()),
            None => (rest, ""),
        };

        // A bare range is a jump: `:42`, `:$`, `:'>`.
        if word.is_empty() {
            if let Some((_, end)) = range {
                self.goto_line(end);
            }
            return;
        }

        // A trailing `!` forces: `:q!` and `:reload!` discard unwritten
        // deletes. Every other command accepts and ignores it, so muscle
        // memory for `:e!` stays harmless.
        let (word, bang) = match word.strip_suffix('!') {
            Some(word) => (word, true),
            None => (word, false),
        };
        let Some(command) = Command::resolve(word) else {
            self.fail(ViviError::UnknownCommand(word.to_string()));
            return;
        };

        match command.name {
            "quit" => {
                if self.buffer.modified && !bang {
                    self.fail(ViviError::Unsaved(":q! to discard them".into()));
                } else {
                    self.quit = true;
                }
            }
            "edit" => {
                if args.is_empty() {
                    let usage = format!(":{} {}", command.name, command.args);
                    self.fail(ViviError::MissingArgument(usage));
                } else {
                    // With no range, ask about the line the cursor is on.
                    self.ask_agent(range.unwrap_or((self.row, self.row)), args);
                }
            }
            "delete" => self.delete_lines(range.unwrap_or((self.row, self.row))),
            "write" => self.write_file(),
            "definition" => self.goto_definition(),
            "pop" => self.pop_jump(),
            "tag" => self.tag_command(args),
            "jumps" => self.note(jump::describe(&self.jumps)),
            "lsp" => self.lsp_command(),
            "output" => self.open_pane(),
            "reload" => {
                if self.buffer.modified && !bang {
                    self.fail(ViviError::Unsaved(":reload! to discard them".into()));
                } else if self.reload() {
                    self.note(format!("\"{}\" reloaded", self.buffer.name()));
                }
            }
            "messages" => self.show_messages(),
            "help" => self.show_reader("help", cmd::help_lines()),
            // `Command::resolve` only returns names from COMMANDS, so a miss here
            // means the table gained an entry without an arm.
            name => self.fail(ViviError::Unimplemented(name.to_string())),
        }
    }

    /// Parse a leading ex range off a command, against where the cursor is,
    /// how long the buffer is, and what was selected last.
    pub fn parse_range<'a>(
        &self,
        input: &'a str,
    ) -> Result<(Option<LineRange>, &'a str), ViviError> {
        cmd::parse_range(input, self.row, self.buffer.len() - 1, self.last_selection)
    }

    // --- the agent --------------------------------------------------------

    /// Send the given lines, plus the typed prompt, to the configured agent.
    pub fn ask_agent(&mut self, (start, end): LineRange, prompt: &str) {
        if self.agent_running() {
            self.note("an agent is still working");
            return;
        }
        // The agent reads the file from disk and rewrites it there; deletes
        // it cannot see would be lost when its edit is reloaded.
        if self.buffer.modified {
            self.fail(ViviError::Unsaved(":w them first".into()));
            return;
        }

        let agent = match Agent::resolve() {
            Ok(agent) => agent,
            Err(error) => {
                self.mode = Mode::Normal;
                self.fail(error);
                return;
            }
        };

        let context = self.selection_context(start, end);
        let rx = agent.spawn(prompt.to_string(), context);
        self.job = Some(Job {
            rx,
            agent: agent.display.clone(),
            prompt: prompt.to_string(),
            output: Vec::new(),
            running: true,
            failed: false,
            offset: 0,
            follow: true,
            height: 1,
            // Stay in the editor. A spinner and the marked lines say it is
            // working; there is nothing else worth saying.
            open: false,
            range: Some((start, end)),
            shown: 0,
        });
        self.message = None;
    }

    /// The selected code, fenced and labelled so the agent knows where it came from.
    pub fn selection_context(&self, start: usize, end: usize) -> String {
        agent::context(&self.buffer.name(), &self.buffer.lines, (start, end))
    }

    /// Move whatever the agent has produced so far into the output pane.
    pub fn drain_agent(&mut self) {
        let mut finished = false;
        let Some(job) = &mut self.job else { return };
        while let Ok(event) = job.rx.try_recv() {
            match event {
                AgentEvent::Line(line) => job.output.push(line),
                // Streamed text arrives mid-word and mid-line, so it is
                // appended to whatever is there rather than pushed as a line.
                // Consecutive calls to the same tool collapse into one line
                // with a count, rather than a column of identical markers with
                // a blank line between each.
                AgentEvent::Tool(name) => {
                    let marker = format!("· {name}");
                    match job.output.iter_mut().rev().find(|line| !line.is_empty()) {
                        Some(line) if line.as_str() == marker => *line = format!("{marker} ×2"),
                        Some(line) if line.starts_with(&format!("{marker} ×")) => {
                            let count: usize = line
                                .rsplit('×')
                                .next()
                                .and_then(|n| n.parse().ok())
                                .unwrap_or(1);
                            *line = format!("{marker} ×{}", count + 1);
                        }
                        // Text ending in a newline leaves an empty line open;
                        // the marker belongs on it, not below it.
                        _ => match job.output.last_mut() {
                            Some(last) if last.is_empty() => *last = marker,
                            _ => job.output.push(marker),
                        },
                    }
                    // Whatever it says next starts on its own line.
                    if job.output.last().is_some_and(|line| !line.is_empty()) {
                        job.output.push(String::new());
                    }
                }
                AgentEvent::Text(chunk) => {
                    if job.output.is_empty() {
                        job.output.push(String::new());
                    }
                    for (i, part) in chunk.split('\n').enumerate() {
                        if i > 0 {
                            job.output.push(String::new());
                        }
                        job.output.last_mut().expect("just ensured one").push_str(part);
                    }
                }
                AgentEvent::Done(error) => {
                    job.running = false;
                    if let Some(error) = error {
                        job.output.push(String::new());
                        job.failed = true;
                        job.output.push(error.to_string());
                    }
                    finished = true;
                }
            }
        }
        // Follow the tail while output is still arriving.
        if job.running {
            job.follow = true;
        }

        if finished {
            // The agent may well have rewritten the file we're looking at.
            let (agent, failed) = (job.agent.clone(), job.failed);
            let reloaded = self.reload();
            let state = if failed { "failed" } else { "finished" };
            let note = if reloaded { ", file reloaded" } else { "" };
            self.message = Some((format!("{agent} {state}{note} · ^Ww to read"), failed));
        }
    }

    /// Whether an agent is mid-run. Kept separate from `busy`: a language
    /// server building its index must not stop you asking an agent something.
    pub fn agent_running(&self) -> bool {
        self.job.as_ref().is_some_and(|job| job.running)
    }

    /// The agent's name and how its last run ended, once it has finished.
    pub fn agent_outcome(&self) -> Option<(&str, bool)> {
        let job = self.job.as_ref()?;
        (!job.running).then_some((job.agent.as_str(), !job.failed))
    }

    // --- jumping ----------------------------------------------------------

    /// Take the editor to a definition the server resolved.
    pub fn follow(&mut self, target: Location, from: Jump) {
        let utf8 = self.lsp.as_ref().is_some_and(|l| l.encoding == Encoding::Utf8);
        let same_file = self.buffer.path.as_deref() == Some(target.path.as_path());
        if !same_file && !self.open_file(&target.path) {
            return;
        }
        self.jumps.push(from);

        let row = target.line.min(self.buffer.len() - 1);
        let col = from_lsp_character(self.buffer.line(row), target.character, utf8);
        self.place_cursor(row, col);
        self.lsp_outcome = Some(true);
        // Where you landed is already the first thing on the status line; saying
        // it again is just the same words twice.
        self.message = None;
    }

    /// `Ctrl-T`: unwind one level of the jump stack.
    pub fn pop_jump(&mut self) {
        let Some(jump) = self.jumps.pop() else {
            // Not a failure: you simply have not jumped anywhere yet.
            self.note("nothing to go back to");
            return;
        };
        match jump.path {
            Some(path) if Some(&path) != self.buffer.path.as_ref() => {
                if self.open_file(&path) {
                    self.place_cursor(jump.row, jump.col);
                }
            }
            _ => self.place_cursor(jump.row, jump.col),
        }
    }

    /// `:tag <file>[:<line>[:<col>]]` — jump without involving a language
    /// server. The same stack `Ctrl-]` uses, so `Ctrl-T` unwinds either.
    pub fn tag_command(&mut self, args: &str) {
        if args.is_empty() {
            self.fail(ViviError::MissingArgument(":tag <file>:<line>".to_string()));
            return;
        }
        let (path, row, col) = jump::parse_tag(args);
        let from = self.here();
        if self.open_file(&path) {
            self.jumps.push(from);
            self.place_cursor(row, col);
        }
    }

    /// Where we are now, ready to be pushed before jumping away from it.
    pub fn here(&self) -> Jump {
        Jump { path: self.buffer.path.clone(), row: self.row, col: self.col }
    }

    // --- the file ---------------------------------------------------------

    /// Swap the buffer for another file, keeping the language server in sync.
    pub fn open_file(&mut self, path: &Path) -> bool {
        // `Buffer::from_file` opens a missing file as an empty new one, which is
        // right on the command line but wrong for a jump — there is nothing there.
        if !path.is_file() {
            self.fail(ViviError::NotFound(path.to_path_buf()));
            return false;
        }
        match Buffer::from_file(path) {
            Ok(buffer) => {
                self.buffer = buffer;
                self.row = 0;
                self.col = 0;
                self.want_col = 0;
                self.row_offset = 0;
                self.col_offset = 0;
                self.visual_anchor = None;
                self.last_selection = None;
                if let Some(lsp) = &mut self.lsp {
                    lsp.did_open(path, &self.buffer.text());
                }
                true
            }
            Err(e) => {
                self.fail(e);
                false
            }
        }
    }

    /// Re-read the buffer from disk, keeping the cursor where it can still go.
    pub fn reload(&mut self) -> bool {
        let Some(path) = self.buffer.path.clone() else { return false };
        match Buffer::from_file(&path) {
            Ok(fresh) => {
                self.buffer = fresh;
                self.row = self.row.min(self.buffer.len() - 1);
                self.col = self.want_col.min(last_col(self.buffer.line(self.row)));
                true
            }
            Err(e) => {
                self.fail(e);
                false
            }
        }
    }

    /// Re-read the file when something else writes it — an agent, or any other
    /// editor. Polling `stat` rather than watching the inode survives the
    /// write-to-temp-then-rename dance that most tools use to save atomically.
    pub fn poll_file(&mut self) {
        let Some(path) = self.buffer.path.clone() else { return };
        let fresh = file_stamp(&path);
        if fresh.is_none() || fresh == self.buffer.stamp {
            return;
        }
        // Someone else wrote the file while the buffer holds unwritten
        // deletes. Reloading would silently discard them, so hold on and say
        // so — taking the new stamp is what keeps it to once.
        if self.buffer.modified {
            self.buffer.stamp = fresh;
            self.fail(ViviError::ChangedOnDisk);
            return;
        }
        if self.reload() {
            self.note(format!("\"{}\" reloaded", self.buffer.name()));
        }
    }

    /// `:write` — put the buffer's deletes on disk, and say so.
    pub fn write_file(&mut self) {
        match self.buffer.save() {
            Ok(()) => self.note(format!("\"{}\" written", self.buffer.name())),
            Err(error) => self.fail(error),
        }
    }

    // --- panes ------------------------------------------------------------

    /// `Ctrl-W w` — move between the buffer and the output pane.
    pub fn switch_window(&mut self) {
        if self.mode == Mode::Output {
            self.mode = Mode::Normal;
            return;
        }
        if self.reader.is_some() {
            self.mode = Mode::Output;
        } else if let Some(job) = &mut self.job {
            job.open = true;
            self.mode = Mode::Output;
        } else {
            self.note("no output to show");
        }
    }

    /// `Ctrl-W c` — close the pane, keeping its contents for next time.
    pub fn close_window(&mut self) {
        if self.reader.take().is_none() {
            match &mut self.job {
                Some(job) => job.open = false,
                None => self.note("no output to show"),
            }
        }
        self.mode = Mode::Normal;
    }

    /// `:output` — bring the agent's reply back up where you left it.
    pub fn open_pane(&mut self) {
        match &mut self.job {
            Some(job) => {
                job.open = true;
                self.mode = Mode::Output;
            }
            None => self.note("no output to show"),
        }
    }

    /// Put a static list of lines in the scrollable pane.
    pub fn show_reader(&mut self, title: &'static str, lines: Vec<String>) {
        self.reader = Some(Reader::new(title, lines));
        self.mode = Mode::Output;
    }

    fn on_output_key(&mut self, key: KeyEvent) {
        if self.reader.is_some() {
            self.on_reader_key(key);
            return;
        }

        // The window prefix works from inside the pane too, so it is left the
        // same way it was entered. The mode only changes once the second key
        // arrives — leaving early would send that key to the wrong handler.
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let pending_window = std::mem::take(&mut self.command) == "^W";
        match key.code {
            KeyCode::Char('w') if pending_window => {
                self.mode = Mode::Normal;
                return;
            }
            KeyCode::Char('c') if pending_window => {
                self.close_window();
                return;
            }
            KeyCode::Char('w') if ctrl => {
                self.command = "^W".to_string();
                return;
            }
            _ => {}
        }

        let Some(job) = &mut self.job else {
            self.mode = Mode::Normal;
            return;
        };
        // `shown` counts wrapped display lines, which is what `offset` indexes.
        let total = job.shown.max(job.output.len());
        match pane::scroll_key(key, job.height, job.offset, total) {
            Scroll::Close => {
                job.open = false;
                self.mode = Mode::Normal;
            }
            // `Ctrl-W` never reaches the pane: it is taken just above.
            Scroll::Prefix => {}
            Scroll::By(delta) => {
                job.offset = pane::scrolled(job.offset, delta, job.height, total);
                // Scrolling by hand means the user wants to stay put, rather
                // than chase the tail.
                job.follow = delta > 0 && job.offset == pane::last_page(job.height, total);
            }
        }
    }

    fn on_reader_key(&mut self, key: KeyEvent) {
        let Some(reader) = &mut self.reader else { return };
        match pane::scroll_key(key, reader.height, reader.offset, reader.shown) {
            Scroll::Close => {
                self.reader = None;
                self.mode = Mode::Normal;
            }
            Scroll::Prefix => self.command = "^W".to_string(),
            Scroll::By(delta) => {
                reader.offset =
                    pane::scrolled(reader.offset, delta, reader.height, reader.shown);
            }
        }
    }

    // --- the language server ----------------------------------------------

    /// Start the language server as the editor opens, rather than on the first
    /// jump. Indexing a crate from cold takes seconds, and they are better
    /// spent while you are reading than while you are waiting.
    pub fn warm_up_lsp(&mut self) {
        let Some(path) = self.buffer.path.clone() else { return };
        // A file with no server we know of is not a problem to announce; it is
        // only worth a word if you actually ask for a definition.
        if Lsp::command_for(&path).is_err() {
            return;
        }
        self.ensure_lsp();
    }

    /// `Ctrl-]`: ask the server where the symbol under the cursor is defined.
    pub fn goto_definition(&mut self) {
        let symbol = symbol_at(self.buffer.line(self.row), self.col);
        if symbol.is_empty() {
            self.note("no symbol under the cursor");
            return;
        }
        if !self.ensure_lsp() {
            return;
        }
        let Some(path) = self.buffer.path.clone() else { return };
        let utf8 = self.lsp.as_ref().is_some_and(|l| l.encoding == Encoding::Utf8);
        let character = to_lsp_character(self.buffer.line(self.row), self.col, utf8);
        let (row, col) = (self.row, self.col);

        self.lsp_outcome = None;
        let lsp = self.lsp.as_mut().expect("ensure_lsp succeeded");
        let id = lsp.definition(&path, row, character);
        // No commentary: the indicator on the right says something is happening,
        // and a failure will name the symbol itself.
        self.goto = Some(PendingGoto::new(id, self.tick, row, col, symbol));
    }

    /// Start the language server for this file, once, on first use.
    fn ensure_lsp(&mut self) -> bool {
        if self.lsp.is_some() {
            return true;
        }
        if self.lsp_error {
            // Already filed the reason; don't respawn it on every keypress, but
            // don't fail silently either.
            self.fail(ViviError::LspUnavailable);
            return false;
        }
        let Some(path) = self.buffer.path.clone() else {
            self.fail(ViviError::NoFileForLsp);
            return false;
        };
        match Lsp::start(&path) {
            Ok(mut lsp) => {
                lsp.did_open(&path, &self.buffer.text());
                // The indicator on the right is the whole announcement.
                self.lsp = Some(lsp);
                true
            }
            Err(e) => {
                self.lsp_error = true;
                self.lsp_outcome = Some(false);
                self.file_error(e);
                false
            }
        }
    }

    /// Handle whatever the language server has said, and retry a `Ctrl-]` that
    /// came back empty because the index was not built yet.
    pub fn drain_lsp(&mut self) {
        let Some(lsp) = &mut self.lsp else { return };
        let events = lsp.poll();

        for event in events {
            match event {
                LspEvent::Ready => {}
                // Indexing just finished: whatever we asked for is answerable
                // now, so retry at once instead of waiting out the delay.
                LspEvent::Indexing(false) => {
                    if let Some(pending) = &mut self.goto {
                        pending.retry_at = self.tick;
                    }
                }
                LspEvent::Indexing(true) => {}
                LspEvent::Error(detail) => {
                    if let Some(pending) = self.goto.take() {
                        let server = self.lsp.as_ref().map(|l| l.name.clone()).unwrap_or_default();
                        self.fail(ViviError::Refused { server, symbol: pending.symbol, detail });
                    }
                }
                LspEvent::Definition { id, target } => {
                    let Some(pending) = &self.goto else { continue };
                    if pending.id != id {
                        continue; // A stale reply from a jump we already gave up on.
                    }
                    match target {
                        Some(target) => {
                            let pending = self.goto.take().expect("checked just above");
                            let from = Jump {
                                path: self.buffer.path.clone(),
                                row: pending.row,
                                col: pending.col,
                            };
                            self.follow(target, from);
                        }
                        // Empty is ambiguous: no such symbol, or not indexed yet.
                        None => {
                            let pending = self.goto.as_mut().expect("checked just above");
                            if pending.attempts >= GOTO_ATTEMPTS {
                                let pending = self.goto.take().expect("checked just above");
                                self.goto_failed(&pending.symbol, pending.attempts);
                            } else {
                                pending.retry_at = self.tick + goto::GOTO_RETRY_TICKS;
                            }
                        }
                    }
                }
            }
        }

        // Re-ask once the retry delay has passed.
        let Some(pending) = &self.goto else { return };
        if self.tick < pending.retry_at {
            return;
        }
        // A server that died mid-lookup would otherwise leave us retrying into
        // a closed pipe until the attempt count ran out.
        if self.lsp.as_mut().is_some_and(|l| l.exited().is_some()) {
            let pending = self.goto.take().expect("checked just above");
            self.goto_failed(&pending.symbol, pending.attempts);
            return;
        }
        let Some(pending) = &self.goto else { return };
        let (row, col) = (pending.row, pending.col);
        let Some(path) = self.buffer.path.clone() else { return };
        let utf8 = self.lsp.as_ref().is_some_and(|l| l.encoding == Encoding::Utf8);
        let character = to_lsp_character(self.buffer.line(row), col, utf8);
        let lsp = self.lsp.as_mut().expect("checked at the top");
        let id = lsp.definition(&path, row, character);
        let pending = self.goto.take().expect("checked just above");
        self.goto = Some(pending.again(id, self.tick));
    }

    /// Explain a failed `Ctrl-]`, and file the explanation wherever it fits.
    pub fn goto_failed(&mut self, symbol: &str, attempts: u32) {
        let Some(lsp) = &mut self.lsp else { return };
        let (reason, long) = goto::failure(lsp, symbol, attempts);
        if matches!(reason, ViviError::ServerExited { .. }) {
            self.lsp = None; // Let the next jump start a fresh one.
        }

        self.lsp_outcome = Some(false);
        if long {
            self.file_error(reason);
        } else {
            self.fail(reason);
        }
    }

    /// Whether the language server is busy on our behalf — warming up, building
    /// its index, or answering a jump.
    ///
    /// The label names what the server gives you rather than the machinery
    /// that gives it — see `LSP_LABEL`.
    pub fn lsp_activity(&self) -> Option<&'static str> {
        let lsp = self.lsp.as_ref()?;
        // A server that failed is not busy, however unfinished it looks: one
        // that never answers `initialize` would otherwise spin for ever.
        if self.lsp_outcome == Some(false) {
            return None;
        }
        (!lsp.ready || lsp.indexing || self.goto.is_some()).then_some(LSP_LABEL)
    }

    /// How things stand once the server is idle: ready, or failed.
    pub fn lsp_outcome(&self) -> Option<bool> {
        if self.lsp_outcome == Some(false) {
            return Some(false);
        }
        if self.lsp_activity().is_some() {
            return None;
        }
        self.lsp.as_ref().filter(|lsp| lsp.ready).map(|_| true)
    }

    /// `:lsp` — what the language server is up to, for when `Ctrl-]` misbehaves.
    pub fn lsp_command(&mut self) {
        let Some(path) = self.buffer.path.clone() else {
            self.fail(ViviError::NoFileForLsp);
            return;
        };
        // Before one is running there may be nothing to run at all, which is
        // the only thing `:lsp` ever says in red.
        let started = self.lsp.is_some();
        let text = goto::describe(self.lsp.as_mut(), &path);
        self.message = Some((text, !started && self.lsp_error));
    }

    // --- drawing ----------------------------------------------------------

    pub fn draw(&mut self, frame: &mut Frame) {
        let full = frame.area();
        let input_height = match self.mode {
            Mode::Command => ui::input_height(&self.command, full.width, full.height),
            _ => 0,
        };
        let [main, input, status] = ui::regions(full, input_height);

        // A document you read — `:help`, `:messages` — takes the whole screen.
        // There is no reason to keep the file in view while you read a
        // reference, and half a screen of each is worse than all of one.
        if let Some(reader) = &mut self.reader {
            ui::draw_reader(frame, main, reader);
            ui::draw_status(frame, status, self.spinner(), &self.status());
            return;
        }

        // The agent's reply is different: you are reading what it said *about*
        // the code, so both stay on screen.
        let (body, pane) = match self.job.as_ref().filter(|job| job.open) {
            Some(_) => {
                let [body, pane] = ui::split(main);
                (body, Some(pane))
            }
            None => (main, None),
        };

        self.scroll_into_view(body);
        // While an agent works, keep the lines it was handed marked, so you can
        // see what it has.
        let working = self.job.as_ref().filter(|job| job.running).and_then(|job| job.range);
        let offset = (self.row_offset, self.col_offset);
        ui::draw_text(frame, body, &self.buffer, offset, self.selection(), working);

        if let (Some(pane), Some(job)) = (pane, self.job.as_mut()) {
            ui::draw_job(frame, pane, job);
        }
        if input_height > 0 {
            ui::draw_input(frame, input, &self.command);
        }
        ui::draw_status(frame, status, self.spinner(), &self.status());

        if matches!(self.mode, Mode::Normal | Mode::Visual) && input_height == 0 {
            let col = self.cursor_display_col() - self.col_offset;
            ui::place_cursor(frame, body, col, self.row - self.row_offset);
        }
    }

    /// What the status line has to say right now.
    fn status(&self) -> ui::Status {
        let mut indicators = Vec::new();
        if let Some(label) = self.lsp_activity() {
            indicators.push(ui::Indicator { label: label.to_string(), outcome: None });
        } else if let Some(ok) = self.lsp_outcome() {
            indicators.push(ui::Indicator { label: LSP_LABEL.to_string(), outcome: Some(ok) });
        }
        // The agent's own name, not the word "agent": `claude` says more than
        // `agent running` does, in fewer columns.
        if let Some(job) = self.job.as_ref().filter(|job| job.running) {
            indicators.push(ui::Indicator { label: job.agent.clone(), outcome: None });
        } else if let Some((agent, ok)) = self.agent_outcome() {
            indicators.push(ui::Indicator { label: agent.to_string(), outcome: Some(ok) });
        }
        ui::Status {
            name: self.buffer.name(),
            row: self.row,
            modified: self.buffer.modified,
            selection: self.selection(),
            message: self.message.clone(),
            indicators,
        }
    }

    /// Keep the cursor inside the viewport, and remember how tall it is.
    pub fn scroll_into_view(&mut self, area: Rect) {
        self.view_height = ui::view_height(area);
        let cursor = (self.row, self.cursor_display_col());
        (self.row_offset, self.col_offset) =
            ui::scroll_into_view(area, cursor, (self.row_offset, self.col_offset));
    }

    /// Screen column of the cursor, with tabs expanded.
    pub fn cursor_display_col(&self) -> usize {
        let line = self.buffer.line(self.row);
        display_width(&line[..byte_index(line, self.col)])
    }

    // --- what the editor says ---------------------------------------------

    /// Say that something went wrong. An error is red, and survives the next
    /// keystroke where a note does not.
    pub fn fail(&mut self, error: ViviError) {
        self.message = Some((error.to_string(), true));
    }

    /// Say something that is merely worth knowing.
    pub fn note(&mut self, text: impl Into<String>) {
        self.message = Some((text.into(), false));
    }

    /// File an error in `:messages` without putting it on the status line, for
    /// when the explanation is longer than one line has room for.
    pub fn file_error(&mut self, error: ViviError) {
        self.messages.push((error.to_string(), true));
        self.message = None;
    }

    /// Keep a copy of anything we displayed. Messages that arrive while the
    /// screen is busy — a jump failing three seconds after the keypress — are
    /// otherwise gone the moment the next one lands.
    pub fn log_message(&mut self) {
        let Some(current) = &self.message else { return };
        if self.messages.last() == Some(current) {
            return;
        }
        self.messages.push(current.clone());
        if self.messages.len() > 200 {
            self.messages.remove(0);
        }
    }

    /// `:messages` — everything we have said, in the scrollable pane. Errors
    /// that arrived while you were typing are recoverable here.
    pub fn show_messages(&mut self) {
        self.log_message();
        let output: Vec<String> = if self.messages.is_empty() {
            vec!["(no messages yet)".to_string()]
        } else {
            self.messages
                .iter()
                .map(|(text, is_error)| format!("{} {text}", if *is_error { "E" } else { " " }))
                .collect()
        };
        self.show_reader("messages", output);
    }
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::editor::harness::{app, press, temp_file};

    /// A job wired to a channel, as a streaming agent run looks from inside.
    fn streaming_job() -> (std::sync::mpsc::Sender<AgentEvent>, App) {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = app("code");
        app.job = Some(Job {
            rx,
            agent: "claude".into(),
            prompt: "explain".into(),
            output: Vec::new(),
            running: true,
            failed: false,
            offset: 0,
            follow: true,
            height: 8,
            open: true,
            range: None,
            shown: 0,
        });
        (tx, app)
    }

    #[test]
    fn streamed_chunks_are_assembled_into_lines() {
        let (tx, mut app) = streaming_job();

        // Chunks arrive mid-word and mid-line, exactly as the event stream
        // sends them: a word split in two, then a newline inside a chunk.
        for chunk in ["Here is ", "wha", "t it does:\nit ", "adds", " two numbers\n"] {
            tx.send(AgentEvent::Text(chunk.into())).unwrap();
        }
        app.drain_agent();
        assert_eq!(
            app.job.as_ref().unwrap().output,
            ["Here is what it does:", "it adds two numbers", ""],
            "split words are rejoined and newlines start new lines"
        );

        // And it keeps assembling across separate drains, not just within one.
        tx.send(AgentEvent::Text("a tail".into())).unwrap();
        app.drain_agent();
        assert_eq!(app.job.as_ref().unwrap().output.last().unwrap(), "a tail");
    }

    #[test]
    fn repeated_tool_calls_collapse_instead_of_stacking_up() {
        let (tx, mut app) = streaming_job();

        // What a tool-heavy run really looks like: a burst of the same tool,
        // some commentary, then another burst.
        tx.send(AgentEvent::Text("Reading the files.\n".into())).unwrap();
        for _ in 0..5 {
            tx.send(AgentEvent::Tool("Bash".into())).unwrap();
        }
        tx.send(AgentEvent::Text("Now the big one:\n".into())).unwrap();
        tx.send(AgentEvent::Tool("Read".into())).unwrap();
        tx.send(AgentEvent::Tool("Read".into())).unwrap();
        app.drain_agent();

        assert_eq!(
            app.job.as_ref().unwrap().output,
            ["Reading the files.", "· Bash ×5", "Now the big one:", "· Read ×2", ""],
            "one line per run of a tool, and no blank line between them"
        );
    }

    #[test]
    fn the_language_server_is_named_in_words_editors_already_use() {
        // Whatever the label is, it must be the same word in every state — the
        // indicator is a single slot whose mark changes, not three messages.
        let mut app = app("fn main() {}");
        app.lsp_outcome = Some(false);
        assert_eq!(app.lsp_outcome(), Some(false));

        // And it must not be protocol jargon or a question begging to be asked.
        for jargon in ["lsp", "LSP", "index", "server", "analyzer"] {
            assert_ne!(LSP_LABEL, jargon, "{jargon} names the machinery, not the feature");
        }
        assert_eq!(LSP_LABEL, "symbols", "the word editors already use for this");
    }

    #[test]
    fn the_spinner_turns_on_the_clock_not_on_the_event_loop() {
        let mut app = app("hello");
        // Backdate the start to walk it through a full revolution.
        let frames: Vec<&str> = (0..SPINNER.len() * 2)
            .map(|i| {
                app.started = Instant::now() - SPINNER_FRAME * i as u32;
                app.spinner()
            })
            .collect();

        // Every frame appears, in order, and it wraps round.
        assert_eq!(frames[..SPINNER.len()], SPINNER[..]);
        assert_eq!(frames[SPINNER.len()..], SPINNER[..], "and it comes round again");

        // Ticking the event loop without time passing must not advance it.
        app.started = Instant::now();
        let before = app.spinner();
        app.tick += 100;
        assert_eq!(app.spinner(), before, "the loop rate does not drive the spinner");
    }

    #[test]
    fn nothing_to_do_is_not_an_error() {
        // Red is for things that went wrong, not for keys that had nothing to
        // act on. These three are the "you have not done that yet" cases.
        let mut app = app("one\ntwo");

        app.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert_eq!(app.message.clone().unwrap(), ("nothing to go back to".into(), false));

        app.run_command("output");
        assert_eq!(app.message.clone().unwrap(), ("no output to show".into(), false));

        app.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(app.message.clone().unwrap(), ("no output to show".into(), false));
    }

    #[test]
    fn messages_keeps_a_readable_history() {
        let mut app = app("hello");

        app.message = Some(("first thing".to_string(), false));
        app.log_message();
        app.message = Some(("it broke".to_string(), true));
        app.log_message();
        app.message = Some(("it broke".to_string(), true));
        app.log_message();
        assert_eq!(app.messages.len(), 2, "identical consecutive messages collapse");

        app.run_command("messages");
        let reader = app.reader.as_ref().expect("the reader opens");
        assert!(app.mode == Mode::Output);
        assert_eq!(reader.title, "messages", "and it is not an agent job");
        assert_eq!(reader.lines, ["  first thing", "E it broke"], "errors are marked");

        // And it is scrollable and closable like any other pane.
        press(&mut app, KeyCode::Char('q'));
        assert!(app.mode == Mode::Normal);
    }

    #[test]
    fn opening_a_file_says_nothing_but_a_missing_one_is_an_error() {
        let path = temp_file("startup", "one\ntwo\nthree\n");
        let app = App::new(Buffer::from_file(&path).unwrap());
        assert!(app.message.is_none(), "a file that opened fine is unremarkable");

        let missing = std::env::temp_dir().join("vivi-definitely-not-here.txt");
        let _ = std::fs::remove_file(&missing);
        let mut app = App::new(Buffer::from_file(&missing).unwrap());
        let (text, is_error) = app.message.clone().expect("a missing file must say so");
        assert!(is_error && text.contains("can't find file"), "{text}");

        // And being an error, it survives the first keystroke — the whole point,
        // since otherwise a typo'd filename looks like an empty file.
        press(&mut app, KeyCode::Char('j'));
        assert!(app.message.is_some(), "the warning must not vanish on a keypress");
    }
}
