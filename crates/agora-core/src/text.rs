//! Small text helpers shared by notifications and connectors.

/// Case-insensitive whole-word match ("urgent" matches "URGENT:" but not "urgently").
pub fn contains_word(haystack: &str, needle: &str) -> bool {
    let needle = needle.trim().to_lowercase();
    if needle.is_empty() {
        return false;
    }
    let hay = haystack.to_lowercase();
    hay.match_indices(&needle).any(|(i, _)| {
        let before = hay[..i].chars().next_back();
        let after = hay[i + needle.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

/// Single-line plain-text preview of a Markdown message, at most `max` characters.
pub fn preview(md: &str, max: usize) -> String {
    let mut out = String::new();
    for word in md.split_whitespace() {
        let word = word.trim_matches(|c| matches!(c, '*' | '_' | '~' | '`' | '>'));
        if word.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    if out.chars().count() > max {
        let cut: String = out.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", cut.trim_end())
    } else {
        out
    }
}

/// Standard base64 with padding (for small data URLs such as account logos).
pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_padding() {
        assert_eq!(super::base64(b"Man"), "TWFu");
        assert_eq!(super::base64(b"Ma"), "TWE=");
        assert_eq!(super::base64(b"M"), "TQ==");
        assert_eq!(super::base64(b""), "");
    }

    use super::*;

    #[test]
    fn word_boundaries() {
        assert!(contains_word("This is URGENT: fix", "urgent"));
        assert!(contains_word("urgent", "Urgent"));
        assert!(!contains_word("urgently", "urgent"));
        assert!(contains_word("ping Nico.", "nico"));
        assert!(!contains_word("anything", ""));
        assert!(contains_word("è urgente!", "urgente"));
    }

    #[test]
    fn previews() {
        assert_eq!(preview("**hello**\n\n`world`", 50), "hello world");
        assert_eq!(preview("abcdef ghij", 6), "abcde…");
    }
}
