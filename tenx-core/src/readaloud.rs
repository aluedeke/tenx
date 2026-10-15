//! Reading an agent's reply aloud (`tenx web`'s speaker button): the reply is
//! Markdown, written to be looked at, and a speech synthesizer reads every
//! `*`, `|` and URL it is given. `spoken_text` turns it into plain sentences:
//! emphasis and code marks dropped, links reduced to their text, list items
//! and headings ended like sentences, table rows read as lists, and code
//! blocks left out — read aloud, code is noise.

/// What a code block is read as.
pub const CODE_BLOCK: &str = "(code block)";

/// `markdown` as text to speak: one line per paragraph, heading, list item or
/// table row; blank and rule lines dropped.
pub fn spoken_text(markdown: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut fence: Option<&str> = None;
    for line in markdown.lines() {
        let trimmed = line.trim();
        if let Some(open) = fence {
            if trimmed.starts_with(open) {
                fence = None;
            }
            continue;
        }
        if let Some(open) = ["```", "~~~"].into_iter().find(|f| trimmed.starts_with(f)) {
            fence = Some(open);
            if out.last().map(String::as_str) != Some(CODE_BLOCK) {
                out.push(CODE_BLOCK.to_string());
            }
            continue;
        }
        if trimmed.is_empty() || is_rule(trimmed) || is_table_separator(trimmed) {
            continue;
        }
        let (body, block) = strip_block(trimmed);
        let text = if trimmed.starts_with('|') { table_row(body) } else { inline(body) };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        out.push(if block { sentence(text) } else { text.to_string() });
    }
    out.join("\n")
}

/// `---`, `***`, `___`: a horizontal rule.
fn is_rule(line: &str) -> bool {
    let c: Vec<char> = line.chars().filter(|c| !c.is_whitespace()).collect();
    c.len() >= 3 && ['-', '*', '_'].iter().any(|m| c.iter().all(|x| x == m))
}

/// `|---|:--:|`: the line under a table's header.
fn is_table_separator(line: &str) -> bool {
    line.starts_with('|') && line.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

/// The line without its block marker (heading `#`s, quote `>`, list bullet or
/// number), and whether it had one — such a line is a unit of its own and
/// is ended like a sentence, so the voice pauses after it.
fn strip_block(line: &str) -> (&str, bool) {
    let mut s = line;
    let mut block = false;
    while let Some(rest) = s.strip_prefix('>') {
        s = rest.trim_start();
    }
    let hashes = s.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) && s[hashes..].starts_with(' ') {
        return (s[hashes..].trim(), true);
    }
    for bullet in ["- [ ] ", "- [x] ", "- ", "* ", "+ "] {
        if let Some(rest) = s.strip_prefix(bullet) {
            return (rest.trim(), true);
        }
    }
    let digits = s.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 {
        if let Some(rest) = s[digits..].strip_prefix(". ").or_else(|| s[digits..].strip_prefix(") ")) {
            s = rest.trim();
            block = true;
        }
    }
    if s.starts_with('|') {
        block = true;
    }
    (s, block)
}

/// `| a | b |` → `a, b`.
fn table_row(line: &str) -> String {
    line.trim_matches('|')
        .split('|')
        .map(|cell| inline(cell.trim()))
        .filter(|cell| !cell.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Ends `text` with a full stop unless it already ends a sentence.
fn sentence(text: &str) -> String {
    match text.chars().last() {
        Some('.' | '!' | '?' | ':' | ';' | '…') => text.to_string(),
        _ => format!("{text}."),
    }
}

/// Inline Markdown dropped: `[text](url)` → `text`, a bare URL → "a link",
/// `` ` ``, `**`, `__`, `~~` and `*` around words removed. A single `_` is
/// kept: it is more often in a `snake_case` name than around one.
fn inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if c == '[' {
            if let Some((label, after)) = link(rest) {
                out.push_str(label);
                rest = after;
                continue;
            }
        }
        if rest.starts_with("http://") || rest.starts_with("https://") {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            out.push_str("a link");
            // Punctuation after a URL belongs to the sentence.
            let url = &rest[..end];
            let tail = url.len() - url.trim_end_matches(['.', ',', ')', ';', ':']).len();
            rest = &rest[end - tail..];
            continue;
        }
        if let Some(after) = ["**", "__", "~~"].iter().find_map(|m| rest.strip_prefix(m)) {
            rest = after;
            continue;
        }
        // A `*` with spaces on both sides is arithmetic, not emphasis.
        let lone_star = c == '*' && rest[1..].starts_with(' ') && out.ends_with(' ');
        if c == '`' || (c == '*' && !lone_star) {
            rest = &rest[1..];
            continue;
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    // Collapse what removed marks left behind.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `[label](target)` at the start of `s`: the label and what follows.
fn link(s: &str) -> Option<(&str, &str)> {
    let close = s.find("](")?;
    let label = &s[1..close];
    if label.contains('[') {
        return None;
    }
    let end = s[close + 2..].find(')')? + close + 2;
    Some((label, &s[end + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emphasis_code_and_links_are_dropped() {
        assert_eq!(
            spoken_text("Run **`make test`** and see [the docs](https://x.y/z) or https://example.com/a."),
            "Run make test and see the docs or a link."
        );
        assert_eq!(spoken_text("a *very* ~~bad~~ good idea"), "a very bad good idea");
        assert_eq!(spoken_text("keep snake_case_names as they are"), "keep snake_case_names as they are");
        assert_eq!(spoken_text("2 * 3 = 6"), "2 * 3 = 6");
    }

    #[test]
    fn blocks_become_sentences() {
        let md = "## Result\n\nIt works.\n\n- first point\n- second point!\n1. step one\n> quoted\n---\n";
        assert_eq!(spoken_text(md), "Result.\nIt works.\nfirst point.\nsecond point!\nstep one.\nquoted");
    }

    #[test]
    fn code_blocks_are_left_out() {
        let md = "Before.\n```rust\nfn main() {}\n```\n```\nmore\n```\nAfter.";
        assert_eq!(spoken_text(md), format!("Before.\n{CODE_BLOCK}\nAfter."));
        // An unclosed fence runs to the end, as Markdown has it.
        assert_eq!(spoken_text("Hi.\n~~~\nx\n"), format!("Hi.\n{CODE_BLOCK}"));
    }

    #[test]
    fn table_rows_are_read_as_lists() {
        let md = "| State | Meaning |\n|---|:--|\n| **Blocked** | waits on you |";
        assert_eq!(spoken_text(md), "State, Meaning.\nBlocked, waits on you.");
    }

    #[test]
    fn plain_text_is_unchanged() {
        assert_eq!(spoken_text("Yes, this is possible."), "Yes, this is possible.");
        assert_eq!(spoken_text(""), "");
    }
}
