# CLAUDE.md

## Code style

- Group imports as one `use <crate>::{<modules>}` statement per crate — merge `use std::path::Path;` and `use std::process::ExitCode;` into `use std::{path::Path, process::ExitCode};`. Crate-local imports go under a single `use crate::{...}`.
- `pub(super)` and `pub(crate)` are forbidden. Use plain `pub` (or no visibility modifier) instead.
- Document every member of a struct or enum, or none of them. A type where half the fields carry a `///` and half do not reads as if the undocumented ones were forgotten. If a field is worth a line, they all are; if the type's own doc comment already says everything, leave the members bare.
- One `///` per member — a comment placed above two fields only attaches to the first.

## Reading order

- Every file reads from top to bottom, like prose. Declarations first — the module doc, the imports, the constants and types the file is about — then the entry point, then everything it reaches, in the order it reaches them. A helper goes below its caller, never above it. Someone reading a file straight through should meet each piece already knowing why it exists.
- A file that has grown past that — where you have to scroll back to work out what calls what — wants splitting, not reordering. Split along what the code *does*, one job per module, and let the parent `mod.rs` be the table of contents: the module list and nothing else.
- The same order applies inside a `#[cfg(test)]` module: the tests first, the fixtures and helpers they share underneath.

## Tests

- Do not write simple unit tests. A test that pins a pure helper's output, round-trips a value, or re-states a `match` arm only restates the code and has to be edited every time the code changes.
- Write tests that exercise the real thing end to end — drive the editor, speak the actual protocol to the actual server, run the real binary. Those catch bugs the code cannot state about itself.
- Mark tests that need a real external program or take a long time `#[ignore = "<why>"]`, so `cargo test` stays fast.
- When a test cannot fail without the production code being obviously broken to a reader, delete it instead of maintaining it.
- Tests live beside the code they exercise: each module tests itself. Shared fixtures go in one place the whole tree can reach, like `editor/harness.rs`.
