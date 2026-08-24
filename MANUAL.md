# The vivi manual

Everything `vivi` responds to. See [README.md](README.md) for what it is and why.

- [Starting up](#starting-up)
- [Modes](#modes)
- [Moving around](#moving-around)
- [Deleting lines](#deleting-lines)
- [Ex commands](#ex-commands)
- [Ranges](#ranges)
- [Asking an agent](#asking-an-agent)
- [Panes](#panes)
- [Jumping to definitions](#jumping-to-definitions)
- [When something looks broken](#when-something-looks-broken)
- [Language servers](#language-servers)
- [Reloading](#reloading)
- [The status line](#the-status-line)
- [How text is displayed](#how-text-is-displayed)
- [Limitations](#limitations)

## Starting up

    vivi <filename>

The filename is required: nothing here can write a file, so an empty unnamed
buffer would be a window onto nothing. `vivi` with no argument prints
`usage: vivi <file>` and exits.

A file that opens is unremarkable and says nothing — the status line just shows
its name. A filename that does not exist opens an empty buffer and reports
`can't find file <name>`, as an error, so it survives your first keystroke: in
a viewer you cannot type into, a missing file is a dead end you want to notice
rather than a blank page you mistake for an empty file.

There is no insert mode: the agent is how text gets written. The one edit
`vivi` makes itself is deleting lines — see
[Deleting lines](#deleting-lines) — and `:w` is what puts a delete on disk.

Output that is not a terminal — a pipe, a cron job — is reported as an ordinary
error rather than a panic. The language server, if one is known for the file
type, is started here too; see [Language servers](#language-servers).

## Modes

| Mode | Entered by | Left by |
| --- | --- | --- |
| Normal | the default | — |
| Visual Line | `V` or `v` | `Esc`, `V`, `v`, `d` (deleting it), or running a command |
| Command | `:` | `Enter` runs it, `Esc` cancels, `Backspace` on an empty line cancels |
| Output | `:output` or `Ctrl-W w` for the agent's reply, `:help` or `:messages` for a document | `q` or `Esc`, and `Ctrl-W w` / `Ctrl-W c` for the agent pane |

`v` and `V` do the same thing: selection is always linewise, because the
things you send to an agent are lines.

Typing `:` opens an input line above the status line, between two rules, with a
`> ` marker:

    ────────────────────────────────────────────────
    > '<,'>edit make this handle unicode and add a
      test for the emoji case
    ────────────────────────────────────────────────
    src/main.rs:42  3 lines

It is a text area, not a single line. A prompt too long for the screen wraps
onto as many rows as it needs, growing upward, with continuation lines lined up
under the marker. It stops at half the screen and then scrolls to keep the end —
and the cursor — in view, so nothing ever runs off the right edge and the buffer
never disappears.

**The status line is always the last row.** The input box takes space from the
buffer above it, never from the status line, so the filename, the spinner and
the line number stay exactly where they are whether or not you are typing.

## Moving around

| Key | Does |
| --- | --- |
| `h` `l`, `←` `→` | left / right one character |
| `Backspace` / `Space` | left / right one character |
| `j` `k`, `↓` `↑` | down / up one line |
| `Enter` | down one line |
| `w` / `b` | start of the next / previous word |
| `0` / `Home` | first column |
| `^` | first non-blank character |
| `$` / `End` | last column |
| `gg` | first line |
| `G` | last line |
| `Ctrl-D` / `Ctrl-U` | half a screen down / up |
| `Ctrl-F` / `Ctrl-B`, `PageDown` / `PageUp` | a screen down / up |
| `Ctrl-E` / `Ctrl-Y` | scroll one line, leaving the cursor on its line until the edge pushes it |
| `:42` | jump to line 42 |

Word motions use vi's three character classes — keyword characters
(alphanumerics and `_`), punctuation, and whitespace — so `w` stops at the
boundary between `foo` and `(`.

The cursor keeps a *wanted column*: move down through a short line and back into
a long one, and it returns to where it was, exactly like vi.

## Deleting lines

| Key / command | Does |
| --- | --- |
| `dd` | delete the current line |
| `d` | in a selection, delete the selected lines |
| `:delete` / `:d` | delete the range, defaulting to the current line |
| `:write` / `:w` | write the buffer back to the file |

Deleting is linewise, like selection. A delete changes only the buffer: `:w`
writes it to disk, and until then `[+]` on the status line marks the buffer
as ahead of the file. There is no undo — `:reload!` re-reads the file, which
discards every unwritten delete, and `:q!` quits without them. Deleting more
than one line reports `3 fewer lines`; deleting every line leaves a single
empty one, as vi does.

The agent and unwritten deletes do not mix, in either direction. While an
agent is working a delete is refused with `an agent is still working`: it may
be about to rewrite the very lines you are deleting. And `:edit` with
unwritten deletes is refused with `no write since last change — :w them
first`: the agent works on the file as it is on disk, so deletes it cannot
see would be silently lost when its rewrite is reloaded.

## Ex commands

Every command has a full name and, where it earns one, a short alias. Both
spellings are equivalent — `:edit` and `:e` are the same command.

| Command | Alias | Does |
| --- | --- | --- |
| `:<line>` | | jump to a line (`:42`, `:$`, `:'>`) |
| `:quit` | `:q` | quit |
| `:edit <prompt>` | `:e` | have the agent edit the range |
| `:delete` | `:d` | delete the range, defaulting to the current line |
| `:write` | `:w` | write the buffer to the file |
| `:definition` | `:def` | jump to the definition under the cursor (same as `gd`) |
| `:pop` | `:po` | unwind one level of the tag stack (same as `Ctrl-T`) |
| `:tag <file>[:<line>[:<col>]]` | `:ta` | jump to a location, no language server involved |
| `:jumps` | | how deep the tag stack is, and where the last jump came from |
| `:lsp` | | what the language server is doing |
| `:output` | `:out` | open the agent output pane |
| `:reload` | | force a re-read from disk |
| `:messages` | `:mes` | every message so far, errors included |
| `:help` | `:h` | keys and commands, from the same tables that implement them |

A trailing `!` forces. With unwritten deletes in the buffer, `:q` and
`:reload` refuse with `no write since last change`, and `:q!` and `:reload!`
discard the deletes and go ahead. On every other command the `!` is accepted
and means nothing, so old muscle memory stays harmless.

**`:edit` is the agent, not vi's re-edit.** In `vivi` the agent is the editing
mechanism, so `:edit <prompt>` is how you change the file, and the command that
re-reads it from disk is `:reload`. There is no short alias for `:reload`, and
`:e` is bound to `:edit`, so vi's habit of typing `:e` to reload does not
silently do the wrong thing — it asks what you want the agent to do.

An unknown command reports `not an editor command: <name>`, and a missing
argument reports `argument required: <how to spell it>`. Vi's numbered codes
(`E492`, `E471`) are deliberately not used: the number is a key into a help
system `vivi` does not have, and the sentence was always the part you read.

The command set is defined by one table in `editor/cmd.rs`; `:help` and this
page are both checked against it by tests, so neither can describe a command
that does not exist.

## Ranges

Any command may be prefixed with a range. `:edit` and `:delete` are the ones
that use it.

| Range | Means |
| --- | --- |
| *(none)* | the line the cursor is on |
| `%` | the whole file |
| `'<,'>` | the last visual selection |
| `10,20` | lines 10 to 20 |
| `.` | the current line |
| `$` | the last line |
| `3` | just line 3 |

Addresses may be mixed — `:.,$edit tidy the rest`, `:10,$edit`, `:'<,'>edit`.
Reversed ranges are sorted, and addresses past the end of the file are clamped
to the last line. A malformed range reports `invalid range`.

Pressing `:` while a visual selection is up prefills the command line with
`'<,'>` and keeps the selection highlighted while you type.

## Asking an agent

    :edit make this async
    :'<,'>edit rename this to `render`
    :10,20edit rewrite this without the clone
    :%edit add doc comments to every public function

The usual flow is `V`, `j` `j`, `:edit make this async`.

### What the agent receives

The prompt is passed as a command-line argument. The selected lines go to the
agent's stdin, fenced and labelled:

    Here are lines 10-20 of src/main.rs:

    ```
    fn main() {
    ...
    ```

### Which agent runs

`$VIVI_AGENT` wins if set; otherwise `PATH` is probed for `claude`, then
`codex`.

| Agent | Invoked as |
| --- | --- |
| `claude` | `claude -p <prompt>` |
| `codex` | `codex exec <prompt>` |

`$VIVI_AGENT` may be a bare name, a path, or a command with leading arguments:

    VIVI_AGENT=codex vivi main.rs
    VIVI_AGENT="claude --model opus" vivi main.rs

It still has to name a CLI `vivi` knows how to drive non-interactively — an
unknown program is rejected (`unsupported agent: mytool`) rather than guessed
at, since there is no prompt flag to invent.

### While it works

One agent runs at a time; asking again reports `an agent is still working`. The
status line carries `⠇ claude` — a spinner and the agent's own name — until it
finishes, and the editor stays responsive throughout: the subprocess runs on its
own thread and its stdout is streamed back line by line.

When it finishes, the file is re-read from disk, so any edits it made are
already on screen:

    claude finished, file reloaded · ^Ww to read

and the indicator settles to `✓ claude` or `✗ claude`. A failing agent reports its
exit status and the last line of its stderr — which is how you find out you are
not logged in.

## Panes

There are two kinds, and they are different on purpose.

**The agent's reply is a split.** `Ctrl-W w` (or `:output`) opens it across the
bottom half, with the file still above it — you are reading what the agent said
*about* that code, so both stay in view. A dim rule heads it with the agent and
the question you asked:

    ── claude · make this async ─────────────────────────────

It does not say whether it is still thinking; the status line already does, and
saying it twice is just saying it twice.

**`:help` and `:messages` take the whole screen.** They are documents you read
and then dismiss, and there is no reason to keep the file in view while you read
a reference. Half a screen of each would be worse than all of one. They carry a
single dim title and nothing else — no rule, no state, none of the agent pane's
furniture.

Both scroll the same way:

| Key | Does |
| --- | --- |
| `j` / `k`, `↓` / `↑` | scroll a line |
| `Ctrl-D` / `Ctrl-U` | scroll half a screen |
| `g` / `G` | top / bottom |
| `q` / `Esc` | close |

vi's window commands drive the split, and only the split — a document is not a
window:

| Key | Does |
| --- | --- |
| `Ctrl-W w` | move between the buffer and the agent pane, either way |
| `Ctrl-W c` | close the agent pane |

`Ctrl-W Ctrl-W` works too, as in vi. In `:help` and `:messages`, `q` and `Esc`
are the way out.

Long lines wrap rather than clip — the whole point of `:messages` is reading an
error too long for the status line, and a pane that clipped at the same width
would be no better.

The agent pane follows the tail while output is arriving; scrolling up stops it
following, and scrolling back to the bottom resumes. Closing it keeps the
output, so `Ctrl-W w` brings it back.

## Jumping to definitions

| Key / command | Does |
| --- | --- |
| `gd` | jump to the definition of the symbol under the cursor |
| `Ctrl-]` | the same, on terminals and layouts that can send it |
| `:definition` / `:def` | the same, as a command |
| `Ctrl-T` | unwind one level back |
| `:pop` | same as `Ctrl-T` |
| `:tag <file>[:<line>[:<col>]]` | jump straight to a place, no language server involved |
| `:jumps` | how deep the stack is, and where the last jump came from |

**`gd` is the jump key.** It is two ordinary letters, so it survives every
keyboard layout and terminal.

`Ctrl-]` is bound as well, and is accepted whether the terminal reports it as
`Ctrl-]` or as the control byte `0x1D` that most terminals send (which decodes
as `Ctrl-5`). But it cannot be relied on: on a Nordic layout `]` is AltGr+9, and
a terminal may collapse Ctrl+AltGr+9 into a bare `9` that never reaches the
program as a chord at all. When a chord appears to do nothing, that is usually
why — and `gd` is the way through.

Jumps from `gd`, `Ctrl-]` and `:tag` share one stack, so `Ctrl-T` unwinds any of
them. Line and column in `:tag` are 1-based and both optional:
`:tag src/lsp.rs:180`.

### When a jump fails

The language server has to index the project before it can answer anything —
six seconds or so for a small crate, from cold. It is started when the editor
is, not on your first jump, so those seconds are spent while you are reading
rather than while you are waiting. `⠹ symbols` on the status line means it is
still working; `✓ symbols` means a jump will be answered at once. A file with no
language server we know of starts nothing and says nothing.

If you do jump before it is ready, `vivi` keeps asking in the background — about
twice a second, for up to roughly half a minute — with `⠹ symbols` on the status
line until an answer arrives, so the jump lands as soon as it can. An indexing
run that finishes retriggers the pending lookup immediately.

Failures name their cause rather than collapsing into "not found":

| Message | Means |
| --- | --- |
| `no symbol under the cursor` | the cursor is on punctuation or whitespace |
| `no language server known for .xyz files` | unknown extension; set `VIVI_LSP` |
| `rust-analyzer not found on PATH` | the server is not installed |
| `rust-analyzer exited with status 1: …` | the server died, quoting its stderr |
| `rust-analyzer never finished starting` | no reply to `initialize` |
| `rust-analyzer is still indexing after 60 tries` | give it a moment and retry |
| `no definition for foo` | the server answered, and there is nothing there |
| `language server unavailable — :messages` | it failed to start earlier; the reason is in the log |

A jump that does not land leaves `✗ symbols` on the status line. The two that are
the server's own account rather than yours — it died, or it never answered
`initialize` — are longer than a status line can hold, so they go to
`:messages` in full instead of being truncated where you are looking.

## When something looks broken

Nothing in `vivi` fails silently.

**Red is for things that went wrong, not for keys that had nothing to act on.**
These are all dimmed, not red, because none of them is a failure:

| Message | When |
| --- | --- |
| `nothing to go back to` | `Ctrl-T` with an empty tag stack |
| `no output to show` | `Ctrl-W w` or `:output` with no pane |
| `no symbol under the cursor` | `gd` on whitespace or punctuation |
| `an agent is still working` | `:edit` or a delete while one is running |

Red is kept for mistakes (`not an editor command`, `argument required`) and for
things that actually broke (a file that will not open, a language server that
died). Every one of them is a variant of `ViviError` in `error.rs`, so the same
failure reads the same way wherever it surfaces.

| | |
| --- | --- |
| an error message | stays on the status line until replaced; `Esc` dismisses |
| `:messages` | every message so far, errors marked `E`, full screen and wrapped |
| `:help` | every key and command that exists |
| `:lsp` | the language server's state and its last complaint |

An unbound key does nothing and says nothing; `:help` lists the ones that do.

`:messages` matters because failures can arrive seconds after the keypress that
caused them — a jump that fails once the language server finishes indexing, an
agent that dies while you are scrolling. Those land on the status line whenever
they happen, and get typed over. The log keeps them.

## Language servers

`$VIVI_LSP` overrides everything; otherwise the server is chosen by extension.

| Extension | Server |
| --- | --- |
| `.rs` | `rust-analyzer` |

`rust-analyzer` is the only server chosen automatically, because it is the only
one that has been tested. Every other language needs `$VIVI_LSP`.

    VIVI_LSP="rust-analyzer" vivi main.rs

Like `$VIVI_AGENT`, it may be a bare name, a path, or a command with arguments.

**Project root.** The server is started in the outermost directory containing a
`Cargo.toml`, or failing that the directory containing `.git`, so a workspace
member resolves against the whole workspace rather than just its own crate.

**Position encoding.** Columns are negotiated at `initialize`: UTF-8 byte
offsets where the server supports them, UTF-16 code units otherwise. Cursors
land correctly either way, including on lines containing emoji or accented
characters.

**Lifecycle.** The server is started when the editor is, as soon as it knows
which one to run for the file you opened, and told about every file you open or
jump into afterwards. A file type with no server it knows of starts nothing and
says nothing — the complaint waits until you actually ask for a definition. It
is shut down (and then killed, for the ones that do not take the hint) when the
editor exits. Its stderr is captured rather than inherited — it would otherwise
scribble over the screen — and the tail of it is what failure messages quote.

`:lsp` reports the current state — `starting`, `indexing`, `ready`, or how it
exited — with the position encoding, the project root, and the server's last
complaint if it has made one:

    rust-analyzer · ready · utf-8 · root /Users/me/src/vivi

When nothing is running, because none is known for this file type or because
starting it failed, `:lsp` says that instead:

    not started; Ctrl-] would run /usr/local/bin/rust-analyzer
    no language server known for .txt files

## Reloading

The file is re-read whenever its modification time or size changes on disk,
whoever wrote it. Stat polling is used rather than inode watching, because most
tools save by writing a temporary file and renaming it over the original.

The cursor is kept where it can still go — clamped to the new last line and to
the new length of its line — and the status line says `"main.rs" reloaded`.
`:reload` forces a re-read.

With unwritten deletes in the buffer the re-read would silently discard them,
so it does not happen: the status line says `file changed on disk — :reload!
takes it, :w overwrites it`, once, and the choice is yours. `:reload` alone
refuses for the same reason the automatic reload did.

## The status line

The status line is one row, always the last one, with two columns of air at each
end. It is built from segments in a fixed order, and the first is never given
up:

    src/main.rs:42  3 lines      ✓ symbols  ⠧ claude

| Segment | Side | When |
| --- | --- | --- |
| `src/main.rs:42` | left | always — the file you are in and the line you are on |
| `[+]` | left | the buffer holds deletes not yet written with `:w`, in bold |
| `3 lines` | left | while a selection is up, in bold |
| a message | left | until something replaces it; errors in the terminal's own red |
| `⠹ symbols` | right | the language server is starting, indexing, or answering a jump |
| `✓ symbols` | right | ready, so `gd` is answered at once |
| `✗ symbols` | right | the last jump failed on the server's account |
| `⠧ claude` | right | an agent is working, named as it was invoked |
| `✓ claude` / `✗ claude` | right | its last run finished, or failed |

Background work is pinned to the right so it holds still: on the left it would
shuffle sideways every time a message came or went.

Each indicator keeps its label and swaps only the mark in front of it: a spinner
while it turns, then a green `✓` or a red `✗`. The colour carries the state and
the mark repeats it, so it still reads with colours turned off. The language
server's slot is labelled `symbols` — the word editors already use for this
("go to symbol", symbol search, and LSP's own `workspace/symbol` request), so it
needs no explaining. It names the thing all three states are about: building the
symbol index, having it, or failing to. `definitions` would name only what
`vivi` currently asks for, and would be wrong the moment references or hover
arrive; `lsp` names a wire protocol; `index` begs the question "of what". It is
a plural noun rather than a verb so it reads the same in every state, where
`✓ indexing` would be tense-mismatched.

The command line is not part of the status line. Typing `:` opens its own box
above it, and the status line keeps its row unchanged; see [Modes](#modes).

Messages are appended, never swapped in for the filename — losing it was how a
message could leave you unsure which file you were even looking at.

While an agent works, **the lines it was given stay marked** in dimmed reverse
video, weaker than a live selection but clear enough to see what it is holding.

Informational messages clear on the next keypress. **Errors do not** — they stay
until something replaces them, because a message you cannot read is the same as
no message at all. `Esc` dismisses one.

## How text is displayed

- Tabs expand to 8-column stops for display; the file is untouched.
- `~` marks lines past the end of the buffer.
- Long lines scroll horizontally, by the minimum needed to keep the cursor on
  screen. So does vertical scrolling.
- CRLF line endings are read; a trailing newline terminates the last line rather
  than starting an empty one.
- No line numbers and no syntax highlighting.
- No colours of `vivi`'s own: nothing paints a background, and the only
  foregrounds are the terminal's own red for errors and the red and green of the
  `✗` and `✓` marks. Everything secondary is dimmed instead, so it reads on
  light and dark terminals alike.

## Limitations

- No insert mode, and no undo — `:reload!` is the way back to what is on
  disk. The only edits are the agent's and deleting lines.
- One file at a time, named on the command line — no buffer list, and no empty
  unnamed buffer. A jump replaces the buffer; the tag stack is how you get back.
  The only split is the agent's pane, and it is not another file.
- One agent job at a time.
- Only `claude` and `codex` are known agent CLIs.
- No search (`/`), no marks beyond `'<` and `'>`, no counts before motions.
- Jump-to-definition only. No references, hover, completion, diagnostics or
  rename, though the language server could answer all of them.
- `Ctrl-]` depends on the terminal and keyboard layout; `gd` does not.
