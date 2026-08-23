//! Drawing the editor: the text window, the two panes, the input box and the
//! status line. Nothing here knows about the editor's state — each piece is
//! handed exactly what it puts on the screen, and nothing else.

use ratatui::{
    layout::{Constraint, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::{
    agent::Job,
    buffer::{Buffer, LineRange},
    editor::pane::Reader,
    text::expand_tabs,
};

// No colors of our own: we inherit the terminal's theme, and lean on DIM for
// anything secondary so it reads correctly on both light and dark backgrounds.
pub const DIM: Style = Style::new().add_modifier(Modifier::DIM);

/// Everything the status line says: where you are and what you need to read on
/// the left, then whatever is happening in the background on the right.
pub struct Status {
    pub name: String,
    pub row: usize,
    pub selection: Option<LineRange>,
    pub message: Option<(String, bool)>,
    pub indicators: Vec<Indicator>,
}

/// One background job, named. `None` while it is still working — a spinner
/// turns where the mark will go.
pub struct Indicator {
    pub label: String,
    pub outcome: Option<bool>,
}

/// The three regions, bottom-up: the status line is always the last row and
/// never moves; the input box sits above it and is zero rows tall unless you
/// are typing a command; the buffer gets the rest.
pub fn regions(area: Rect, input_height: u16) -> [Rect; 3] {
    Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(input_height),
        Constraint::Length(1),
    ])
    .areas(area)
}

/// The buffer and the agent's pane, when the pane is open. Unlike a document
/// you read, the agent's reply is about the code, so both stay on screen.
pub fn split(main: Rect) -> [Rect; 2] {
    let pane = (main.height / 2).max(3).min(main.height.saturating_sub(2));
    Layout::vertical([Constraint::Fill(1), Constraint::Length(pane)]).areas(main)
}

/// Rows the input box needs: a rule, the wrapped prompt, a rule.
pub fn input_height(command: &str, width: u16, screen: u16) -> u16 {
    let rows = command.chars().count() / input_width(width) + 1;
    (rows as u16 + 2).clamp(3, (screen / 2).max(3))
}

/// Rows of text the window has room for.
pub fn view_height(area: Rect) -> usize {
    (area.height as usize).max(1)
}

/// Keep the cursor inside the viewport, vim-style: scroll the minimum amount.
/// The cursor's column is a display column, with tabs already expanded.
pub fn scroll_into_view(
    area: Rect,
    (row, col): (usize, usize),
    (mut row_offset, mut col_offset): (usize, usize),
) -> (usize, usize) {
    let height = view_height(area);
    if row < row_offset {
        row_offset = row;
    } else if row >= row_offset + height {
        row_offset = row + 1 - height;
    }

    let width = area.width as usize;
    if col < col_offset {
        col_offset = col;
    } else if width > 0 && col >= col_offset + width {
        col_offset = col + 1 - width;
    }
    (row_offset, col_offset)
}

