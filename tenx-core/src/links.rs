//! A task's links: its PRs, the Artifact pages and Claude Design projects its
//! agent made, its Linear issue, its ports, and other web pages — what `L`
//! (the picker) and `f` (hint letters) in the column open, and what tenx web
//! lists under its ↗ button.
//!
//! The watcher gathers the raw text — the agent's replies from its transcript,
//! the `## Links` section of `TASK.md` — and caches what [`find`] reads out of
//! it in `.tenx-live.json` ([`crate::live::Live`]); [`all`] adds the PRs and
//! ports tenx already knows and puts everything in display order. Tool results
//! are deliberately not read: a "list my projects" result alone carries
//! dozens of links that have nothing to do with the task, while an agent names
//! what it made in its reply.

use crate::live::PrInfo;
use base64::Engine;
use serde::{Deserialize, Serialize};

/// What a link points at — its icon, its group, and its place in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    Pr,
    Artifact,
    Design,
    Linear,
    Port,
    Web,
}

impl LinkKind {
    pub fn icon(self) -> &'static str {
        match self {
            LinkKind::Pr => "⇡",
            LinkKind::Artifact => "◇",
            LinkKind::Design => "◆",
            LinkKind::Linear => "▸",
            LinkKind::Port => "⌁",
            LinkKind::Web => "↗",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            LinkKind::Pr => "PR",
            LinkKind::Artifact => "artifact",
            LinkKind::Design => "design",
            LinkKind::Linear => "linear",
            LinkKind::Port => "port",
            LinkKind::Web => "web",
        }
    }

    /// Made with Claude (an Artifact page, a Claude Design project): what the
    /// row's `◇` chip counts.
    pub fn is_claude(self) -> bool {
        matches!(self, LinkKind::Artifact | LinkKind::Design)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub url: String,
    pub label: String,
    pub kind: LinkKind,
}

/// At most this many links of one kind are kept from the transcript: a long
/// session can mention hundreds of pages, and the newest are the ones you
/// come back for.
pub const MAX_PER_KIND: usize = 10;

/// `url`'s kind, from its host and path.
pub fn classify(url: &str) -> LinkKind {
    let rest = strip_scheme(url);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    match host.trim_start_matches("www.") {
        "claude.ai" if path.starts_with("artifact/") || path.starts_with("code/artifact/") => LinkKind::Artifact,
        "claude.ai" if path.starts_with("design/") => LinkKind::Design,
        "linear.app" if path.contains("/issue/") => LinkKind::Linear,
        "github.com" if pr_number(path).is_some() => LinkKind::Pr,
        "localhost" | "127.0.0.1" => LinkKind::Port,
        h if h.starts_with("localhost:") || h.starts_with("127.0.0.1:") => LinkKind::Port,
        _ => LinkKind::Web,
    }
}

fn strip_scheme(url: &str) -> &str {
    url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")).unwrap_or(url)
}

/// `owner/repo/pull/12[/files]` → the repo and the number.
fn pr_number(path: &str) -> Option<(&str, u64)> {
    let mut seg = path.split(['/', '?', '#']);
    let _owner = seg.next()?;
    let repo = seg.next()?;
    if seg.next()? != "pull" {
        return None;
    }
    Some((repo, seg.next()?.parse().ok()?))
}

/// A name for `url` when the text gave it none.
pub fn derived_label(url: &str, kind: LinkKind) -> String {
    let rest = strip_scheme(url);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    match kind {
        LinkKind::Pr => pr_number(path).map(|(r, n)| format!("{r}#{n}")).unwrap_or_else(|| rest.to_string()),
        LinkKind::Linear => {
            // <workspace>/issue/<KEY>/<slug>
            let mut it = path.split(['/', '?', '#']).skip_while(|s| *s != "issue").skip(1);
            match it.next().filter(|k| !k.is_empty()) {
                Some(key) => match it.next().filter(|s| !s.is_empty()) {
                    Some(slug) => format!("{key} {}", slug.replace('-', " ")),
                    None => key.to_string(),
                },
                None => "Linear issue".to_string(),
            }
        }
        LinkKind::Design => query_param(rest, "file").map(|f| file_title(&f)).unwrap_or_else(|| "Claude Design project".to_string()),
        LinkKind::Artifact => "Artifact".to_string(),
        LinkKind::Port => rest.trim_end_matches('/').to_string(),
        LinkKind::Web => {
            let host = host.trim_start_matches("www.");
            let path = path.split(['?', '#']).next().unwrap_or("").trim_end_matches('/');
            if path.is_empty() { host.to_string() } else { format!("{host}/{path}") }
        }
    }
}

