//! One scanner over PL/pgSQL body text, shared by every transform in this
//! module.
//!
//! The transforms all need the same question answered: is this byte offset
//! inside a string literal or a comment, or is it live text? Four separate
//! answers to it, three of them wrong about the `''` escape, are what produced
//! the family of defects R52 to R55.

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use alloc::{
    borrow::ToOwned,
    boxed::Box,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};

/// What the scanner is inside at a given offset.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Region {
    /// Live text, where a keyword is a keyword.
    Code,
    /// Anything quoted or commented, where it is not.
    Quoted,
}

/// Walks `text` once, reporting the region each byte belongs to.
///
/// Handles single-quoted strings with the `''` escape and with backslash
/// escapes, double-quoted identifiers, dollar-quoted strings with arbitrary
/// tags, `--` line comments, and `/* */` block comments. The delimiters
/// themselves count as [`Region::Quoted`], so a scan for a keyword can simply
/// skip every quoted byte.
pub(crate) struct Scanner<'a> {
    text: &'a str,
}

impl<'a> Scanner<'a> {
    pub(crate) fn new(text: &'a str) -> Self {
        Self { text }
    }

    /// The region of every byte offset in the text, as a parallel map.
    pub(crate) fn regions(&self) -> Vec<Region> {
        let bytes = self.text.as_bytes();
        let mut regions = vec![Region::Code; bytes.len()];
        let mut index = 0;

        while index < bytes.len() {
            let rest = &self.text[index..];
            let span = if let Some(after) = rest.strip_prefix("--") {
                after.find('\n').map_or(rest.len(), |end| end + 3)
            } else if let Some(after) = rest.strip_prefix("/*") {
                after.find("*/").map_or(rest.len(), |end| end + 4)
            } else if bytes[index] == b'\'' {
                single_quoted_len(rest)
            } else if bytes[index] == b'"' {
                double_quoted_len(rest)
            } else if let Some(len) = dollar_quoted_len(rest) {
                len
            } else {
                index += 1;
                continue;
            };

            for region in regions.iter_mut().skip(index).take(span) {
                *region = Region::Quoted;
            }
            index += span;
        }

        regions
    }

    /// The offset of the first `needle` byte that is live code.
    pub(crate) fn find_in_code(&self, needle: u8) -> Option<usize> {
        let regions = self.regions();
        self.text
            .bytes()
            .zip(regions)
            .position(|(byte, region)| byte == needle && region == Region::Code)
    }

    /// The offset of `keyword` as a whole word in live code, searched from
    /// `from`, comparing case-insensitively.
    ///
    /// Both sides need a word boundary, which is what stops `myelsif` from
    /// matching `elsif`.
    pub(crate) fn find_keyword(&self, keyword: &str, from: usize) -> Option<usize> {
        let haystack = self.text.as_bytes();
        let needle = keyword.as_bytes();
        if needle.is_empty() || needle.len() > haystack.len() {
            return None;
        }
        let regions = self.regions();

        (from..=haystack.len() - needle.len()).find(|&start| {
            regions.get(start).copied() != Some(Region::Quoted)
                && haystack[start..start + needle.len()].eq_ignore_ascii_case(needle)
                && !is_word_byte(start.checked_sub(1).map(|before| haystack[before]))
                && !is_word_byte(haystack.get(start + needle.len()).copied())
        })
    }

    /// The next word of live code at or after `from`, skipping whitespace and
    /// anything quoted or commented.
    pub(crate) fn next_word_in_code(&self, from: usize) -> Option<&'a str> {
        let regions = self.regions();
        let bytes = self.text.as_bytes();
        let start = (from..bytes.len())
            .find(|&index| regions[index] == Region::Code && !bytes[index].is_ascii_whitespace())?;
        let end = (start..bytes.len())
            .find(|&index| regions[index] != Region::Code || !is_word_byte(Some(bytes[index])))
            .unwrap_or(bytes.len());
        (end > start).then(|| &self.text[start..end])
    }

    /// Split on every `separator` byte that is live code.
    pub(crate) fn split_in_code(&self, separator: u8) -> Vec<&'a str> {
        let regions = self.regions();
        let mut pieces = Vec::new();
        let mut start = 0;
        for (index, (byte, region)) in self.text.bytes().zip(regions).enumerate() {
            if byte == separator && region == Region::Code {
                pieces.push(&self.text[start..index]);
                start = index + 1;
            }
        }
        pieces.push(&self.text[start..]);
        pieces
    }
}

