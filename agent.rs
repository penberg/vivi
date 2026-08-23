//! Driving an agent CLI as a subprocess: which one to run, how to invoke it
//! non-interactively, and how to stream its reply back to the editor.

use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver},
};

use serde_json::Value;

use crate::{buffer::LineRange, error::ViviError, which::find_on_path};

/// The environment variable that chooses the agent.
pub const AGENT_ENV: &str = "VIVI_AGENT";

/// A streaming reply from the agent subprocess.
pub enum AgentEvent {
    /// A whole line, from an agent that speaks plain text.
    Line(String),
    /// A chunk of text with no line structure, from a streaming agent. It may
    /// contain newlines, or be half a word.
    Text(String),
    /// The agent reached for a tool, by name.
    Tool(String),
    /// `Some(error)` if the agent could not be run or exited non-zero.
    Done(Option<ViviError>),
}

/// An in-flight (or finished) agent invocation and its output pane state.
pub struct Job {
    /// Where the reply arrives, a piece at a time, from the reader thread.
    pub rx: Receiver<AgentEvent>,
    /// Which agent we ran, so the status line can name it.
    pub agent: String,
    /// What we asked, kept for the pane's title.
    pub prompt: String,
    /// Everything it has said so far, a line at a time.
    pub output: Vec<String>,
    /// Whether it is still working.
    pub running: bool,
    /// Whether it ended badly, which colours how we report it.
    pub failed: bool,
    /// First visible display line of the output pane.
    pub offset: usize,
    /// Stick to the tail as new output arrives, until the user scrolls away.
    pub follow: bool,
    /// Rows of the output pane on screen, set when it is drawn.
    pub height: usize,
    /// Whether the output pane is on screen.
    pub open: bool,
    /// The lines this job was given, marked on screen while it works.
    /// `None` for panes that are not agent output, like `:help`.
    pub range: Option<(usize, usize)>,
    /// Display lines after wrapping, which is what `offset` counts. Set when
    /// the pane is drawn, since it depends on how wide the pane is.
    pub shown: usize,
}

