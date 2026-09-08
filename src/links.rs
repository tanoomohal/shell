//! URL detection in terminal text.

use std::ops::Range;

/// Schemes we are willing to hand to the OS opener.
///
/// Deliberately narrow. Terminal output is untrusted — it can contain anything
/// a remote server or a program chose to print — and `file://` or a custom
/// scheme registered by some other app is an easy way to make a click do
/// something the user did not intend.
const SCHEMES: &[&str] = &["https://", "http://"];

/// Characters that end a URL.
fn is_boundary(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || matches!(c, '"' | '\'' | '<' | '>' | '`' | '|' | '\u{2502}' | '\u{00a0}')
}

/// Trailing characters that are far more often sentence punctuation than part
/// of the address.
const TRAILING: &[char] = &['.', ',', ';', ':', '!', '?', ']', '}', '\'', '"', '*'];

fn matches_at(chars: &[char], at: usize, needle: &str) -> bool {
    needle
        .chars()
        .enumerate()
        .all(|(i, c)| chars.get(at + i).is_some_and(|x| x.eq_ignore_ascii_case(&c)))
}

/// Finds a URL covering `column` in a single row of terminal text.
///
/// Returns the column range it occupies and the URL itself. Columns are
/// character offsets, matching how a grid row is addressed.
pub fn find_url_at(line: &str, column: usize) -> Option<(Range<usize>, String)> {
    let chars: Vec<char> = line.chars().collect();

    for start in 0..chars.len() {
        let Some(scheme) = SCHEMES.iter().find(|s| matches_at(&chars, start, s)) else {
            continue;
        };

        let scheme_len = scheme.chars().count();
        let mut end = start + scheme_len;
        while end < chars.len() && !is_boundary(chars[end]) {
            end += 1;
        }

        // Trim trailing punctuation, and a closing paren only when nothing
        // opened it — plenty of real URLs contain balanced parentheses.
        while end > start + scheme_len {
            let last = chars[end - 1];
            let unbalanced_paren =
                last == ')' && !chars[start..end].contains(&'(');
            if TRAILING.contains(&last) || unbalanced_paren {
                end -= 1;
            } else {
                break;
            }
        }

        // A bare scheme with no host is not a link.
        if end <= start + scheme_len {
            continue;
        }

        if column >= start && column < end {
            return Some((start..end, chars[start..end].iter().collect()));
        }
    }

    None
}

/// Hands a URL to the platform opener.
pub fn open(url: &str) {
    // Re-check the scheme: this is the last gate before handing untrusted
    // terminal content to the OS.
    if !SCHEMES.iter().any(|s| url.to_ascii_lowercase().starts_with(s)) {
        log::warn!("refusing to open non-http url");
        return;
    }
    if url.chars().any(char::is_control) {
        log::warn!("refusing to open url containing control characters");
        return;
    }

    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    match std::process::Command::new(program).arg(url).spawn() {
        Ok(_) => {},
        Err(err) => log::warn!("failed to open url: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_plain_url() {
        let line = "see https://example.com/docs for details";
        let (range, url) = find_url_at(line, 6).expect("no url found");
        assert_eq!(url, "https://example.com/docs");
        assert_eq!(range, 4..28);
    }

    #[test]
    fn returns_none_away_from_the_url() {
        let line = "see https://example.com now";
        assert!(find_url_at(line, 0).is_none());
        assert!(find_url_at(line, 25).is_none());
    }

    #[test]
    fn trims_sentence_punctuation() {
        let (_, url) = find_url_at("go to https://example.com/a.", 10).unwrap();
        assert_eq!(url, "https://example.com/a");
        let (_, url) = find_url_at("(see https://example.com/b),", 10).unwrap();
        assert_eq!(url, "https://example.com/b");
    }

    #[test]
    fn keeps_balanced_parentheses() {
        let (_, url) =
            find_url_at("https://en.wikipedia.org/wiki/Shell_(computing)", 5).unwrap();
        assert_eq!(url, "https://en.wikipedia.org/wiki/Shell_(computing)");
    }

    #[test]
    fn stops_at_quotes_and_box_drawing() {
        let (_, url) = find_url_at("\"https://example.com/x\"", 5).unwrap();
        assert_eq!(url, "https://example.com/x");
        let (_, url) = find_url_at("\u{2502} https://example.com/y \u{2502}", 5).unwrap();
        assert_eq!(url, "https://example.com/y");
    }

    #[test]
    fn ignores_other_schemes() {
        assert!(find_url_at("file:///etc/passwd", 4).is_none());
        assert!(find_url_at("ftp://example.com", 4).is_none());
        // A scheme-like custom handler must not be clickable.
        assert!(find_url_at("myapp://do-something", 4).is_none());
    }

    #[test]
    fn ignores_a_bare_scheme() {
        assert!(find_url_at("https://", 3).is_none());
    }

    #[test]
    fn is_case_insensitive_about_the_scheme() {
        let (_, url) = find_url_at("HTTPS://Example.COM/z", 3).unwrap();
        assert_eq!(url, "HTTPS://Example.COM/z");
    }
}
