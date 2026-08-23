//! Text measured the way a terminal draws it: in columns, not bytes.

pub const TAB_WIDTH: usize = 8;

/// The last column the cursor may rest on, vim's normal-mode rule.
pub fn last_col(line: &str) -> usize {
    line.chars().count().saturating_sub(1)
}

pub fn first_non_blank(line: &str) -> usize {
    line.chars().position(|c| !c.is_whitespace()).unwrap_or(0)
}

pub fn byte_index(line: &str, col: usize) -> usize {
    line.char_indices().nth(col).map(|(i, _)| i).unwrap_or(line.len())
}

pub fn expand_tabs(line: &str) -> String {
    if !line.contains('\t') {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len());
    for c in line.chars() {
        if c == '\t' {
            let pad = TAB_WIDTH - display_width(&out) % TAB_WIDTH;
            out.extend(std::iter::repeat_n(' ', pad));
        } else {
            out.push(c);
        }
    }
    out
}

pub fn display_width(text: &str) -> usize {
    text.chars().fold(0, |w, c| if c == '\t' { w + TAB_WIDTH - w % TAB_WIDTH } else { w + 1 })
}

/// The identifier the cursor is sitting on, if any.
pub fn symbol_at(line: &str, col: usize) -> String {
    let chars: Vec<char> = line.chars().collect();
    if chars.get(col).is_none_or(|c| char_class(*c) != 1) {
        return String::new();
    }
    let start = chars[..col].iter().rposition(|c| char_class(*c) != 1).map_or(0, |i| i + 1);
    let end =
        chars[col..].iter().position(|c| char_class(*c) != 1).map_or(chars.len(), |i| col + i);
    chars[start..end].iter().collect()
}

/// Word motions treat keyword characters, punctuation and blanks as separate classes.
pub fn char_class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

/// Convert a cursor column to the offset an LSP server expects, in whichever
/// units it negotiated. `col` counts characters; LSP counts bytes or UTF-16
/// code units, and for ASCII all three happen to agree — which is exactly why
/// getting this wrong stays invisible until someone opens a file with an emoji.
pub fn to_lsp_character(line: &str, col: usize, utf8: bool) -> usize {
    let prefix: String = line.chars().take(col).collect();
    if utf8 {
        prefix.len()
    } else {
        prefix.chars().map(char::len_utf16).sum()
    }
}

/// The inverse: an LSP column back to a character column we can put a cursor on.
pub fn from_lsp_character(line: &str, character: usize, utf8: bool) -> usize {
    let mut used = 0;
    for (col, c) in line.chars().enumerate() {
        if used >= character {
            return col;
        }
        used += if utf8 { c.len_utf8() } else { c.len_utf16() };
    }
    line.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_symbol_under_the_cursor_is_the_whole_identifier() {
        let line = "    let x = foo_bar(baz);";
        assert_eq!(symbol_at(line, 12), "foo_bar", "from the start of the name");
        assert_eq!(symbol_at(line, 15), "foo_bar", "from the middle");
        assert_eq!(symbol_at(line, 18), "foo_bar", "from the last character");
        assert_eq!(symbol_at(line, 19), "", "punctuation is not a symbol");
        assert_eq!(symbol_at(line, 0), "", "nor is whitespace");
        assert_eq!(symbol_at(line, 20), "baz");
        assert_eq!(symbol_at(line, 99), "", "past the end of the line");
    }

    #[test]
    fn lsp_columns_round_trip_through_both_encodings() {
        // A 4-byte character: 1 char, 4 UTF-8 bytes, 2 UTF-16 units.
        let line = "a\u{1F600}b";
        for (utf8, expected) in [(true, [0, 1, 5, 6]), (false, [0, 1, 3, 4])] {
            let got: Vec<_> = (0..4).map(|c| to_lsp_character(line, c, utf8)).collect();
            assert_eq!(got, expected, "utf8={utf8}");
            for col in 0..4 {
                assert_eq!(
                    from_lsp_character(line, to_lsp_character(line, col, utf8), utf8),
                    col,
                    "round trip at {col} with utf8={utf8}"
                );
            }
        }
    }

    #[test]
    fn tabs_expand_to_stops() {
        assert_eq!(expand_tabs("\tx"), "        x");
        assert_eq!(expand_tabs("ab\tx"), "ab      x");
        assert_eq!(display_width("ab\t"), 8);
    }
}