/// The code a job is asked about, fenced and labelled so the agent knows where
/// it came from.
pub fn context(name: &str, lines: &[String], (start, end): LineRange) -> String {
    let mut out = format!("Here are lines {}-{} of {name}:\n\n```\n", start + 1, end + 1);
    for line in &lines[start..=end] {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("```\n");
    out
}

/// How to drive a particular CLI non-interactively.
#[derive(Debug)]
enum AgentKind {
    /// `claude -p <prompt>`
    Claude,
    /// `codex exec <prompt>`
    Codex,
}

#[derive(Debug)]
pub struct Agent {
    program: PathBuf,
    args: Vec<String>,
    pub display: String,
    kind: AgentKind,
}

impl Agent {
    /// `VIVI_AGENT` wins; otherwise probe for a known CLI on `PATH`.
    pub fn resolve() -> Result<Self, ViviError> {
        if let Some(spec) = std::env::var_os(AGENT_ENV) {
            let spec = spec.to_string_lossy().into_owned();
            let mut words = spec.split_whitespace().map(str::to_string);
            let Some(program) = words.next() else {
                return Err(ViviError::AgentEnvEmpty);
            };
            let Some(path) = find_on_path(&program) else {
                return Err(ViviError::AgentNotOnPath(program));
            };
            return Self::at(path, words.collect());
        }

        for candidate in ["claude", "codex"] {
            if let Some(path) = find_on_path(candidate) {
                return Self::at(path, Vec::new());
            }
        }
        Err(ViviError::NoAgent)
    }

    /// Only CLIs we know how to drive non-interactively are accepted: an
    /// unknown program has no prompt flag we could guess at.
    pub fn at(program: PathBuf, args: Vec<String>) -> Result<Self, ViviError> {
        let display = program
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| program.display().to_string());
        let kind = match display.as_str() {
            "claude" => AgentKind::Claude,
            "codex" => AgentKind::Codex,
            other => return Err(ViviError::UnsupportedAgent(other.to_string())),
        };
        Ok(Self { program, args, display, kind })
    }

    /// Run the agent on a background thread, streaming stdout back line by line.
    pub fn spawn(&self, prompt: String, context: String) -> Receiver<AgentEvent> {
        let (tx, rx) = mpsc::channel();
        let streaming = matches!(self.kind, AgentKind::Claude);
        let mut command = self.command(&prompt);
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());

        std::thread::spawn(move || {
            let mut child = match command.spawn() {
                Ok(child) => child,
                Err(e) => {
                    let _ = tx.send(AgentEvent::Done(Some(ViviError::AgentStartFailed(e))));
                    return;
                }
            };

            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(context.as_bytes());
                // Dropping stdin signals EOF, without which the agent waits forever.
            }

            // Drain stderr on its own thread so a chatty agent can't fill the pipe
            // buffer and deadlock while we're reading stdout.
            let stderr = child.stderr.take().map(|stderr| {
                std::thread::spawn(move || {
                    BufReader::new(stderr).lines().map_while(Result::ok).collect::<Vec<_>>()
                })
            });

            if let Some(stdout) = child.stdout.take() {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    let event = match streaming {
                        // A stream-json line we cannot read is not worth showing;
                        // most of them are bookkeeping we do not care about.
                        true => match stream_event(&line) {
                            Some(event) => event,
                            None => continue,
                        },
                        false => AgentEvent::Line(line),
                    };
                    if tx.send(event).is_err() {
                        return; // The pane was closed; stop pumping.
                    }
                }
            }

            let status = child.wait();
            let errors = stderr.and_then(|h| h.join().ok()).unwrap_or_default();
            let failure = match status {
                Ok(status) if status.success() => None,
                Ok(status) => Some(ViviError::AgentExited {
                    code: status.code(),
                    detail: errors.last().cloned(),
                }),
                Err(e) => Some(ViviError::AgentFailed(e)),
            };
            let _ = tx.send(AgentEvent::Done(failure));
        });

        rx
    }

    fn command(&self, prompt: &str) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args);
        match self.kind {
            // Both CLIs take the prompt as an argument and append piped stdin,
            // which is where the selected code goes.
            //
            // `-p` alone prints the finished answer and nothing before it, so
            // the pane would sit empty for however long the agent takes. The
            // JSON event stream is the only way to watch it work.
            AgentKind::Claude => cmd
                .arg("-p")
                .arg("--output-format=stream-json")
                .arg("--include-partial-messages")
                .arg("--verbose")
                .arg(prompt),
            AgentKind::Codex => cmd.arg("exec").arg(prompt),
        };
        cmd
    }
}

/// Pull the readable part out of one `stream-json` line, or `None` if the line
/// is bookkeeping. These shapes were taken from a real `claude -p` session:
///
/// ```text
/// {"type":"stream_event","event":{"type":"content_block_delta",
///   "delta":{"type":"text_delta","text":"hello"}}}
/// ```
fn stream_event(line: &str) -> Option<AgentEvent> {
    let value: Value = serde_json::from_str(line).ok()?;

    // A failed run reports itself in the last object rather than on stderr.
    if value.get("is_error").and_then(Value::as_bool) == Some(true) {
        let text = value
            .get("result")
            .and_then(Value::as_str)
            .unwrap_or("the agent reported an error");
        return Some(AgentEvent::Text(format!("\n{text}\n")));
    }

    let event = value.get("event")?;
    match event.get("type")?.as_str()? {
        // The answer itself, a few characters at a time.
        "content_block_delta" => {
            let delta = event.get("delta")?;
            match delta.get("type")?.as_str()? {
                "text_delta" => Some(AgentEvent::Text(delta.get("text")?.as_str()?.to_string())),
                _ => None,
            }
        }
        // Reaching for a tool can take a while with nothing to show for it, so
        // name it rather than letting the pane look stalled.
        "content_block_start" => {
            let block = event.get("content_block")?;
            if block.get("type")?.as_str()? != "tool_use" {
                return None;
            }
            Some(AgentEvent::Tool(block.get("name")?.as_str()?.to_string()))
        }
        _ => None,
    }
}

