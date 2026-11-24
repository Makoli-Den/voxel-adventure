//! Making text that comes from a mod safe to print: bounded length and no control characters.
//!
//! Log lines and error messages of a mod end up in the game's console. Without this, a mod
//! could start a fake line that looks like the game's or another mod's (`"\n[base] ..."`),
//! or send terminal escape sequences.

/// The longest UTF-8 encoding of one character, in bytes.
pub(crate) const MAX_UTF8_CHAR_BYTES: usize = 4;

/// Replaces control characters except tab with a space and cuts the result to at most
/// `limit` bytes without splitting a character. Newlines become spaces too, so a message is
/// always one line.
pub(crate) fn printable(text: &str, limit: usize) -> String {
    let mut clean: String = text
        .chars()
        .map(|c| if c.is_control() && c != '\t' { ' ' } else { c })
        .collect();
    truncate_to_char_boundary(&mut clean, limit);
    clean
}

/// Cuts `text` to at most `limit` bytes without splitting a character.
pub(crate) fn truncate_to_char_boundary(text: &mut String, limit: usize) {
    if text.len() <= limit {
        return;
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_respects_char_boundaries() {
        let mut text = "a\u{e9}b".to_owned(); // 'é' is two bytes, at 1..3
        truncate_to_char_boundary(&mut text, 2);
        assert_eq!(text, "a");
        let mut short = "abc".to_owned();
        truncate_to_char_boundary(&mut short, 10);
        assert_eq!(short, "abc");
    }

    #[test]
    fn control_characters_become_spaces_except_tab() {
        assert_eq!(
            printable("ok\n[base] fake\r\x1b[31mred\u{85}\tend", 100),
            "ok [base] fake  [31mred \tend"
        );
    }

    #[test]
    fn printable_cuts_after_cleaning() {
        assert_eq!(printable("\u{e9}\u{e9}", 3), "\u{e9}");
    }
}