fn query_param(rest: &str, key: &str) -> Option<String> {
    let query = rest.split_once('?')?.1.split('#').next()?;
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| percent_decode(v))
    })
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(v) => {
                        out.push(v);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `designs/Quick Links.dc.html` → `Quick Links`.
fn file_title(f: &str) -> String {
    let f = f.rsplit('/').next().unwrap_or(f);
    f.strip_suffix(".dc.html").or_else(|| f.strip_suffix(".html")).unwrap_or(f).to_string()
}

/// Every web link in `text`, in the order they appear, each named by the
/// Markdown link around it (`[name](url)`), else by a `Name: url` lead-in on
/// its line, else from the URL itself ([`derived_label`]). A PR or a Linear
/// issue is always named from its URL: `PR:` or `Linear:` says less than
/// `api#12`. Links to this machine are skipped — a port's link comes from the
/// ports that are listening now, not from a server the agent once started.
pub fn find(text: &str) -> Vec<Link> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut from = 0;
        while let Some(i) = next_url(line, from) {
            // Only what follows the previous link can name this one.
            let before = &line[from..i];
            let tail = &line[i..];
            let end = tail.find(|c: char| c.is_whitespace() || "<>\"'`|\\)]}".contains(c)).unwrap_or(tail.len());
            from = i + end;
            let url = tail[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', '*', '_', '~']);
            if strip_scheme(url).len() < 4 {
                continue;
            }
            let kind = classify(url);
            if kind == LinkKind::Port {
                continue;
            }
            let label = markdown_label(before)
                .or_else(|| (!matches!(kind, LinkKind::Pr | LinkKind::Linear)).then(|| context_label(before)).flatten())
                .unwrap_or_else(|| derived_label(url, kind));
            out.push(Link { url: url.to_string(), label, kind });
        }
    }
    out
}

fn next_url(line: &str, from: usize) -> Option<usize> {
    let s = &line[from..];
    [s.find("https://"), s.find("http://")].into_iter().flatten().min().map(|i| i + from)
}

/// `… [Name](` before a URL → `Name`.
fn markdown_label(before: &str) -> Option<String> {
    let inner = before.strip_suffix("](")?;
    let start = inner.rfind('[')?;
    let label = clean(&inner[start + 1..]);
    (!label.is_empty()).then_some(label)
}

/// `- **Design proposal**: ` before a URL → `Design proposal`: a short lead-in
/// that ends in a colon.
fn context_label(before: &str) -> Option<String> {
    let lead = before.trim_end().trim_end_matches(['*', '_', '(', '<']).trim_end();
    let lead = lead.strip_suffix(':')?;
    let lead = lead.trim_start().trim_start_matches(['-', '–', '—', '*', '+', '#', '>', ',', ';']).trim_start();
    let lead = lead.trim_start_matches(|c: char| c.is_ascii_digit()).trim_start_matches(['.', ')']);
    let label = clean(lead);
    (!label.is_empty() && label.chars().count() <= 40 && !label.contains(':')).then_some(label)
}

/// Markdown emphasis and code marks off, whitespace collapsed.
fn clean(s: &str) -> String {
    s.replace(['*', '`'], "").split_whitespace().collect::<Vec<_>>().join(" ").trim_matches('_').to_string()
}