#[cfg(test)]
pub mod tests {
    use std::sync::mpsc;

    use ratatui::{
        crossterm::event::KeyCode,
        style::{Color, Modifier},
    };

    use super::*;
    use crate::{
        buffer::Buffer,
        editor::{
            app::{Mode, SPINNER},
            goto::PendingGoto,
            harness::{app, job, press, render, tag_fixture, temp_file},
            App,
        },
    };

    #[test]
    fn known_clis_get_their_non_interactive_flags() {
        let claude = Agent::at(PathBuf::from("/usr/bin/claude"), vec![]).unwrap();
        let cmd = claude.command("explain this");
        assert_eq!(claude.display, "claude");
        assert_eq!(
            args_of(&cmd),
            [
                "-p",
                "--output-format=stream-json",
                "--include-partial-messages",
                "--verbose",
                "explain this",
            ],
            "claude is asked for the event stream, so the reply can be watched"
        );

        let codex = Agent::at(PathBuf::from("/usr/bin/codex"), vec![]).unwrap();
        assert_eq!(args_of(&codex.command("explain this")), ["exec", "explain this"]);

        // Anything else we have no way to drive.
        let error = Agent::at(PathBuf::from("/usr/bin/mytool"), vec![]).unwrap_err();
        assert!(matches!(&error, ViviError::UnsupportedAgent(name) if name == "mytool"), "{error}");
    }

    #[test]
    fn vivi_agent_overrides_the_probe_and_may_carry_args() {
        let path = stub_agent("env-override", "codex", "#!/bin/sh\nexit 0\n");
        let dir = path.parent().unwrap().display().to_string();

        // A bare name is resolved against PATH...
        temp_env(&[("PATH", &dir), ("VIVI_AGENT", "codex")], || {
            let agent = Agent::resolve().unwrap();
            assert!(matches!(agent.kind, AgentKind::Codex));
            assert_eq!(args_of(&agent.command("hi")), ["exec", "hi"]);
        });

        // ...and extra words become leading arguments.
        temp_env(&[("PATH", &dir), ("VIVI_AGENT", "codex --model gpt")], || {
            let agent = Agent::resolve().unwrap();
            assert_eq!(agent.args, ["--model", "gpt"]);
        });

        temp_env(&[("PATH", &dir), ("VIVI_AGENT", "nonesuch")], || {
            let error = Agent::resolve().unwrap_err();
            assert!(matches!(&error, ViviError::AgentNotOnPath(p) if p == "nonesuch"), "{error}");
        });

        // ...but a CLI we don't know how to drive is rejected, not guessed at.
        let mytool = stub_agent("env-unsupported", "mytool", "#!/bin/sh\nexit 0\n");
        let mytool_dir = mytool.parent().unwrap().display().to_string();
        temp_env(&[("PATH", &mytool_dir), ("VIVI_AGENT", "mytool")], || {
            let error = Agent::resolve().unwrap_err();
            assert!(
                matches!(&error, ViviError::UnsupportedAgent(name) if name == "mytool"),
                "{error}"
            );
        });
    }

