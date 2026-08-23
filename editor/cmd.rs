//! The ex command line: the table of what you may type after `:`, the ranges
//! that may come before it, and the help both of them generate.

use crate::{
    buffer::LineRange,
    editor::keys::BINDINGS,
    error::ViviError,
};

/// One ex command. The table below is the only place the command set is
/// defined: dispatch, `:help` and `MANUAL.md` are all checked against it, so
/// they cannot drift apart.
pub struct Command {
    /// The full name, as `:help` lists it and as dispatch matches it.
    pub name: &'static str,
    /// The short form, or "" when the name is already short.
    pub alias: &'static str,
    /// What may follow the name, spelled the way `:help` shows it.
    pub args: &'static str,
    /// One line saying what it does.
    pub help: &'static str,
}

pub const COMMANDS: [Command; 11] = [
    Command { name: "quit", alias: "q", args: "", help: "quit" },
    Command {
        name: "edit",
        alias: "e",
        args: "<prompt>",
        help: "have the agent edit the range, defaulting to the current line",
    },
    Command {
        name: "definition",
        alias: "def",
        args: "",
        help: "jump to the definition under the cursor, as `gd` does",
    },
    Command { name: "pop", alias: "po", args: "", help: "unwind one jump, as `Ctrl-T` does" },
    Command {
        name: "tag",
        alias: "ta",
        args: "<file>[:<line>[:<col>]]",
        help: "jump straight to a place, without a language server",
    },
    Command { name: "jumps", alias: "", args: "", help: "how deep the tag stack is" },
    Command { name: "lsp", alias: "", args: "", help: "what the language server is doing" },
    Command { name: "output", alias: "out", args: "", help: "open the agent's reply" },
    Command { name: "reload", alias: "", args: "", help: "force a re-read from disk" },
    Command {
        name: "messages",
        alias: "mes",
        args: "",
        help: "every message so far, errors included",
    },
    Command { name: "help", alias: "h", args: "", help: "this list" },
];

impl Command {
    /// The command a typed word means, by full name or by alias.
    pub fn resolve(word: &str) -> Option<&'static Command> {
        COMMANDS.iter().find(|c| c.name == word || (!c.alias.is_empty() && c.alias == word))
    }

    /// `:edit` / `:e <prompt>`, as the manual and help spell it.
    pub fn spelling(&self) -> String {
        let mut out = format!(":{}", self.name);
        if !self.alias.is_empty() {
            out.push_str(&format!(" / :{}", self.alias));
        }
        if !self.args.is_empty() {
            out.push(' ');
            out.push_str(self.args);
        }
        out
    }
}

/// Parse a leading ex range — `'<,'>`, `%`, `3,7`, `$`, `.` — off a command,
/// against the cursor's line, the buffer's last line, and the last selection.
pub fn parse_range(
    input: &str,
    row: usize,
    last: usize,
    selection: Option<LineRange>,
) -> Result<(Option<LineRange>, &str), ViviError> {
    if let Some(rest) = input.strip_prefix('%') {
        return Ok((Some((0, last)), rest.trim_start()));
    }
    let (Some(first), rest) = address(input, row, last, selection) else {
        return Ok((None, input));
    };
    let Some(rest) = rest.strip_prefix(',') else {
        return Ok((Some((first, first)), rest.trim_start()));
    };
    let (Some(second), rest) = address(rest, row, last, selection) else {
        return Err(ViviError::InvalidRange);
    };
    Ok((Some((first.min(second), first.max(second))), rest.trim_start()))
}

/// One address of a range, returning the line and whatever follows it.
fn address(
    input: &str,
    row: usize,
    last: usize,
    selection: Option<LineRange>,
) -> (Option<usize>, &str) {
    for (mark, line) in [
        ("'<", selection.map(|r| r.0)),
        ("'>", selection.map(|r| r.1)),
        (".", Some(row)),
        ("$", Some(last)),
    ] {
        if let Some(rest) = input.strip_prefix(mark) {
            return (line.map(|l| l.min(last)), rest);
        }
    }
    let digits = input.len() - input.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return (None, input);
    }
    let line = input[..digits].parse::<usize>().unwrap_or(1).saturating_sub(1);
    (Some(line.min(last)), &input[digits..])
}