/// True for a byte that can appear inside an identifier.
fn is_word_byte(byte: Option<u8>) -> bool {
    byte.is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$')
}

/// Length of the single-quoted string starting at the front of `rest`,
/// including both quotes.
///
/// A doubled quote is an escaped quote and stays inside, which is the case
/// three of the four replaced scanners got wrong. A backslash escape is honored
/// too, for `E'...'` strings.
fn single_quoted_len(rest: &str) -> usize {
    let bytes = rest.as_bytes();
    let mut index = 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'\'' if bytes.get(index + 1) == Some(&b'\'') => index += 2,
            b'\'' => return index + 1,
            _ => index += 1,
        }
    }
    rest.len()
}

/// Length of the double-quoted identifier starting at the front of `rest`.
fn double_quoted_len(rest: &str) -> usize {
    let bytes = rest.as_bytes();
    let mut index = 1;
    while index < bytes.len() {
        match bytes[index] {
            b'"' if bytes.get(index + 1) == Some(&b'"') => index += 2,
            b'"' => return index + 1,
            _ => index += 1,
        }
    }
    rest.len()
}

/// Length of the dollar-quoted string starting at the front of `rest`, or
/// `None` when the `$` does not open one.
///
/// The tag is everything between the opening `$` and the next `$`, and it must
/// be a valid identifier or empty, so `$1` is a placeholder rather than a tag.
fn dollar_quoted_len(rest: &str) -> Option<usize> {
    let after_first = rest.strip_prefix('$')?;
    let tag_end = after_first.find('$')?;
    let tag = &after_first[..tag_end];
    if !tag.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    if tag.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }

    let opener_len = tag_end + 2;
    let delimiter = &rest[..opener_len];
    rest[opener_len..]
        .find(delimiter)
        .map_or(Some(rest.len()), |end| Some(opener_len + end + delimiter.len()))
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::{Region, Scanner};

    fn regions_of(text: &str) -> Vec<Region> {
        Scanner::new(text).regions()
    }

    #[test]
    fn a_doubled_quote_does_not_close_the_string() {
        let text = "a 'it''s' b";
        let regions = regions_of(text);
        assert_eq!(regions[2], Region::Quoted, "the opening quote");
        assert_eq!(regions[8], Region::Quoted, "the closing quote");
        assert_eq!(regions[10], Region::Code, "the b after it");
    }

    #[test]
    fn a_dollar_quote_spans_its_tag() {
        let text = "x $tag$ ; $tag$ y";
        assert_eq!(Scanner::new(text).find_in_code(b';'), None);
        assert_eq!(regions_of(text)[16], Region::Code);
    }

    #[test]
    fn a_placeholder_is_not_a_dollar_quote() {
        let text = "$1 ; $2";
        assert_eq!(Scanner::new(text).find_in_code(b';'), Some(3));
    }

    #[test]
    fn a_keyword_needs_a_boundary_on_both_sides() {
        assert_eq!(Scanner::new("myelsif x").find_keyword("elsif", 0), None);
        assert_eq!(Scanner::new("elsifx x").find_keyword("elsif", 0), None);
        assert_eq!(Scanner::new("a ELSIF b").find_keyword("elsif", 0), Some(2));
    }

    #[test]
    fn a_keyword_inside_a_comment_is_not_found() {
        assert_eq!(Scanner::new("-- elsif\nx").find_keyword("elsif", 0), None);
        assert_eq!(Scanner::new("/* elsif */ x").find_keyword("elsif", 0), None);
    }

    /// A haystack shorter than the keyword has no match and must not index
    /// past its end.
    #[test]
    fn the_next_word_skips_whitespace_and_comments() {
        let scanner = Scanner::new("EXCEPTION -- note\n  WHEN x");
        assert_eq!(scanner.next_word_in_code(9), Some("WHEN"));
        assert_eq!(Scanner::new("EXCEPTION").next_word_in_code(9), None);
    }

    #[test]
    fn a_short_text_has_no_keyword() {
        assert_eq!(Scanner::new("x, y").find_keyword("DEFAULT", 0), None);
        assert_eq!(Scanner::new("").find_keyword("BEGIN", 0), None);
    }

    #[test]
    fn splitting_ignores_separators_inside_quotes() {
        let pieces = Scanner::new("a := 'x;y'; b := 2").split_in_code(b';');
        assert_eq!(pieces, vec!["a := 'x;y'", " b := 2"]);
    }
}