/// The file itself. `selection` is what is highlighted now; `working` is what
/// an agent was handed, kept marked so you can see what it has.
pub fn draw_text(
    frame: &mut Frame,
    area: Rect,
    buffer: &Buffer,
    (row_offset, col_offset): (usize, usize),
    selection: Option<LineRange>,
    working: Option<LineRange>,
) {
    let mut lines = Vec::with_capacity(area.height as usize);
    for i in 0..area.height as usize {
        let row = row_offset + i;
        if row >= buffer.len() {
            // Past the end of the buffer, just like vim.
            lines.push(Line::from(Span::styled("~", DIM)));
            continue;
        }
        let text = expand_tabs(buffer.line(row));
        let visible: String = text.chars().skip(col_offset).collect();
        // REVERSED rather than a background color, so the highlight picks up
        // whatever colors the terminal is already using. The agent's lines get
        // the same treatment dimmed, so they stay clearly weaker than a live
        // selection and still read on light and dark terminals alike.
        let style = match (selection, working) {
            (Some((start, end)), _) if (start..=end).contains(&row) => {
                Style::new().add_modifier(Modifier::REVERSED)
            }
            (_, Some((start, end))) if (start..=end).contains(&row) => {
                Style::new().add_modifier(Modifier::REVERSED | Modifier::DIM)
            }
            _ => Style::new(),
        };
        lines.push(Line::from(Span::styled(visible, style)));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// A document, full screen, with a dim title so you know what you opened.
pub fn draw_reader(frame: &mut Frame, area: Rect, reader: &mut Reader) {
    let [header, body] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    reader.height = body.height as usize;
    frame.render_widget(Paragraph::new(Line::from(Span::styled(reader.title, DIM))), header);

    let wrapped = wrap(&reader.lines, body.width);
    reader.shown = wrapped.len();
    let lines: Vec<Line> =
        wrapped.into_iter().skip(reader.offset).take(reader.height).map(Line::from).collect();
    frame.render_widget(Paragraph::new(lines), body);
}

/// The agent's reply, under a rule naming the question it answers.
pub fn draw_job(frame: &mut Frame, area: Rect, job: &mut Job) {
    let [header, body] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    job.height = body.height as usize;

    // No state here: the status line already carries `⠧ claude` or `✓ claude`,
    // and saying it twice is just saying it twice. What the header is for is
    // reminding you which question this is the answer to.
    let title = format!("── {} · {} ", job.agent, job.prompt);
    let rule: String =
        title.chars().chain(std::iter::repeat('─')).take(area.width as usize).collect();
    frame.render_widget(Paragraph::new(Line::from(Span::styled(rule, DIM))), header);

    let wrapped = wrap(&job.output, body.width);
    job.shown = wrapped.len();
    if job.follow {
        job.offset = wrapped.len().saturating_sub(job.height);
    }
    let lines: Vec<Line> =
        wrapped.into_iter().skip(job.offset).take(job.height).map(Line::from).collect();
    frame.render_widget(Paragraph::new(lines), body);
}

/// Break lines to the pane's width. Wrapped rather than clipped, because the
/// whole point of `:messages` is reading an error too long for the status line,
/// and a pane that clipped at its own width would be no better. Wrapping by
/// hand — rather than leaving it to `Paragraph` — keeps an offset counting the
/// lines you actually see, so scrolling can still reach the end.
fn wrap(lines: &[String], width: u16) -> Vec<String> {
    let width = (width as usize).max(1);
    lines
        .iter()
        .flat_map(|line| {
            let line = expand_tabs(line);
            if line.is_empty() {
                return vec![String::new()];
            }
            line.chars().collect::<Vec<_>>().chunks(width).map(|c| c.iter().collect()).collect()
        })
        .collect()
}

/// The `:` command line: a `> ` marker between two rules. No box — the rules
/// separate it from the buffer above and the status line below, and nothing
/// else is needed.
pub fn draw_input(frame: &mut Frame, area: Rect, command: &str) {
    let rule = "─".repeat(area.width as usize);
    let [top, body, bottom] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1), Constraint::Length(1)])
            .areas(area);
    for edge in [top, bottom] {
        frame.render_widget(Paragraph::new(Line::from(Span::styled(rule.clone(), DIM))), edge);
    }

    let width = input_width(area.width);
    let typed: Vec<char> = command.chars().collect();

    // Wrap by character, not by word: the cursor has to land on the exact cell
    // you are about to type into, and word wrapping makes that unknowable.
    let chunks: Vec<String> = if typed.is_empty() {
        vec![String::new()]
    } else {
        typed.chunks(width).map(|c| c.iter().collect()).collect()
    };

    // Show the end of a long prompt — that is where you are typing.
    let visible = body.height as usize;
    let skipped = chunks.len().saturating_sub(visible);

    let lines: Vec<Line> = chunks[skipped..]
        .iter()
        .enumerate()
        .map(|(i, chunk)| {
            // Only the first row gets the marker; the rest line up under it.
            let marker = if i == 0 && skipped == 0 { "> " } else { "  " };
            Line::from(vec![Span::styled(marker, DIM), Span::from(chunk.clone())])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), body);

    let cursor = typed.len();
    let row = (cursor / width).saturating_sub(skipped) as u16;
    let column = (cursor % width) as u16;
    frame.set_cursor_position(Position::new(body.x + 2 + column, body.y + row));
}

