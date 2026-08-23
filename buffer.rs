//! The file we are looking at, held in memory a line at a time.

use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

use crate::error::ViviError;

/// An inclusive range of buffer lines: a visual selection, or the lines an ex
/// command addresses.
pub type LineRange = (usize, usize);

/// An in-memory, read-only view of a file.
pub struct Buffer {
    /// Where it came from, or `None` for a buffer with no file behind it.
    pub path: Option<PathBuf>,
    /// The file, split on newlines, always at least one line long.
    pub lines: Vec<String>,
    /// Set when the file did not exist on disk.
    pub is_new: bool,
    /// Modification time and size, to notice edits made behind our back.
    pub stamp: Option<(SystemTime, u64)>,
}

impl Buffer {
    pub fn from_file(path: &Path) -> Result<Self, ViviError> {
        let (lines, is_new) = match std::fs::read_to_string(path) {
            Ok(text) => {
                let mut lines: Vec<String> = text
                    .split('\n')
                    .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
                    .collect();
                // A trailing newline terminates the last line, it does not start a new one.
                if lines.len() > 1 && lines.last().is_some_and(|l| l.is_empty()) {
                    lines.pop();
                }
                (lines, false)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (vec![String::new()], true),
            Err(source) => return Err(ViviError::Unreadable { path: path.to_path_buf(), source }),
        };
        Ok(Self {
            path: Some(absolute(path)),
            lines,
            is_new,
            stamp: file_stamp(path),
        })
    }

    /// How this buffer is named on the status line and in messages.
    pub fn name(&self) -> String {
        match &self.path {
            Some(path) => short_name(path),
            None => "[No Name]".to_string(),
        }
    }

    pub fn line(&self, row: usize) -> &str {
        self.lines.get(row).map(String::as_str).unwrap_or("")
    }

    /// The whole buffer as one string, which is what `textDocument/didOpen` wants.
    pub fn text(&self) -> String {
        let mut out = self.lines.join("\n");
        out.push('\n');
        out
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }
}

/// Resolve a path to an absolute one. Everything downstream — `file://` URIs
/// for the language server, jump targets, change detection — needs a real
/// absolute path, and a relative one silently produces a nonsense URI.
pub fn absolute(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    match std::env::current_dir() {
        Ok(cwd) if path.is_relative() => cwd.join(path),
        _ => path.to_path_buf(),
    }
}

/// A path as a human wants to read it: relative to where we were started when
/// that is shorter, absolute otherwise. Every message that names a file goes
/// through here, so the same file reads the same way wherever it is mentioned.
pub fn short_name(path: &Path) -> String {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| path.strip_prefix(cwd).ok())
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Cheap change detection: an agent rewriting a file changes one or both of these.
pub fn file_stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{harness::temp_file, App};

    #[test]
    fn the_file_reloads_when_it_changes_underneath_us() {
        let path = temp_file("watch", "one\ntwo\nthree\n");
        let mut app = App::new(Buffer::from_file(&path).unwrap());
        app.goto_line(2);
        assert_eq!(app.buffer.len(), 3);

        // Nothing changed: no reload, no message.
        app.message = None;
        app.poll_file();
        assert!(app.message.is_none());

        // An agent rewrites the file behind our back.
        std::fs::write(&path, "one\nrewritten\nthree\nfour\n").unwrap();
        // Some filesystems only keep second-granularity mtimes, so make sure the
        // size differs too — which is exactly why the stamp carries both.
        app.poll_file();
        assert_eq!(app.buffer.line(1), "rewritten");
        assert_eq!(app.buffer.len(), 4);
        assert!(app.message.as_ref().unwrap().0.contains("reloaded"));
        assert_eq!(app.row, 2, "the cursor stays put when the line still exists");
    }

    #[test]
    fn reloading_a_shorter_file_keeps_the_cursor_in_bounds() {
        let path = temp_file("shrink", "one\ntwo\nthree\nfour\nfive\n");
        let mut app = App::new(Buffer::from_file(&path).unwrap());
        app.goto_line(4);

        std::fs::write(&path, "one\n").unwrap();
        app.poll_file();
        assert_eq!(app.buffer.len(), 1);
        assert_eq!((app.row, app.col), (0, 0), "cursor clamped into the shorter file");
    }

    #[test]
    fn a_file_is_required_on_the_command_line() {
        // `Buffer::empty` is gone with it: there is no unnamed-buffer state left
        // for the rest of the editor to have to think about.
        let path = temp_file("required", "hello\n");
        let buffer = Buffer::from_file(&path).unwrap();
        assert!(buffer.path.is_some());
    }
}
