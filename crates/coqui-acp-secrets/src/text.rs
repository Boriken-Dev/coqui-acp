//! Text the person reads in a prompt: nothing in it may change how it looks.

/// A character that can hide or reorder what is around it: controls (Cc), format characters (Cf), line and paragraph separators
/// (Zl, Zp), and whitespace that is not a plain space.
pub(crate) fn suspect(c: char) -> bool {
    c.is_control()
        || (c.is_whitespace() && c != ' ')
        || matches!(
            u32::from(c),
            0xAD | 0x600..=0x605
                | 0x61C
                | 0x6DD
                | 0x70F
                | 0x8E2
                | 0x180E
                | 0x200B..=0x200F
                | 0x2028..=0x202E
                | 0x2060..=0x206F
                | 0xFEFF
                | 0xFFF9..=0xFFFB
                | 0x110BD
                | 0x1BCA0..=0x1BCA3
                | 0x1D173..=0x1D17A
                | 0xE0001
                | 0xE0020..=0xE007F
        )
}

/// The longest run of plain spaces shown as spaces; a longer run could push text out of view.
const MAX_SPACES: usize = 3;

/// Whether `text` has a [`suspect`] character or a long run of spaces.
pub(crate) fn has_hidden(text: &str) -> bool {
    text.chars().any(suspect) || text.contains(&" ".repeat(MAX_SPACES + 1))
}

/// `text` on one line for a prompt: a [`suspect`] character is written as `\u{..}` (or `\n`, `\r`, `\t`), a run of more than
/// three spaces as `<N spaces>`, and with `quote` a `'` as `'\''`.
pub(crate) fn escape(text: &str, quote: bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut spaces = 0usize;
    let flush = |out: &mut String, spaces: &mut usize| {
        if *spaces > MAX_SPACES {
            out.push_str(&format!("<{} spaces>", *spaces));
        } else {
            out.push_str(&" ".repeat(*spaces));
        }
        *spaces = 0;
    };
    for c in text.chars() {
        if c == ' ' {
            spaces += 1;
            continue;
        }
        flush(&mut out, &mut spaces);
        match c {
            '\'' if quote => out.push_str("'\\''"),
            '\\' => out.push_str("\\\\"),
            '<' => out.push_str("\\<"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if suspect(c) => out.push_str(&format!("\\u{{{:x}}}", u32::from(c))),
            c => out.push(c),
        }
    }
    flush(&mut out, &mut spaces);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_characters_and_long_runs_of_spaces_are_written_out() {
        assert_eq!(escape("a\u{200b}b", false), "a\\u{200b}b");
        assert_eq!(
            escape("a\u{2028}b\u{a0}c\u{2003}d", false),
            "a\\u{2028}b\\u{a0}c\\u{2003}d"
        );
        assert_eq!(
            escape("a\u{feff}b\u{e0041}", false),
            "a\\u{feff}b\\u{e0041}"
        );
        assert_eq!(escape("a   b", false), "a   b");
        assert_eq!(escape("a    b", false), "a<4 spaces>b");
        assert!(has_hidden("x\u{200e}") && has_hidden("a    b") && !has_hidden("a b"));
    }
}