/// Columns available for text: the whole width, less the two the `> ` marker
/// takes and the wrapped lines stay indented under.
fn input_width(width: u16) -> usize {
    (width as usize).saturating_sub(2).max(1)
}

/// The status line: where you are, then whatever is happening, in fixed order.
/// The filename is never traded away for something transient — losing it was
/// how a message could leave you unsure which file you were even looking at.
pub fn draw_status(frame: &mut Frame, area: Rect, spinner: &str, status: &Status) {
    // Two columns of air at each end, so nothing sits flush against the edge
    // of the terminal.
    let area = Rect { x: area.x + 2, width: area.width.saturating_sub(4), ..area };

    let mut left = vec![Span::styled(format!("{}:{}", status.name, status.row + 1), DIM)];
    let mut push = |text: String, style: Style| {
        left.push(Span::styled("  ", DIM));
        left.push(Span::styled(text, style));
    };
    if let Some((first, last)) = status.selection {
        let count = last - first + 1;
        let lines = if count == 1 { "line" } else { "lines" };
        push(format!("{count} {lines}"), Style::new().add_modifier(Modifier::BOLD));
    }
    if let Some((text, is_error)) = &status.message {
        // Errors use the terminal's own red, not a color of our choosing.
        let style = if *is_error { Style::new().fg(Color::Red) } else { DIM };
        push(text.clone(), style);
    }

    // Background work sits on the right, where it holds still. On the left it
    // would shuffle sideways every time a message came or went. Each slot keeps
    // its label and swaps only the mark before it: a spinner while it turns,
    // then a tick or a cross. The colour carries the state and the mark repeats
    // it, so it still reads with colours off.
    let mut right = Vec::new();
    for indicator in &status.indicators {
        // Two columns before the first slot too, so a long message on the left
        // runs out of room rather than into the indicator.
        right.push(Span::styled("  ", DIM));
        match indicator.outcome {
            None => right.push(Span::styled(format!("{spinner} {}", indicator.label), DIM)),
            Some(ok) => {
                let (glyph, style) = if ok {
                    ("✓", Style::new().fg(Color::Green))
                } else {
                    ("✗", Style::new().fg(Color::Red))
                };
                right.push(Span::styled(glyph, style));
                right.push(Span::styled(format!(" {}", indicator.label), DIM));
            }
        }
    }
    let width: usize = right.iter().map(|s| s.content.chars().count()).sum();

    let [left_area, right_area] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(width as u16)]).areas(area);
    frame.render_widget(Paragraph::new(Line::from(left)), left_area);
    frame.render_widget(Paragraph::new(Line::from(right)), right_area);
}

