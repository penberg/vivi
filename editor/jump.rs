//! The tag stack: what a place we jumped from looks like, how `:tag` spells a
//! destination, and what `:jumps` says about the pile.

use std::path::{Path, PathBuf};

/// Somewhere we jumped from, so `Ctrl-T` can put us back.
#[derive(Debug, Clone, PartialEq)]
pub struct Jump {
    pub path: Option<PathBuf>,
    pub row: usize,
    pub col: usize,
}

/// `<file>[:<line>[:<col>]]`, as `:tag` spells a destination. Lines and columns
/// are 1-based where a human types them and 0-based everywhere else, so they
/// are converted here rather than at the other end.
pub fn parse_tag(args: &str) -> (PathBuf, usize, usize) {
    let mut parts = args.split(':');
    let file = parts.next().unwrap_or_default();
    let line = parts.next().and_then(|l| l.parse::<usize>().ok()).unwrap_or(1);
    let col = parts.next().and_then(|c| c.parse::<usize>().ok()).unwrap_or(1);
    (Path::new(file).to_path_buf(), line.saturating_sub(1), col.saturating_sub(1))
}

/// What `:jumps` says: how far in you are, and what `Ctrl-T` would do next.
pub fn describe(stack: &[Jump]) -> String {
    let Some(jump) = stack.last() else {
        return "tag stack empty".to_string();
    };
    let name = jump
        .path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "[No Name]".to_string());
    format!("{} deep · back to {name}:{}", stack.len(), jump.row + 1)
}
