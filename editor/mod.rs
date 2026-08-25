//! The editor. [`app`] holds the state, everything the editor does with it, and
//! the loop that keeps it moving; every other module here is one self-contained
//! piece it reaches for — the tables, the arithmetic, and the drawing.

pub mod app;
pub mod cmd;
pub mod delete;
pub mod goto;
pub mod jump;
pub mod keys;
pub mod motion;
pub mod pane;
pub mod ui;

#[cfg(test)]
pub mod harness;

pub use app::App;
