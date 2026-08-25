//! What can go wrong, in one place.
//!
//! Vim answers a missing file with `E484`. The number is a lookup key for a
//! help system we do not have, so here the sentence is the whole message: the
//! part after the colon was always the part you read anyway.

use std::{fmt, io, path::PathBuf};

use crate::{agent::AGENT_ENV, buffer::short_name, lsp::LSP_ENV};

/// Every failure the editor can report. One variant per thing that actually
/// goes wrong, so the site that hits it does not have to phrase it, and the
/// site that shows it does not have to parse it back out of a string.
#[derive(Debug)]
pub enum ViviError {
    /// Started with nothing to edit.
    Usage,
    /// The terminal, or anything else the outside world does to us.
    Io(io::Error),

    // --- the file ---
    /// Asked to jump to, or open, something that is not there.
    NotFound(PathBuf),
    /// It is there, but reading it failed — a directory, a permission, a device.
    Unreadable { path: PathBuf, source: io::Error },
    /// Writing a delete back failed; the file still holds the old contents.
    Unwritable { path: PathBuf, source: io::Error },
    /// The buffer holds deletes the file does not, and the command would
    /// lose or need them. The payload is the way through.
    Unsaved(String),
    /// Someone else wrote the file while the buffer holds unwritten deletes.
    ChangedOnDisk,

    // --- ex commands ---
    /// Typed something after `:` that is not in the command table.
    UnknownCommand(String),
    /// A name in the command table with no arm to run it: our bug, not yours.
    Unimplemented(String),
    /// The command needs an argument; the payload is how to spell it.
    MissingArgument(String),
    /// A range we could not read: `3,` with nothing after the comma.
    InvalidRange,

    // --- agents ---
    /// Nothing to run: no known agent on PATH, and no override set.
    NoAgent,
    /// The override is set to an empty string, which names nothing.
    AgentEnvEmpty,
    /// The override names a program that is not on PATH.
    AgentNotOnPath(String),
    /// We know the program by name but not how to invoke it non-interactively.
    UnsupportedAgent(String),
    /// The program is there, but spawning it failed.
    AgentStartFailed(io::Error),
    /// The agent ran and came back unhappy. `detail` is the last thing it said
    /// on stderr, which is usually the only useful part.
    AgentExited { code: Option<i32>, detail: Option<String> },
    /// We could not even collect the agent's exit status.
    AgentFailed(io::Error),

    // --- language servers ---
    /// Nothing known to run for this kind of file.
    NoLanguageServer(String),
    /// The override is set to an empty string, which names nothing.
    LspEnvEmpty,
    /// The override names a program that is not on PATH.
    LspEnvNotOnPath(String),
    /// We know which server this file wants, but it is not installed.
    LspNotInstalled(String),
    /// The program is there, but spawning it failed.
    LspStartFailed { name: String, source: io::Error },
    /// A server we already failed to start once, so we are not trying again.
    LspUnavailable,
    /// No file, so nothing to resolve symbols against.
    NoFileForLsp,
    /// The server answered a request with an error of its own.
    Refused { server: String, symbol: String, detail: String },
    /// The server process is gone. `status` is how it went.
    ServerExited { status: String, detail: Option<String> },
    /// It started, but never answered `initialize`.
    ServerNeverStarted { server: String, detail: Option<String> },
    /// It is alive and working, just not finished — try again later.
    ServerIndexing { server: String, attempts: u32 },
    /// The server looked and there is nothing there. About your cursor, not it.
    NoDefinition(String),
}

impl fmt::Display for ViviError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => write!(f, "usage: vivi <file>"),
            Self::Io(e) => write!(f, "{e}"),

            Self::NotFound(path) => write!(f, "can't find file {}", short_name(path)),
            Self::Unreadable { path, source } => {
                write!(f, "can't open {}: {source}", short_name(path))
            }
            Self::Unwritable { path, source } => {
                write!(f, "can't write {}: {source}", short_name(path))
            }
            Self::Unsaved(hint) => write!(f, "no write since last change — {hint}"),
            Self::ChangedOnDisk => {
                write!(f, "file changed on disk — :reload! takes it, :w overwrites it")
            }

            Self::UnknownCommand(word) => write!(f, "not an editor command: {word}"),
            Self::Unimplemented(name) => write!(f, "{name} is not implemented"),
            Self::MissingArgument(usage) => write!(f, "argument required: {usage}"),
            Self::InvalidRange => write!(f, "invalid range"),

            Self::NoAgent => {
                write!(f, "no agent found: install claude or codex, or set {AGENT_ENV}")
            }
            Self::AgentEnvEmpty => write!(f, "{AGENT_ENV} is set but empty"),
            Self::AgentNotOnPath(program) => {
                write!(f, "{AGENT_ENV}: {program} not found on PATH")
            }
            Self::UnsupportedAgent(name) => write!(f, "unsupported agent: {name}"),
            Self::AgentStartFailed(e) => write!(f, "failed to start agent: {e}"),
            Self::AgentExited { code, detail } => {
                match code {
                    Some(code) => write!(f, "agent exited with status {code}")?,
                    None => write!(f, "agent was killed")?,
                }
                match detail {
                    Some(detail) => write!(f, ": {detail}"),
                    None => Ok(()),
                }
            }
            Self::AgentFailed(e) => write!(f, "agent failed: {e}"),

            Self::NoLanguageServer(extension) => {
                write!(f, "no language server known for .{extension} files")
            }
            Self::LspEnvEmpty => write!(f, "{LSP_ENV} is set but empty"),
            Self::LspEnvNotOnPath(program) => {
                write!(f, "{LSP_ENV}: {program} not found on PATH")
            }
            Self::LspNotInstalled(program) => {
                write!(f, "{program} not found on PATH (set {LSP_ENV} to override)")
            }
            Self::LspStartFailed { name, source } => {
                write!(f, "failed to start {name}: {source}")
            }
            Self::LspUnavailable => write!(f, "language server unavailable — :messages"),
            Self::NoFileForLsp => write!(f, "no file, so no language server"),
            Self::Refused { server, symbol, detail } => {
                write!(f, "{server} refused {symbol}: {detail}")
            }
            Self::ServerExited { status, detail } => match detail {
                Some(detail) => write!(f, "{status}: {detail}"),
                None => write!(f, "{status}"),
            },
            Self::ServerNeverStarted { server, detail } => match detail {
                Some(detail) => write!(f, "{server} never finished starting: {detail}"),
                None => write!(f, "{server} never finished starting (no reply to initialize)"),
            },
            Self::ServerIndexing { server, attempts } => {
                write!(f, "{server} is still indexing after {attempts} tries")
            }
            Self::NoDefinition(symbol) => write!(f, "no definition for {symbol}"),
        }
    }
}

impl std::error::Error for ViviError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e)
            | Self::Unreadable { source: e, .. }
            | Self::Unwritable { source: e, .. }
            | Self::AgentStartFailed(e)
            | Self::AgentFailed(e)
            | Self::LspStartFailed { source: e, .. } => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for ViviError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
