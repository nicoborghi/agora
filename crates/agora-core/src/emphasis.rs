//! Single-character emphasis (Rocket.Chat and Slack: `*bold*`, `~strike~`) to and
//! from standard Markdown (`**bold**`, `~~strike~~`, `*italic*` = `_italic_`).
//! Code spans and fenced blocks are never rewritten.

/// Splits `md` into (is_code, text) parts: fenced blocks and inline code spans are code.
pub fn segments(md: &str) -> Vec<(bool, &str)> {
    let mut out = Vec::new();
    let mut rest = md;
    while let Some(t) = rest.find('`') {
        if t > 0 {
            out.push((false, &rest[..t]));
        }
        let after = &rest[t..];
        let delim = if after.starts_with("```") { "```" } else { "`" };
        match after[delim.len()..].find(delim) {
            Some(end) => {
                let stop = delim.len() * 2 + end;
                out.push((true, &after[..stop]));
                rest = &after[stop..];
            }
            None => {
                out.push((true, after));
                rest = "";
            }
        }
    }
    if !rest.is_empty() {
        out.push((false, rest));
    }
    out
}

/// Applies `f` to the text outside code.
pub fn map_text(md: &str, f: impl Fn(&str) -> String) -> String {
    segments(md).into_iter().map(|(code, part)| if code { part.to_string() } else { f(part) }).collect()
}

/// Closing partner of a single delimiter at `open`: same line, no space just inside, not doubled.
fn close_single(c: &[char], open: usize, d: char) -> Option<usize> {
    let first = *c.get(open + 1)?;
    if first.is_whitespace() || first == d || (open > 0 && c[open - 1].is_alphanumeric()) {
        return None;
    }
    (open + 2..c.len())
        .take_while(|&j| c[j] != '\n')
        .find(|&j| c[j] == d && !c[j - 1].is_whitespace() && c[j - 1] != d && c.get(j + 1) != Some(&d))
}

/// Index of the first char of the closing pair for a double delimiter at `open`.
fn close_double(c: &[char], open: usize, d: char) -> Option<usize> {
    if !c.get(open + 2).is_some_and(|x| !x.is_whitespace()) {
        return None;
    }
    (open + 3..c.len().saturating_sub(1))
        .take_while(|&j| c[j] != '\n')
        .find(|&j| c[j] == d && c[j + 1] == d && !c[j - 1].is_whitespace())
}

fn single_to_double(c: &[char]) -> String {
    let mut out = String::with_capacity(c.len() + 8);
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if ch == '*' || ch == '~' {
            if c.get(i + 1) == Some(&ch) {
                // Already doubled (some clients send standard Markdown): copy the run.
                while i < c.len() && c[i] == ch {
                    out.push(ch);
                    i += 1;
                }
                continue;
            }
            if let Some(end) = close_single(c, i, ch) {
                let d = if ch == '*' { "**" } else { "~~" };
                out.push_str(d);
                out.push_str(&single_to_double(&c[i + 1..end]));
                out.push_str(d);
                i = end + 1;
                continue;
            }
        }
        out.push(ch);
        i += 1;
    }
    out
}

fn double_to_single(c: &[char]) -> String {
    let mut out = String::with_capacity(c.len());
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if (ch == '*' || ch == '~') && c.get(i + 1) == Some(&ch) {
            if let Some(end) = close_double(c, i, ch) {
                out.push(ch);
                out.push_str(&double_to_single(&c[i + 2..end]));
                out.push(ch);
                i = end + 2;
                continue;
            }
        } else if ch == '*' {
            if let Some(end) = close_single(c, i, ch) {
                out.push('_');
                out.push_str(&double_to_single(&c[i + 1..end]));
                out.push('_');
                i = end + 1;
                continue;
            }
        }
        out.push(ch);
        i += 1;
    }
    out
}

/// `*bold*` → `**bold**`, `~strike~` → `~~strike~~`.
pub fn from_single(md: &str) -> String {
    map_text(md, |t| single_to_double(&t.chars().collect::<Vec<_>>()))
}

/// `**bold**` → `*bold*`, `*italic*` → `_italic_`, `~~strike~~` → `~strike~`.
pub fn to_single(md: &str) -> String {
    map_text(md, |t| double_to_single(&t.chars().collect::<Vec<_>>()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_ways() {
        assert_eq!(from_single("*bold* and _it_ and ~gone~"), "**bold** and _it_ and ~~gone~~");
        assert_eq!(from_single("**already** standard"), "**already** standard");
        assert_eq!(from_single("* list item\n2*3*4 `*code*`"), "* list item\n2*3*4 `*code*`");
        assert_eq!(from_single("```\n*x*\n```\n*y*"), "```\n*x*\n```\n**y**");
        assert_eq!(to_single("**bold** *it* ~~gone~~"), "*bold* _it_ ~gone~");
        assert_eq!(to_single("- item\n`**raw**`"), "- item\n`**raw**`");
        assert_eq!(from_single(&to_single("**a** _b_ ~~c~~")), "**a** _b_ ~~c~~");
    }
}