/// The links in `TASK.md`'s `## Links` section.
pub fn task_md_links(md: &str) -> Vec<Link> {
    let mut inside = false;
    let mut section = String::new();
    for line in md.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            inside = heading.trim().eq_ignore_ascii_case("links");
            continue;
        }
        if inside {
            section.push_str(line);
            section.push('\n');
        }
    }
    find(&section)
}

fn same(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

/// `found` (in the order the transcript said them, oldest first) laid over
/// `older` (newest first): newest first, each URL once — under the name it
/// was given last —, at most [`MAX_PER_KIND`] of a kind.
pub fn merge(older: &[Link], found: &[Link]) -> Vec<Link> {
    let mut out: Vec<Link> = Vec::new();
    for l in found.iter().rev().chain(older) {
        if out.iter().any(|o| same(&o.url, &l.url)) || out.iter().filter(|o| o.kind == l.kind).count() >= MAX_PER_KIND {
            continue;
        }
        out.push(l.clone());
    }
    out
}

/// Everything a task links to, in display order: PRs, Artifacts, designs,
/// Linear, ports, other pages; within a kind, the PR lookup first, then
/// `TASK.md`, then the transcript newest first. Each URL once.
pub fn all(prs: &[PrInfo], task: &[Link], found: &[Link], ports: &[u16]) -> Vec<Link> {
    let mut out: Vec<Link> = Vec::new();
    let pr_links = prs
        .iter()
        .filter(|p| !p.url.is_empty())
        .map(|p| Link { url: p.url.clone(), label: p.label(), kind: LinkKind::Pr });
    let port_links = ports
        .iter()
        .map(|p| Link { url: format!("http://localhost:{p}"), label: format!("localhost:{p}"), kind: LinkKind::Port });
    for l in pr_links.chain(task.iter().cloned()).chain(found.iter().cloned()).chain(port_links) {
        if !out.iter().any(|o| same(&o.url, &l.url)) {
            out.push(l);
        }
    }
    out.sort_by_key(|l| l.kind as u8);
    out
}

/// The hint letter of the `i`th link chip in `f` mode: `a`–`z`. Upper case
/// is taken by "copy", so there are 26.
pub fn hint_key(i: usize) -> Option<char> {
    (i < 26).then(|| (b'a' + i as u8) as char)
}

/// Which chip a typed hint letter names.
pub fn hint_index(c: char) -> Option<usize> {
    c.is_ascii_lowercase().then(|| (c as u8 - b'a') as usize)
}

/// The escape sequence that puts `text` on the terminal's clipboard (OSC 52).
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64::engine::general_purpose::STANDARD.encode(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(url: &str, label: &str, kind: LinkKind) -> Link {
        Link { url: url.into(), label: label.into(), kind }
    }

    #[test]
    fn urls_are_classified_and_named() {
        let pr = "https://github.com/aluedeke/tenx/pull/418/files";
        assert_eq!(classify(pr), LinkKind::Pr);
        assert_eq!(derived_label(pr, LinkKind::Pr), "tenx#418");
        let a = "https://claude.ai/code/artifact/0f3a";
        assert_eq!(classify(a), LinkKind::Artifact);
        assert_eq!(classify("https://claude.ai/artifact/abc"), LinkKind::Artifact);
        let d = "https://claude.ai/design/p/a234?file=Quick+Links.dc.html";
        assert_eq!(classify(d), LinkKind::Design);
        assert_eq!(derived_label(d, LinkKind::Design), "Quick Links");
        assert_eq!(derived_label("https://claude.ai/design/p/a234", LinkKind::Design), "Claude Design project");
        let l = "https://linear.app/acme/issue/TNX-142/quick-links";
        assert_eq!(classify(l), LinkKind::Linear);
        assert_eq!(derived_label(l, LinkKind::Linear), "TNX-142 quick links");
        assert_eq!(classify("http://localhost:7070/x"), LinkKind::Port);
        assert_eq!(classify("https://github.com/aluedeke/tenx"), LinkKind::Web);
        assert_eq!(derived_label("https://www.docs.rs/ratatui/", LinkKind::Web), "docs.rs/ratatui");
        assert_eq!(percent_decode("a%20b%2"), "a b%2");
    }

    #[test]
    fn find_reads_labels_from_markdown_and_lead_ins() {
        let text = "Done. See [Link picker proposal](https://claude.ai/code/artifact/abc).\n\
                    - **Design proposal**: https://claude.ai/design/p/x?file=Quick+Links.dc.html\n\
                    PR: https://github.com/o/tenx/pull/9, and https://docs.rs/ratatui.\n\
                    started on http://localhost:5173 — I made it here: <https://example.com/a>";
        assert_eq!(
            find(text),
            vec![
                link("https://claude.ai/code/artifact/abc", "Link picker proposal", LinkKind::Artifact),
                link("https://claude.ai/design/p/x?file=Quick+Links.dc.html", "Design proposal", LinkKind::Design),
                link("https://github.com/o/tenx/pull/9", "tenx#9", LinkKind::Pr),
                link("https://docs.rs/ratatui", "docs.rs/ratatui", LinkKind::Web),
                link("https://example.com/a", "I made it here", LinkKind::Web),
            ]
        );
        // A sentence too long to be a name falls back to the URL's.
        let long = "This is a very long sentence that explains everything in detail: https://example.com";
        assert_eq!(find(long)[0].label, "example.com");
        assert!(find("no links, just http:// and https://").is_empty());
    }

    #[test]
    fn task_md_links_reads_only_the_links_section() {
        let md = "# t\n\n## Description\n\nsee https://a.example\n\n## Links\n\n- Linear:\n- PR: https://github.com/o/r/pull/3\n- Spec: https://b.example/spec\n\n## Notes\n\nhttps://c.example\n";
        let found = task_md_links(md);
        assert_eq!(found, vec![link("https://github.com/o/r/pull/3", "r#3", LinkKind::Pr), link("https://b.example/spec", "Spec", LinkKind::Web)]);
    }

    #[test]
    fn merge_keeps_the_newest_once_and_caps_each_kind() {
        let older = vec![link("https://a.example/", "old a", LinkKind::Web)];
        let found = vec![link("https://b.example", "b", LinkKind::Web), link("https://a.example", "new a", LinkKind::Web)];
        let merged = merge(&older, &found);
        assert_eq!(merged.iter().map(|l| l.label.as_str()).collect::<Vec<_>>(), vec!["new a", "b"]);

        let many: Vec<Link> = (0..15).map(|i| link(&format!("https://x.example/{i}"), "x", LinkKind::Web)).collect();
        let merged = merge(&[], &many);
        assert_eq!(merged.len(), MAX_PER_KIND);
        assert_eq!(merged[0].url, "https://x.example/14");
    }

    #[test]
    fn all_orders_by_kind_and_drops_duplicates() {
        let pr = PrInfo { number: 418, url: "https://github.com/o/tenx/pull/418".into(), title: "open links".into(), ..Default::default() };
        let task = vec![link("https://linear.app/a/issue/T-1", "T-1", LinkKind::Linear)];
        let found = vec![
            link("https://docs.rs", "docs", LinkKind::Web),
            link("https://github.com/o/tenx/pull/418", "tenx#418", LinkKind::Pr),
            link("https://claude.ai/artifact/z", "proposal", LinkKind::Artifact),
        ];
        let got = all(&[pr], &task, &found, &[7070]);
        let labels: Vec<_> = got.iter().map(|l| l.label.as_str()).collect();
        assert_eq!(labels, vec!["#418 open links", "proposal", "T-1", "localhost:7070", "docs"]);
    }

    #[test]
    fn hints_are_lower_case_letters() {
        assert_eq!(hint_key(0), Some('a'));
        assert_eq!(hint_key(25), Some('z'));
        assert_eq!(hint_key(26), None);
        assert_eq!(hint_index('c'), Some(2));
        assert_eq!(hint_index('C'), None);
        assert_eq!(osc52("hi"), "\x1b]52;c;aGk=\x07");
    }
}
