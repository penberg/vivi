//! vivi - a vi-like terminal text editor with first-class support for coding agents.

mod agent;
mod buffer;
mod editor;
mod error;
mod lsp;
mod text;
mod which;

use std::{path::Path, process::ExitCode};

use crate::{buffer::Buffer, editor::App, error::ViviError};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("vivi: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), ViviError> {
    let mut args = std::env::args_os().skip(1);
    let Some(path) = args.next() else {
        return Err(ViviError::Usage);
    };
    let buffer = Buffer::from_file(Path::new(&path))?;

    let mut terminal = ratatui::try_init()?;
    let result = App::new(buffer).run(&mut terminal);
    ratatui::restore();
    result
}