    #[test]
    fn probing_prefers_claude_then_codex_then_fails() {
        let claude = stub_agent("probe-claude", "claude", "#!/bin/sh\nexit 0\n");
        let codex = stub_agent("probe-codex", "codex", "#!/bin/sh\nexit 0\n");
        let claude_dir = claude.parent().unwrap().display().to_string();
        let codex_dir = codex.parent().unwrap().display().to_string();

        temp_env(&[("PATH", &format!("{codex_dir}:{claude_dir}")), ("VIVI_AGENT", "")], || {
            assert_eq!(Agent::resolve().unwrap().display, "claude", "claude wins the probe");
        });
        temp_env(&[("PATH", &codex_dir), ("VIVI_AGENT", "")], || {
            assert_eq!(Agent::resolve().unwrap().display, "codex", "falls back to codex");
        });
        temp_env(&[("PATH", "/nonexistent"), ("VIVI_AGENT", "")], || {
            assert!(matches!(Agent::resolve().unwrap_err(), ViviError::NoAgent));
        });
    }

    #[test]
    fn stream_json_lines_become_readable_text() {
        // These are verbatim from a real `claude -p --output-format=stream-json`
        // session, trimmed of the fields we do not read.
        let delta = r#"{"type":"stream_event","event":{"type":"content_block_delta",
            "index":0,"delta":{"type":"text_delta","text":"hello there"}}}"#;
        assert!(matches!(stream_event(delta), Some(AgentEvent::Text(t)) if t == "hello there"));

        // A tool call is named, so a long one does not look like a stall.
        let tool = r#"{"type":"stream_event","event":{"type":"content_block_start",
            "index":1,"content_block":{"type":"tool_use","name":"Bash","input":{}}}}"#;
        assert!(matches!(stream_event(tool), Some(AgentEvent::Tool(t)) if t == "Bash"));

        // A failure is reported in the result object, not on stderr.
        let failed = r#"{"is_error":true,"result":"Credit balance is too low"}"#;
        assert!(matches!(
            stream_event(failed),
            Some(AgentEvent::Text(t)) if t.contains("Credit balance")
        ));

        // Everything else is bookkeeping and stays off the screen.
        for noise in [
            r#"{"type":"system","subtype":"init","tools":["Bash"]}"#,
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}"#,
            r#"{"type":"stream_event","event":{"type":"message_stop"}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hello there"}]}}"#,
            r#"{"is_error":false,"duration_api_ms":2727}"#,
            "not json at all",
            "",
        ] {
            assert!(stream_event(noise).is_none(), "should be ignored: {noise}");
        }

        // Thinking is not the answer, and is not shown as though it were.
        let thinking = r#"{"type":"stream_event","event":{"type":"content_block_delta",
            "delta":{"type":"thinking_delta","thinking":"hmm"}}}"#;
        assert!(stream_event(thinking).is_none());
    }

    #[test]
    fn a_failing_agent_reports_its_stderr() {
        let _env = env_lock();
        let agent = Agent::at(
            stub_agent("failing", "claude", "#!/bin/sh\necho 'not logged in' >&2\nexit 3\n"),
            vec![],
        )
        .unwrap();
        let rx = agent.spawn("hi".into(), String::new());
        let mut failure = None;
        while let Ok(event) = rx.recv_timeout(std::time::Duration::from_secs(10)) {
            if let AgentEvent::Done(e) = event {
                failure = e;
                break;
            }
        }
        let failure = failure.expect("a non-zero exit is reported as a failure");
        let ViviError::AgentExited { code, detail } = &failure else {
            panic!("a non-zero exit must say so: {failure}");
        };
        assert_eq!(*code, Some(3));
        assert_eq!(detail.as_deref(), Some("not logged in"), "the agent's last word on stderr");
    }

    #[test]
    fn streamed_chunks_are_assembled_into_lines() {
        let (tx, rx) = mpsc::channel();
        let mut app = app("code");
        let mut streaming = job(rx);
        streaming.running = true;
        streaming.follow = true;
        streaming.height = 4;
        streaming.open = true;
        app.job = Some(streaming);

        // Chunks arrive mid-word and mid-line, exactly as the event stream sends
        // them: a word split in two, then a newline inside a chunk.
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
    fn the_agent_receives_the_selected_lines_and_the_prompt() {
        let _env = env_lock();
        // The stub records argv and stdin, then answers on stdout.
        // Per-process: two `cargo test` runs at once would otherwise read each
        // other's half-written capture.
        let out = std::env::temp_dir()
            .join(format!("vivi-test-capture-argv-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&out);
        // It answers the way claude does: one `stream-json` line per chunk.
        let script = format!(
            "#!/bin/sh\n{{ echo \"argv: $@\"; cat; }} > {}\n\
             echo '{{\"event\":{{\"type\":\"content_block_delta\",\
             \"delta\":{{\"type\":\"text_delta\",\"text\":\"the \"}}}}}}'\n\
             echo '{{\"event\":{{\"type\":\"content_block_delta\",\
             \"delta\":{{\"type\":\"text_delta\",\"text\":\"answer\"}}}}}}'\n",
            out.display()
        );
        let agent = Agent::at(stub_agent("capture", "claude", &script), vec![]).unwrap();

        let mut app = app("fn main() {\n    println!(\"hi\");\n}");
        press(&mut app, KeyCode::Char('V'));
        press(&mut app, KeyCode::Char('j'));
        let (start, end) = app.selection().unwrap();
        let rx = agent.spawn("what does this do?".into(), app.selection_context(start, end));

        let mut lines = Vec::new();
        let failure = loop {
            match rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap() {
                AgentEvent::Line(l) => lines.push(l),
                AgentEvent::Text(t) => lines.push(t),
                AgentEvent::Tool(t) => lines.push(format!("· {t}")),
                AgentEvent::Done(e) => break e,
            }
        };
        assert!(failure.is_none(), "{failure:?}");
        assert_eq!(lines, ["the ", "answer"], "the reply arrives in chunks, as it is written");

        let captured = std::fs::read_to_string(&out).unwrap();
        assert!(captured.starts_with("argv: -p --output-format=stream-json"), "{captured}");
        assert!(captured.contains("what does this do?"), "the prompt is an argument: {captured}");
        assert!(captured.contains("lines 1-2 of [No Name]"), "{captured}");
        assert!(captured.contains("fn main() {"), "{captured}");
        assert!(captured.contains("println!"), "{captured}");
        assert!(!captured.contains('}'), "the unselected third line is not sent: {captured}");
    }

    #[test]
    fn asking_stays_in_the_editor_and_shows_a_working_marker() {
        let agent = stub_agent("stays", "claude", "#!/bin/sh\nsleep 5\n");
        let dir = agent.parent().unwrap().display().to_string();
        let mut app = app("one\ntwo\nthree");

        press(&mut app, KeyCode::Char('V'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char(':'));
        for c in "edit explain".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        temp_env(&[("PATH", &dir), ("VIVI_AGENT", "")], || {
            press(&mut app, KeyCode::Enter);
        });

        // We are back in the file, not parked in an output pane.
        assert!(app.mode == Mode::Normal, "asking must not steal the editor");
        assert_eq!(app.selection(), None, "the command clears the highlight");
        let job = app.job.as_ref().unwrap();
        assert!(job.running && !job.open, "it runs in the background, pane closed");
        assert_eq!(job.prompt, "explain");

        // The agent's name and a spinner, not a sentence.
        let buffer = render(&mut app, 40, 5);
        let status = (0..40).map(|x| buffer[(x, 4)].symbol()).collect::<String>();
        assert!(SPINNER.iter().any(|s| status.contains(s)), "a spinner: {status:?}");
        assert!(status.contains("claude"), "beside the agent's name: {status:?}");
        for prose in ["working on lines", "to watch"] {
            assert!(!status.contains(prose), "but no prose: {status:?}");
        }

        // The lines it was handed stay marked, so you can see what it has.
        let marked = |y: u16| {
            let cell = &buffer[(0, y)];
            cell.modifier.contains(Modifier::REVERSED) && cell.modifier.contains(Modifier::DIM)
        };
        assert!(marked(0) && marked(1), "the lines sent to the agent are marked");
        assert!(!marked(2), "and only those lines");
        for cell in buffer.content() {
            assert_eq!(cell.bg, Color::Reset, "marking must not paint a background");
        }

        app.run_command("out");
        assert!(app.job.as_ref().unwrap().open, ":out brings up the pane");
    }

    #[test]
    fn asking_without_an_agent_reports_an_error_instead_of_hanging() {
        let mut app = app("code");
        temp_env(&[("PATH", "/nonexistent"), ("VIVI_AGENT", "")], || {
            app.ask_agent((0, 0), "explain");
        });
        assert!(app.job.is_none());
        let (text, is_error) = app.message.clone().unwrap();
        assert!(is_error && text.contains("no agent found"), "{text}");
    }

    #[test]
    fn indexing_does_not_block_asking_an_agent() {
        // These are unrelated background jobs. A language server building its
        // index must not stand between you and a question.
        let (caller, _) = tag_fixture("indexing-vs-agent");
        let mut app = App::new(Buffer::from_file(&caller).unwrap());
        app.goto = Some(PendingGoto::new(1, 0, 0, 0, "greet".into()));

        temp_env(&[("PATH", "/nonexistent"), ("VIVI_AGENT", "")], || {
            app.ask_agent((0, 0), "explain");
        });
        let (text, _) = app.message.clone().unwrap();
        assert!(
            text.contains("no agent found"),
            "it got as far as looking for an agent, not refused outright: {text}"
        );
        assert!(!text.contains("still working"), "{text}");
    }

    #[test]
    fn a_second_ask_is_refused_while_one_is_running() {
        let mut app = app("code");
        let mut first = job(mpsc::channel().1);
        first.prompt = "first".into();
        first.running = true;
        app.job = Some(first);

        app.run_command("edit second");
        assert_eq!(app.job.as_ref().unwrap().prompt, "first", "the running job is untouched");
        let (text, is_error) = app.message.clone().unwrap();
        assert!(!is_error && text.contains("still working"), "{text}");
    }

    #[test]
    fn a_finished_agent_triggers_a_reload() {
        let path = temp_file("after-agent", "before\n");
        let (tx, rx) = mpsc::channel();
        let mut app = App::new(Buffer::from_file(&path).unwrap());
        let mut running = job(rx);
        running.prompt = "rewrite it".into();
        running.running = true;
        app.job = Some(running);

        std::fs::write(&path, "after\n").unwrap();
        tx.send(AgentEvent::Done(None)).unwrap();
        app.drain_agent();

        assert_eq!(app.buffer.line(0), "after", "the agent's edit is picked up");
        let (text, is_error) = app.message.clone().unwrap();
        assert!(!is_error && text.contains("file reloaded"), "{text}");
        assert!(!app.job.as_ref().unwrap().running);
    }

    /// A stand-in agent binary, so tests never invoke a real CLI. `slot` keeps
    /// each test's stub in its own directory, since tests run in parallel.
    pub fn stub_agent(slot: &str, name: &str, script: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vivi-test-{slot}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    /// Set env vars for the duration of `f`. An empty value means "unset".
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Hold the environment still. `temp_env` edits process-wide state, so any
    /// test that *reads* it — including anything that spawns a subprocess, since
    /// the child inherits `PATH` — has to take this too, or it will occasionally
    /// run with another test's `PATH=/nonexistent` and fail to find `cat`.
    pub fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn temp_env(vars: &[(&str, &str)], f: impl FnOnce()) {
        let _guard = env_lock();
        let saved: Vec<_> = vars.iter().map(|(k, _)| (*k, std::env::var_os(k))).collect();
        for (k, v) in vars {
            if v.is_empty() {
                std::env::remove_var(k);
            } else {
                std::env::set_var(k, v);
            }
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }
}
