//! A minimal LSP client: enough of the protocol to ask "where is this symbol
//! defined?" and get an answer back without blocking the editor.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        mpsc::{self, Receiver, Sender},
        Arc, Mutex,
    },
};

use serde_json::{json, Value};

use crate::{error::ViviError, which::find_on_path};

/// The environment variable that chooses the language server.
pub const LSP_ENV: &str = "VIVI_LSP";

/// Error codes that mean "try again", not "give up".
/// <https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/>
const RETRYABLE: [i64; 3] = [
    -32801, // ContentModified
    -32802, // ServerCancelled
    -32800, // RequestCancelled
];

/// Where a symbol is defined.
#[derive(Debug, Clone, PartialEq)]
pub struct Location {
    /// The file it is in, as the server spelled it.
    pub path: PathBuf,
    /// Line, counting from zero as the buffer does.
    pub line: usize,
    /// Column, in whatever units the server negotiated.
    pub character: usize,
}

/// LSP counts columns in UTF-16 code units unless the server agrees otherwise.
/// Getting this wrong silently lands the cursor in the wrong place on any line
/// with non-ASCII, so we negotiate explicitly and remember the answer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Encoding {
    Utf8,
    Utf16,
}

#[derive(Debug)]
pub enum LspEvent {
    /// The server answered `initialize`; requests are now allowed.
    Ready,
    /// The server started or finished building its index.
    Indexing(bool),
    /// A reply to `definition()`, matched by the id it returned.
    Definition { id: u64, target: Option<Location> },
    /// The server, or our conversation with it, broke.
    Error(String),
}

pub struct Lsp {
    /// The server process, kept so we can notice it dying and kill it at the end.
    child: Child,
    /// Messages on their way to the server, written by the writer thread.
    outgoing: Sender<String>,
    /// Messages back from it, already framed and parsed by the reader thread.
    incoming: Receiver<Value>,
    /// The id the next request will carry.
    next_id: u64,
    /// Requests we sent and still expect an answer for.
    definitions: HashSet<u64>,
    /// Files the server has been told about; re-opening one is a protocol error.
    opened: HashMap<PathBuf, i64>,
    /// Anything we tried to send before the handshake completed. A server may
    /// ignore messages that arrive before `initialized`, so they wait here.
    queued: Vec<Value>,
    /// The tail of the server's stderr, so a failure can say what it complained
    /// about instead of just "nothing found".
    stderr: Arc<Mutex<VecDeque<String>>>,
    /// The program we ran, so a failure can name it.
    pub name: String,
    /// Whether the handshake finished and requests are allowed.
    pub ready: bool,
    /// Whether it is building its index, which is why a lookup may come back
    /// empty and be worth asking again.
    pub indexing: bool,
    /// The column units we negotiated.
    pub encoding: Encoding,
}

impl Lsp {
    pub fn start(path: &Path) -> Result<Self, ViviError> {
        let (program, args) = Self::command_for(path)?;
        let root = Self::project_root(path);
        let name = program
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| program.display().to_string());

        let mut child = Command::new(&program)
            .args(&args)
            .current_dir(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Captured, not inherited: letting the server write to the real
            // stderr would scribble over the TUI, but its complaints are the
            // best explanation we have when a lookup fails.
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| ViviError::LspStartFailed { name: name.clone(), source })?;