/// `:help` — keys and commands, straight out of the tables that define them,
/// so it cannot describe a binding that does not exist.
pub fn help_lines() -> Vec<String> {
    let mut lines = Vec::new();
    let mut group = "";
    for binding in &BINDINGS {
        if binding.group != group {
            group = binding.group;
            lines.push(String::new());
            lines.push(group.to_string());
        }
        lines.push(format!("  {:<20} {}", binding.keys, binding.help));
    }
    lines.push(String::new());
    lines.push("commands".to_string());
    for command in &COMMANDS {
        lines.push(format!("  {:<20} {}", command.spelling(), command.help));
    }
    lines.push(String::new());
    lines.push("  ranges: :42  :$  :%  :10,20  :'<,'>  (a bare range jumps)".to_string());
    lines
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyCode;

    use super::*;
    use crate::{
        buffer::Buffer,
        editor::{
            app::Mode,
            harness::{app, press, tag_fixture, temp_file},
            App,
        },
    };

    #[test]
    fn ranges_cover_marks_numbers_and_the_whole_file() {
        let mut app = app("a\nb\nc\nd\ne");
        app.row = 2;
        app.last_selection = Some((1, 3));

        for (input, expected) in [
            ("'<,'>p go", (Some((1, 3)), "p go")),
            ("%p go", (Some((0, 4)), "p go")),
            ("2,4p go", (Some((1, 3)), "p go")),
            (".p go", (Some((2, 2)), "p go")),
            ("$p go", (Some((4, 4)), "p go")),
            ("p go", (None, "p go")), // a bare command has no range
            ("3", (Some((2, 2)), "")),
            ("4,2p go", (Some((1, 3)), "p go")), // backwards ranges are normalised
            ("99p go", (Some((4, 4)), "p go")),  // and out-of-range lines clamped
        ] {
            assert_eq!(app.parse_range(input).unwrap(), expected, "parsing {input:?}");
        }
    }

    #[test]
    fn visual_mode_prefills_the_range_on_colon() {
        let mut app = app("a\nb\nc\nd");
        press(&mut app, KeyCode::Char('V'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char(':'));
        assert_eq!(app.command, "'<,'>");
        assert_eq!(app.last_selection, Some((0, 1)));
        assert!(app.mode == Mode::Command);
        // The highlight stays up while the command is typed.
        assert_eq!(app.selection(), Some((0, 1)));
    }

    #[test]
    fn commands_quit_and_jump() {
        let mut app = app("a\nb\nc");
        app.run_command("2");
        assert_eq!(app.row, 1);
        app.run_command("nope");
        assert!(app.message.as_ref().is_some_and(|(_, err)| *err));
        assert!(!app.quit);
        app.run_command("q");
        assert!(app.quit);
    }

    #[test]
    fn every_command_in_the_table_dispatches() {
        // A table entry with no arm falls through to "not implemented"; a name
        // that never reaches the table is "not an editor command". Both are
        // silent drift, so walk the whole table and rule them out.
        for command in &COMMANDS {
            for word in [command.name, command.alias].iter().filter(|w| !w.is_empty()) {
                let mut app = app("one\ntwo\nthree");
                app.run_command(word);
                if let Some((text, _)) = &app.message {
                    assert!(!text.contains("not implemented"), ":{word} has no arm");
                    assert!(!text.contains("not an editor command"), ":{word} does not resolve");
                }
            }
        }
    }

    #[test]
    fn removed_commands_are_gone() {
        for word in ["keys", "ask", "prompt", "p", "qa", "tags", "o"] {
            let mut app = app("hello");
            app.run_command(word);
            let (text, is_error) = app.message.clone().unwrap();
            assert!(is_error && text.contains("not an editor command"), ":{word} still exists");
            assert!(!app.quit, ":{word} must not quit");
        }
    }

    #[test]
    fn commands_accept_a_redundant_bang() {
        for word in ["q", "q!", "quit", "quit!"] {
            let mut app = app("hello");
            app.run_command(word);
            assert!(app.quit, ":{word} must quit");
        }
    }

    #[test]
    fn edit_and_its_alias_are_the_same_command() {
        for word in ["e", "edit"] {
            let mut app = app("one\ntwo");
            app.run_command(word);
            let (text, is_error) = app.message.clone().unwrap();
            assert!(is_error && text.contains("argument required"), ":{word} -> {text}");
        }
        // The full name is the one named in its own error message.
        let mut app = app("one");
        app.run_command("e");
        assert!(app.message.unwrap().0.contains(":edit <prompt>"));
    }

    #[test]
    fn edit_without_a_prompt_is_an_error() {
        let mut app = app("a\nb");
        app.run_command("edit");
        let (text, is_error) = app.message.clone().unwrap();
        assert!(is_error && text.contains("argument required"), "{text}");
        assert!(app.job.is_none());
    }

    #[test]
    fn edit_asks_the_agent_and_reload_re_reads() {
        // `:edit` is the agent; re-reading the file moved to `:reload`, so the
        // two cannot be confused with each other.
        let path = temp_file("edit-vs-reload", "before\n");
        let mut app = App::new(Buffer::from_file(&path).unwrap());
        std::fs::write(&path, "after\n").unwrap();

        app.run_command("reload");
        assert_eq!(app.buffer.line(0), "after", ":reload re-reads");

        app.run_command("edit");
        assert!(
            app.message.clone().unwrap().0.contains("argument required"),
            ":edit wants a prompt"
        );
    }

    #[test]
    fn def_command_is_a_typable_fallback() {
        let (caller, _) = tag_fixture("def-cmd");
        let mut app = App::new(Buffer::from_file(&caller).unwrap());
        app.place_cursor(1, 9);
        app.run_command("def");
        let (text, is_error) = app.message.clone().expect(":def must reach goto_definition");
        assert!(!is_error && text.contains("no symbol"), "{text}");
    }

    #[test]
    fn help_is_generated_from_the_tables() {
        let mut app = app("hello");
        app.run_command("help");
        assert!(app.mode == Mode::Output);
        let help = app.reader.as_ref().unwrap().lines.join("\n");

        for command in &COMMANDS {
            assert!(help.contains(&command.spelling()), "help omits {}", command.spelling());
        }
        for binding in &BINDINGS {
            assert!(help.contains(binding.keys), "help omits {}", binding.keys);
        }
        press(&mut app, KeyCode::Char('q'));
        assert!(app.mode == Mode::Normal);
    }

    #[test]
    fn the_manual_documents_every_command_and_key() {
        let manual =
            std::fs::read_to_string("MANUAL.md").expect("MANUAL.md must exist beside the source");
        for command in &COMMANDS {
            assert!(
                manual.contains(&format!(":{}", command.name)),
                "MANUAL.md does not document :{}",
                command.name
            );
            if !command.alias.is_empty() {
                assert!(
                    manual.contains(&format!(":{}", command.alias)),
                    "MANUAL.md does not document the alias :{}",
                    command.alias
                );
            }
        }
        // Match the manual's own prose style rather than the table's spacing:
        // check each key that is more than one character, which is every key a
        // reader could not guess.
        for binding in &BINDINGS {
            for key in binding.keys.split_whitespace().filter(|k| k.len() > 1) {
                assert!(manual.contains(key), "MANUAL.md does not document {key}");
            }
        }
        // The three the user must never have to guess at.
        for essential in ["`gd`", "`Ctrl-T`", "`:q`"] {
            assert!(manual.contains(essential), "MANUAL.md must call out {essential}");
        }
        // The project has been renamed once already; keep the docs from drifting
        // back to an old spelling of it.
        let name = env!("CARGO_PKG_NAME");
        assert!(manual.contains(name), "MANUAL.md does not name the program");
        for doc in ["MANUAL.md", "README.md"] {
            let text = std::fs::read_to_string(doc).unwrap();
            let stale = text
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|word| word.eq_ignore_ascii_case("vii"));
            assert!(!stale, "{doc} still calls the program vii, not {name}");
        }

        // Documenting an environment variable under the wrong name is worse
        // than not documenting it, so check the real ones appear.
        for variable in [crate::agent::AGENT_ENV, crate::lsp::LSP_ENV] {
            assert!(manual.contains(variable), "MANUAL.md does not document {variable}");
        }
    }
}