/// Put the terminal's own cursor where the next character goes.
pub fn place_cursor(frame: &mut Frame, area: Rect, col: usize, row: usize) {
    frame.set_cursor_position(Position::new(area.x + col as u16, area.y + row as u16));
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyCode;

    use super::*;
    use crate::editor::{
        app::{Mode, SPINNER},
        goto::PendingGoto,
        harness::{app, press, render, rows},
        App,
    };

    #[test]
    fn view_follows_the_cursor() {
        let text: Vec<String> = (0..20).map(|i| format!("line {i}")).collect();
        let mut app = app(&text.join("\n"));
        let area = Rect::new(0, 0, 20, 4);

        app.goto_line(10);
        app.scroll_into_view(area);
        assert_eq!(app.row_offset, 7, "scrolls just enough to show the cursor");

        app.goto_line(0);
        app.scroll_into_view(area);
        assert_eq!(app.row_offset, 0);
    }

    #[test]
    fn long_lines_scroll_horizontally() {
        let mut app = app(&"x".repeat(40));
        let area = Rect::new(0, 0, 10, 4);
        app.set_col(usize::MAX);
        app.scroll_into_view(area);
        assert_eq!(app.col_offset, 30);
        assert_eq!(app.cursor_display_col() - app.col_offset, 9);
    }

    #[test]
    fn renders_the_visible_window_with_tildes_past_the_end() {
        let mut app = app("one\ntwo\nthree");
        let buffer = render(&mut app, 30, 6);
        let rows = rows(&buffer);
        assert_eq!(rows[..5], ["one", "two", "three", "~", "~"]);
        // Name and line together, and nothing else: no column, no scroll marker.
        assert_eq!(rows[5], "  [No Name]:1", "a two-column margin: {:?}", rows[5]);
        for noise in ["All", "Top", "Bot", "%"] {
            assert!(!rows[5].contains(noise), "status line should not carry {noise:?}");
        }
    }

    /// The rows between the two rules: the prompt itself.
    fn prompt_rows(screen: &[String]) -> Vec<String> {
        let rule = |r: &String| !r.is_empty() && r.chars().all(|c| c == '─');
        let first = screen.iter().position(rule).expect("a rule above") + 1;
        let last = screen.iter().rposition(rule).expect("a rule below");
        screen[first..last].to_vec()
    }

    #[test]
    fn the_status_line_never_gives_up_the_filename() {
        // Trimmed, so the two-column margin does not have to be spelled out in
        // every expectation.
        let status = |app: &mut App| rows(&render(app, 60, 6)).pop().unwrap().trim().to_string();

        let mut app = app("one\ntwo\nthree\nfour");
        app.goto_line(2);
        assert_eq!(status(&mut app), "[No Name]:3", "where you are, always first");

        // A message is appended, not swapped in for the name.
        app.message = Some(("no definition for foo".to_string(), true));
        let line = status(&mut app);
        assert!(line.starts_with("[No Name]:3"), "the name survives a message: {line:?}");
        assert!(line.contains("no definition for foo"), "{line:?}");

        // So is a selection count, and both together keep their order.
        app.mode = Mode::Visual;
        app.visual_anchor = Some(0);
        let line = status(&mut app);
        assert!(
            line.starts_with("[No Name]:3  3 lines  no definition for foo"),
            "segments keep a fixed order: {line:?}"
        );
    }

    #[test]
    fn the_language_server_indicator_is_one_word() {
        let mut app = app("fn main() {}");
        // A lookup in flight, whatever stage the server is at.
        app.goto = Some(PendingGoto::new(1, 0, 0, 3, "main".into()));
        assert_eq!(app.lsp_activity(), None, "nothing to say without a server");
    }

    #[test]
    fn an_outcome_is_a_coloured_mark_beside_its_label() {
        let mut app = app("one\ntwo");
        app.job = Some(crate::agent::Job {
            rx: std::sync::mpsc::channel().1,
            agent: "claude".into(),
            prompt: "go".into(),
            output: vec!["boom".into()],
            running: false,
            failed: true,
            offset: 0,
            follow: false,
            height: 1,
            open: false,
            range: None,
            shown: 0,
        });

        let buffer = render(&mut app, 60, 6);
        let line = rows(&buffer).pop().unwrap();
        // The mark comes first: it is the thing you scan for.
        assert!(line.contains("✗ claude"), "a mark and a name: {line:?}");

        // The mark is in the terminal's own red, and the label stays dim.
        let column = line.chars().position(|c| c == '✗').unwrap() as u16;
        assert_eq!(buffer[(column, 5)].fg, Color::Red);
        for cell in buffer.content() {
            assert_eq!(cell.bg, Color::Reset, "still no background of our own");
        }

        // A run that worked gets the same treatment in green.
        app.job.as_mut().unwrap().failed = false;
        let buffer = render(&mut app, 60, 6);
        let line = rows(&buffer).pop().unwrap();
        assert!(line.contains("✓ claude"), "{line:?}");
        let column = line.chars().position(|c| c == '✓').unwrap() as u16;
        assert_eq!(buffer[(column, 5)].fg, Color::Green);
    }

    #[test]
    fn a_reader_takes_the_whole_screen_and_carries_no_job_chrome() {
        let mut app = app(&(0..30).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n"));
        app.run_command("help");

        let screen = rows(&render(&mut app, 50, 12));

        // A dim title, then the document. No file behind it, and none of the
        // agent pane's furniture.
        assert_eq!(screen[0], "help", "a title so you know what you opened");
        assert!(screen[1..11].iter().any(|r| r.contains("h j k l")), "the document fills it");
        assert!(!screen.iter().any(|r| r.contains("line 0")), "the buffer is not behind it");
        for chrome in ["──", "done", "thinking", "lines ·"] {
            assert!(!screen[..11].iter().any(|r| r.contains(chrome)), "no job chrome: {chrome}");
        }
        // The status line still holds the last row.
        assert!(screen[11].trim_start().starts_with("[No Name]"), "{:?}", screen[11]);
    }

    #[test]
    fn the_agent_pane_stays_a_split_and_drops_the_duplicated_state() {
        let mut app = app(&(0..30).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n"));
        app.job = Some(crate::agent::Job {
            rx: std::sync::mpsc::channel().1,
            agent: "claude".into(),
            prompt: "make this async".into(),
            output: vec!["a reply".into()],
            running: true,
            failed: false,
            offset: 0,
            follow: true,
            height: 1,
            open: true,
            range: None,
            shown: 0,
        });

        let screen = rows(&render(&mut app, 60, 12));
        assert!(screen.iter().any(|r| r.starts_with("line ")), "the code stays visible");
        let header = screen.iter().find(|r| r.starts_with("──")).expect("a header rule");
        assert!(header.contains("claude") && header.contains("make this async"), "{header:?}");
        // The status line already says whether it is running; not twice.
        for state in ["thinking", "done", "failed"] {
            assert!(!header.contains(state), "state belongs on the status line: {header:?}");
        }
    }

    #[test]
    fn a_long_error_is_readable_in_full_in_the_pane() {
        let mut app = app("one\ntwo\nthree\nfour\nfive\nsix\nseven\neight");
        let complaint = "rust-analyzer exited with status 1: \
error: Unknown binary 'rust-analyzer' in official toolchain \
'nightly-2026-07-26-aarch64-apple-darwin'.";
        app.messages.push((complaint.to_string(), true));
        app.run_command("messages");

        let buffer = render(&mut app, 40, 14);
        let screen = rows(&buffer);
        let pane: String = screen[1..13].concat();

        // Every word of it is on screen, wrapped rather than clipped.
        for fragment in ["Unknown binary", "official toolchain", "nightly-2026-07-26"] {
            assert!(pane.contains(fragment), "{fragment:?} missing from:\n{pane}");
        }
    }

    #[test]
    fn the_index_indicator_tracks_the_servers_whole_life() {
        let mut app = app("fn main() {}");
        assert_eq!(app.lsp_activity(), None, "nothing to show before one starts");
        assert_eq!(app.lsp_outcome(), None);

        // A failure outranks any half-finished state: a server that never
        // answers `initialize` would otherwise spin for ever.
        app.lsp_outcome = Some(false);
        assert_eq!(app.lsp_activity(), None, "not busy, however unfinished it looks");
        assert_eq!(app.lsp_outcome(), Some(false));
    }

    #[test]
    fn indicators_sit_on_the_right_and_hold_still() {
        let mut app = app("one\ntwo\nthree");
        app.job = Some(crate::agent::Job {
            rx: std::sync::mpsc::channel().1,
            agent: "claude".into(),
            prompt: "go".into(),
            output: Vec::new(),
            running: true,
            failed: false,
            offset: 0,
            follow: true,
            height: 1,
            open: false,
            range: None,
            shown: 0,
        });

        let column_of_indicator = |app: &mut App| {
            let line = rows(&render(app, 60, 6)).pop().unwrap();
            line.find("claude").expect("the agent indicator")
        };

        let resting = column_of_indicator(&mut app);
        assert!(resting > 40, "pinned to the right edge, not next to the name: {resting}");

        // A message on the left must not shove the indicator sideways.
        app.message = Some(("a message of some length".to_string(), false));
        assert_eq!(column_of_indicator(&mut app), resting, "the indicator holds its column");

        app.message = Some(("short".to_string(), true));
        assert_eq!(column_of_indicator(&mut app), resting);
    }

    #[test]
    fn a_working_agent_shows_its_name_and_a_spinner() {
        let mut app = app("one\ntwo");
        app.job = Some(crate::agent::Job {
            rx: std::sync::mpsc::channel().1,
            agent: "claude".into(),
            prompt: "go".into(),
            output: Vec::new(),
            running: true,
            failed: false,
            offset: 0,
            follow: true,
            height: 1,
            open: false,
            range: None,
            shown: 0,
        });

        let line = rows(&render(&mut app, 60, 6)).pop().unwrap();
        assert!(line.trim_start().starts_with("[No Name]:1"), "{line:?}");
        assert!(line.contains("claude"), "the agent names itself: {line:?}");
        assert!(SPINNER.iter().any(|s| line.contains(s)), "an indicator: {line:?}");
        assert!(!line.contains("running"), "and no filler word: {line:?}");
    }

    #[test]
    fn one_selected_line_is_singular() {
        let mut app = app("one\ntwo");
        app.mode = Mode::Visual;
        app.visual_anchor = Some(0);
        let line = rows(&render(&mut app, 40, 6)).pop().unwrap();
        assert!(line.contains("1 line") && !line.contains("1 lines"), "{line:?}");
    }

    #[test]
    fn the_input_is_a_marker_between_two_rules() {
        let mut app = app("one\ntwo\nthree\nfour\nfive\nsix");
        app.mode = Mode::Command;
        app.command = "edit hi".to_string();

        let buffer = render(&mut app, 24, 9);
        let screen = rows(&buffer);
        let rule = "─".repeat(24);

        // Bottom-up: status line last, then a rule, the prompt, a rule.
        assert!(screen[8].trim_start().starts_with("[No Name]"), "status last: {:?}", screen[8]);
        assert_eq!(screen[7], rule, "a rule below");
        assert_eq!(screen[6], "> edit hi", "the prompt, with no border around it");
        assert_eq!(screen[5], rule, "a rule above");
        assert_eq!(screen[..5], ["one", "two", "three", "four", "five"], "buffer keeps the rest");

        // No box drawing beyond the two rules.
        for row in &screen {
            for corner in ['╭', '╮', '╰', '╯', '│'] {
                assert!(!row.contains(corner), "no box: {row:?}");
            }
        }
    }

    #[test]
    fn the_status_line_is_never_replaced_by_the_command_line() {
        let mut app = app("one\ntwo\nthree\nfour\nfive");
        let last = |app: &mut App| rows(&render(app, 30, 9)).pop().unwrap();

        let resting = last(&mut app);
        app.mode = Mode::Command;
        app.command = "edit something".to_string();
        let typing = last(&mut app);

        assert_eq!(resting, typing, "the status line is fixed, whatever else is drawn");
        assert!(resting.contains("[No Name]") && resting.ends_with('1'));
    }

    #[test]
    fn a_long_prompt_wraps_inside_the_box_under_the_marker() {
        let mut app = app("one\ntwo\nthree\nfour\nfive\nsix\nseven");
        app.mode = Mode::Command;
        app.command = "x".repeat(45);

        let buffer = render(&mut app, 24, 11);
        let screen = rows(&buffer);

        // 24 wide, less the two-column marker, is 22 per row.
        let text = prompt_rows(&screen);
        assert_eq!(text.len(), 3, "45 characters need three rows: {text:?}");
        assert_eq!(text[0], format!("> {}", "x".repeat(22)));
        assert_eq!(text[1], format!("  {}", "x".repeat(22)), "continuations line up");
        assert_eq!(text[2], "  x", "and the remainder on the last");
        assert_eq!(
            text.concat().replace("> ", "").replace("  ", ""),
            app.command,
            "nothing is lost"
        );
    }

    #[test]
    fn the_cursor_sits_where_the_next_character_goes() {
        // The box grows upward, so the cursor's absolute row moves with it —
        // check it against the marker rather than a fixed row.
        let cursor_relative_to_marker = |command: &str| {
            let mut app = app("one\ntwo\nthree\nfour\nfive");
            app.mode = Mode::Command;
            app.command = command.to_string();
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(24, 9)).unwrap();
            terminal.draw(|frame| app.draw(frame)).unwrap();
            let position = terminal.get_cursor_position().unwrap();
            let marker = rows(terminal.backend().buffer())
                .iter()
                // An empty prompt renders as a bare `>`, since `rows` trims.
                .position(|r| r.starts_with('>'))
                .expect("the marker row") as u16;
            (position.x, position.y as i32 - marker as i32)
        };

        // Empty: just past the marker, on the marker's own row.
        // No border now, so the marker starts at column 0 and text at column 2.
        assert_eq!(cursor_relative_to_marker(""), (2, 0));
        // 21 fits the 22-column row; the 22nd wraps to the next line.
        assert_eq!(cursor_relative_to_marker(&"x".repeat(21)), (23, 0));
        assert_eq!(cursor_relative_to_marker(&"x".repeat(22)), (2, 1));
    }

    #[test]
    fn a_very_long_prompt_stops_at_half_the_screen() {
        let mut app = app(&(0..40).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n"));
        app.mode = Mode::Command;
        app.command = "x".repeat(1000);

        let buffer = render(&mut app, 24, 12);
        let screen = rows(&buffer);
        assert!(screen[4].starts_with("line"), "the buffer never disappears: {:?}", screen[4]);
        assert!(screen[11].contains("[No Name]"), "and the status line holds its row");

        // What you can see is the end of the prompt, where the cursor is.
        let text = prompt_rows(&screen);
        assert!(!text.is_empty() && !text[0].starts_with('>'), "the start scrolled away");
        assert_eq!(text.last().unwrap().trim_start().chars().count(), 1000 % 22);
    }

    #[test]
    fn renders_tabs_expanded() {
        let mut app = app("a\tb");
        let buffer = render(&mut app, 20, 3);
        assert_eq!(rows(&buffer)[0], "a       b");
    }

    #[test]
    fn scrolled_window_shows_the_cursor_line() {
        let text: Vec<String> = (0..30).map(|i| format!("line {i}")).collect();
        let mut app = app(&text.join("\n"));
        app.goto_line(20);
        let buffer = render(&mut app, 20, 5);
        // 4 text rows + status: the cursor line is the last text row.
        assert_eq!(rows(&buffer)[..4], ["line 17", "line 18", "line 19", "line 20"]);
        assert_eq!(buffer.area.height, 5);
    }

    #[test]
    fn nothing_overrides_the_terminal_theme() {
        let mut app = app("hello\nworld");
        app.message = Some(("boom".into(), true));
        let buffer = render(&mut app, 20, 5);
        for cell in buffer.content() {
            assert_eq!(cell.bg, Color::Reset, "never paints a background");
        }
        // The only non-default foreground is the terminal's own red, for errors.
        let fgs: std::collections::HashSet<_> = buffer.content().iter().map(|c| c.fg).collect();
        assert!(
            fgs.iter().all(|fg| matches!(fg, Color::Reset | Color::Red)),
            "unexpected foreground colors: {fgs:?}"
        );
    }

    #[test]
    fn selection_is_highlighted_without_using_color() {
        let mut app = app("one\ntwo\nthree");
        press(&mut app, KeyCode::Char('V'));
        press(&mut app, KeyCode::Char('j'));
        let buffer = render(&mut app, 12, 5);

        let reversed = |y: u16| buffer[(0, y)].modifier.contains(Modifier::REVERSED);
        assert!(reversed(0) && reversed(1), "selected lines are reversed");
        assert!(!reversed(2), "unselected lines are not");
        for cell in buffer.content() {
            assert_eq!(cell.bg, Color::Reset, "highlight must not paint a background");
        }
    }
}