        let (to_server, from_editor) = mpsc::channel::<String>();
        let mut stdin = child.stdin.take().expect("stdin was piped");
        std::thread::spawn(move || {
            for message in from_editor {
                let frame = format!("Content-Length: {}\r\n\r\n{message}", message.len());
                if stdin.write_all(frame.as_bytes()).is_err() || stdin.flush().is_err() {
                    return;
                }
            }
        });

        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        if let Some(stderr) = child.stderr.take() {
            let tail = Arc::clone(&stderr_tail);
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let Ok(mut tail) = tail.lock() else { return };
                    if tail.len() == 20 {
                        tail.pop_front();
                    }
                    tail.push_back(line);
                }
            });
        }

        let (to_editor, from_server) = mpsc::channel::<Value>();
        let stdout = child.stdout.take().expect("stdout was piped");
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Some(message) = read_frame(&mut reader) {
                if to_editor.send(message).is_err() {
                    return;
                }
            }
        });

        let mut lsp = Self {
            child,
            outgoing: to_server,
            incoming: from_server,
            next_id: 0,
            definitions: HashSet::new(),
            opened: HashMap::new(),
            queued: Vec::new(),
            stderr: stderr_tail,
            name,
            ready: false,
            indexing: false,
            encoding: Encoding::Utf16,
        };

        // `initialize` is the one message that must not be queued.
        lsp.next_id += 1;
        lsp.send(json!({
            "jsonrpc": "2.0",
            "id": lsp.next_id,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": uri_of(&root),
                "capabilities": {
                    // Ask for byte offsets. Without this we get UTF-16 columns.
                    "general": { "positionEncodings": ["utf-8", "utf-16"] },
                    "textDocument": { "definition": { "linkSupport": false } },
                    "window": { "workDoneProgress": true },
                },
            },
        }));
        Ok(lsp)
    }

    /// Pick a language server for this file: `$VIVI_LSP` if set, else by extension.
    pub fn command_for(path: &Path) -> Result<(PathBuf, Vec<String>), ViviError> {
        if let Some(spec) = std::env::var_os(LSP_ENV) {
            let spec = spec.to_string_lossy().into_owned();
            let mut words = spec.split_whitespace().map(str::to_string);
            let program = words.next().ok_or(ViviError::LspEnvEmpty)?;
            let found = find_on_path(&program)
                .ok_or_else(|| ViviError::LspEnvNotOnPath(program.clone()))?;
            return Ok((found, words.collect()));
        }

        let extension = path.extension().unwrap_or_default().to_string_lossy().into_owned();
        let (program, args): (&str, &[&str]) = match extension.as_str() {
            // Only rust-analyzer, because it is the only server this has been
            // tested against. Other languages go through $VIVI_LSP until
            // someone can actually try them.
            "rs" => ("rust-analyzer", &[]),
            other => return Err(ViviError::NoLanguageServer(other.to_string())),
        };
        let found = find_on_path(program)
            .ok_or_else(|| ViviError::LspNotInstalled(program.to_string()))?;
        Ok((found, args.iter().map(|a| a.to_string()).collect()))
    }

    /// The directory the server should treat as the project root.
    pub fn project_root(path: &Path) -> PathBuf {
        let start = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let absolute = if start.is_absolute() {
            start
        } else {
            std::env::current_dir().unwrap_or_default().join(start)
        };
        // The outermost Cargo.toml wins, so a workspace member resolves against
        // the whole workspace rather than just its own crate.
        let mut root = None;
        for dir in absolute.ancestors() {
            if dir.join("Cargo.toml").is_file() {
                root = Some(dir.to_path_buf());
            }
            if dir.join(".git").exists() {
                return root.unwrap_or_else(|| dir.to_path_buf());
            }
        }
        root.unwrap_or(absolute)
    }

    /// Tell the server about a file. The server will not answer questions about
    /// a document it has never been shown, even one already on disk.
    pub fn did_open(&mut self, path: &Path, text: &str) {
        let language_id = match path.extension().unwrap_or_default().to_string_lossy().as_ref() {
            "rs" => "rust",
            "go" => "go",
            "py" => "python",
            "ts" | "tsx" => "typescript",
            "js" | "jsx" => "javascript",
            other => other.to_string().leak(),
        };
        match self.opened.get(path).copied() {
            // Already open: the file changed on disk, so re-sync it in full.
            Some(version) => {
                self.opened.insert(path.to_path_buf(), version + 1);
                self.notify(
                    "textDocument/didChange",
                    json!({
                        "textDocument": {"uri": uri_of(path), "version": version + 1},
                        "contentChanges": [{"text": text}],
                    }),
                );
            }
            None => {
                self.opened.insert(path.to_path_buf(), 1);
                self.notify(
                    "textDocument/didOpen",
                    json!({"textDocument": {
                        "uri": uri_of(path),
                        "languageId": language_id,
                        "version": 1,
                        "text": text,
                    }}),
                );
            }
        }
    }

    /// Ask where the symbol at this position is defined. The returned id shows
    /// up again on the matching `LspEvent::Definition`.
    pub fn definition(&mut self, path: &Path, line: usize, character: usize) -> u64 {
        let id = self.request(
            "textDocument/definition",
            json!({
                "textDocument": {"uri": uri_of(path)},
                "position": {"line": line, "character": character},
            }),
        );
        self.definitions.insert(id);
        id
    }

    /// Collect whatever the server has said since last time.
    pub fn poll(&mut self) -> Vec<LspEvent> {
        let mut events = Vec::new();
        while let Ok(message) = self.incoming.try_recv() {
            if let Some(event) = self.interpret(message) {
                events.push(event);
            }
        }
        events
    }

    fn interpret(&mut self, message: Value) -> Option<LspEvent> {
        if let Some(method) = message.get("method").and_then(Value::as_str) {
            // Servers ask us things too; an unanswered request can stall them.
            if let Some(id) = message.get("id") {
                self.send(json!({"jsonrpc": "2.0", "id": id, "result": Value::Null}));
            }
            if method == "$/progress" {
                let value = message.get("params")?.get("value")?;
                let token = message.get("params")?.get("token")?.as_str().unwrap_or_default();
                if !token.to_lowercase().contains("index") && !token.to_lowercase().contains("cach")
                {
                    return None;
                }
                return match value.get("kind")?.as_str()? {
                    "begin" => {
                        self.indexing = true;
                        Some(LspEvent::Indexing(true))
                    }
                    "end" => {
                        self.indexing = false;
                        Some(LspEvent::Indexing(false))
                    }
                    _ => None,
                };
            }
            return None;
        }

        let id = message.get("id")?.as_u64()?;

        if let Some(error) = message.get("error") {
            let was_definition = self.definitions.remove(&id);
            let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
            // ContentModified / RequestCancelled / ServerCancelled all mean "the
            // ground shifted, ask again" — routine while a server is indexing,
            // and the spec says to retry rather than surface them.
            if was_definition && RETRYABLE.contains(&code) {
                return Some(LspEvent::Definition { id, target: None });
            }
            let text = error.get("message").and_then(Value::as_str).unwrap_or("request failed");
            return Some(LspEvent::Error(text.to_string()));
        }

        if !self.ready && !self.definitions.contains(&id) {
            self.ready = true;
            self.encoding = match message
                .get("result")
                .and_then(|r| r.get("capabilities"))
                .and_then(|c| c.get("positionEncoding"))
                .and_then(Value::as_str)
            {
                Some("utf-8") => Encoding::Utf8,
                _ => Encoding::Utf16,
            };
            // `initialized` must be the first thing after the handshake; only
            // then may everything we queued up go out.
            self.send(json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}));
            for message in std::mem::take(&mut self.queued) {
                self.send(message);
            }
            return Some(LspEvent::Ready);
        }

        if self.definitions.remove(&id) {
            return Some(LspEvent::Definition { id, target: parse_location(message.get("result")?) });
        }
        None
    }

    /// The last thing the server said on stderr, if anything.
    pub fn last_error(&self) -> Option<String> {
        let tail = self.stderr.lock().ok()?;
        tail.back().cloned()
    }

    /// Whether the server process has gone away, and with what status.
    pub fn exited(&mut self) -> Option<String> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(match status.code() {
                Some(code) => format!("{} exited with status {code}", self.name),
                None => format!("{} was killed", self.name),
            }),
            _ => None,
        }
    }

    fn request(&mut self, method: &str, params: Value) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.dispatch(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        id
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.dispatch(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Send now if the handshake is done, otherwise hold it until it is.
    fn dispatch(&mut self, message: Value) {
        if self.ready {
            self.send(message);
        } else {
            self.queued.push(message);
        }
    }

    fn send(&self, message: Value) {
        let _ = self.outgoing.send(message.to_string());
    }
}

impl Drop for Lsp {
    fn drop(&mut self) {
        // Ask nicely, then make sure: an orphaned rust-analyzer will happily sit
        // there eating a core after the editor is gone.
        self.send(json!({"jsonrpc": "2.0", "id": 0, "method": "shutdown"}));
        self.send(json!({"jsonrpc": "2.0", "method": "exit"}));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Read one `Content-Length`-framed JSON-RPC message.
fn read_frame(reader: &mut impl BufRead) -> Option<Value> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None; // The server closed its output.
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0; length?];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

/// A definition reply may be a single `Location`, a list of them, or null.
fn parse_location(result: &Value) -> Option<Location> {
    let first = match result {
        Value::Array(items) => items.first()?,
        Value::Null => return None,
        object => object,
    };
    // `LocationLink` names its fields differently from `Location`.
    let uri = first.get("uri").or_else(|| first.get("targetUri"))?.as_str()?;
    let range = first
        .get("range")
        .or_else(|| first.get("targetSelectionRange"))
        .or_else(|| first.get("targetRange"))?;
    let start = range.get("start")?;
    Some(Location {
        path: path_of(uri)?,
        line: start.get("line")?.as_u64()? as usize,
        character: start.get("character")?.as_u64()? as usize,
    })
}

/// `/a/b c.rs` -> `file:///a/b%20c.rs`
fn uri_of(path: &Path) -> String {
    let absolute = crate::buffer::absolute(path);
    let mut out = String::from("file://");
    for byte in absolute.to_string_lossy().as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// `file:///a/b%20c.rs` -> `/a/b c.rs`
fn path_of(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // Skip an empty authority component: `file://localhost/x` is not supported.
    let rest = rest.strip_prefix('/').map(|r| format!("/{r}")).unwrap_or_else(|| rest.to_string());
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?, 16)
            {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(PathBuf::from(String::from_utf8(out).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole protocol against the real server: start it, open a file, ask
    /// where a symbol is defined, and keep asking while it indexes.
    #[test]
    #[ignore = "starts rust-analyzer and waits for it to index the crate"]
    fn resolves_a_cross_file_symbol_against_rust_analyzer() {
        let path = std::fs::canonicalize("editor/ui.rs").unwrap();
        let source = std::fs::read_to_string(&path).unwrap();
        let (row, line) = source
            .lines()
            .enumerate()
            .find(|(_, l)| l.contains("let text = expand_tabs("))
            .expect("the call site this test is anchored to");
        let col = line.find("expand_tabs").unwrap() + 2;

        let mut lsp = Lsp::start(&path).expect("rust-analyzer must be installed");
        lsp.did_open(&path, &source);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        let mut target = None;
        while std::time::Instant::now() < deadline && target.is_none() {
            let id = lsp.definition(&path, row, col);
            let answered = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                for event in lsp.poll() {
                    if let LspEvent::Definition { id: got, target: found } = event {
                        if got == id {
                            target = found;
                        }
                    }
                }
                if target.is_some() || std::time::Instant::now() > answered {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }

        let target = target.expect("rust-analyzer never resolved the symbol");
        assert!(target.path.ends_with("text.rs"), "jumped to {:?}", target.path);
        let definition = std::fs::read_to_string(&target.path).unwrap();
        let line = definition.lines().nth(target.line).unwrap();
        assert!(line.contains("fn expand_tabs"), "landed on {line:?}");
        assert_eq!(
            &line[target.character..target.character + "expand_tabs".len()],
            "expand_tabs",
            "the column points at the name itself"
        );
        assert_eq!(lsp.encoding, Encoding::Utf8, "we asked for byte offsets");
    }
}
