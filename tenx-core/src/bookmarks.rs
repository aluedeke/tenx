//! A workspace's bookmarks: web pages every task of the workspace can open as
//! a page with one tap (the page bar's `+`), e.g. the team's Linear board or
//! a service's prod, staging and dev consoles. They live in the workspace's
//! `config.toml` (`[[bookmarks]]`), so every task and every device shares
//! them; the pages they open use the workspace's logins (`pages::store_id`).

use serde::{Deserialize, Serialize};

use crate::pages::web_url;

/// Bookmarks a workspace keeps at most: a short list to pick from.
pub const MAX_BOOKMARKS: usize = 40;

/// A name longer than this is cut.
const MAX_NAME: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    /// What the list says: the page's title when it was saved, else its host.
    #[serde(default)]
    pub name: String,
    pub url: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AddError {
    NotWeb,
    Full,
}

impl std::fmt::Display for AddError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AddError::NotWeb => write!(f, "only a web address (http or https) can be a bookmark"),
            AddError::Full => write!(f, "the workspace has {MAX_BOOKMARKS} bookmarks; remove one first"),
        }
    }
}

/// The name a bookmark gets: `name` trimmed and cut, else the address's host
/// and path.
pub fn name_for(name: &str, url: &str) -> String {
    let name: String = name.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_NAME).collect();
    if !name.is_empty() {
        return name;
    }
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    // Never a `user:password@` the address carries: the name is on every
    // device's list.
    let rest = match rest.split_once(['/', '?', '#']) {
        Some((host, _)) if host.contains('@') => &rest[host.rfind('@').map_or(0, |i| i + 1)..],
        None if rest.contains('@') => &rest[rest.rfind('@').map_or(0, |i| i + 1)..],
        _ => rest,
    };
    rest.trim_end_matches('/').chars().take(MAX_NAME).collect()
}

/// Add `url` as `name`; an address already there is renamed and keeps its
/// place. Whether the list changed.
pub fn add(list: &mut Vec<Bookmark>, url: &str, name: &str) -> Result<bool, AddError> {
    let url = url.trim();
    if !web_url(url) {
        return Err(AddError::NotWeb);
    }
    let name = name_for(name, url);
    if let Some(b) = list.iter_mut().find(|b| b.url == url) {
        if b.name == name {
            return Ok(false);
        }
        b.name = name;
        return Ok(true);
    }
    if list.len() >= MAX_BOOKMARKS {
        return Err(AddError::Full);
    }
    list.push(Bookmark { name, url: url.to_string() });
    Ok(true)
}

/// A list as it may come from a hand-written file: web addresses only, each
/// once (its first entry), names tidied, at most [`MAX_BOOKMARKS`].
pub fn clean(list: Vec<Bookmark>) -> Vec<Bookmark> {
    let mut out: Vec<Bookmark> = vec![];
    for b in list {
        let url = b.url.trim();
        if out.len() >= MAX_BOOKMARKS || !web_url(url) || out.iter().any(|o| o.url == url) {
            continue;
        }
        out.push(Bookmark { name: name_for(&b.name, url), url: url.to_string() });
    }
    out
}

/// The `url` and `name` of each `[[bookmarks]]` entry (or of an inline
/// `bookmarks = [{ … }]`), as written: an entry without a `url` string is
/// skipped, a missing `name` is empty.
fn entries(doc: &toml_edit::DocumentMut) -> Vec<Bookmark> {
    let pick = |t: &dyn toml_edit::TableLike| {
        let url = t.get("url")?.as_str()?.to_string();
        let name = t.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_string();
        Some(Bookmark { name, url })
    };
    match doc.get("bookmarks") {
        Some(toml_edit::Item::ArrayOfTables(a)) => a.iter().filter_map(|t| pick(t)).collect(),
        Some(toml_edit::Item::Value(toml_edit::Value::Array(a))) => {
            a.iter().filter_map(|v| v.as_inline_table()).filter_map(|t| pick(t)).collect()
        }
        _ => vec![],
    }
}

/// The bookmarks of a `config.toml`'s text, cleaned ([`clean`]); `None`
/// when the text isn't TOML (e.g. an editor half-way through a save). An
/// entry tenx can't use is left out, never an error: the rest of the file is
/// the workspace's, and a bad bookmark mustn't stop it from loading.
pub fn read(config: &str) -> Option<Vec<Bookmark>> {
    let doc: toml_edit::DocumentMut = config.parse().ok()?;
    Some(clean(entries(&doc)))
}

/// What the page asks of the bookmarks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// Keep `url` as a bookmark named `name` ([`add`]).
    Add { url: String, name: String },
    /// Drop the bookmark for `url` ([`remove`]).
    Remove { url: String },
}

/// `config` with `edit` made to its bookmarks, or `None` when nothing
/// changes. Only the entry concerned is touched: the other entries, their
/// extra keys and comments, and the rest of the file stay as written.
pub fn apply(config: &str, edit: &Edit) -> Result<Option<String>, String> {
    let mut doc: toml_edit::DocumentMut = config.parse().map_err(|e| format!("config.toml isn't valid TOML: {e}"))?;
    let mut list = clean(entries(&doc));
    let same = |t: &dyn toml_edit::TableLike, url: &str| t.get("url").and_then(|u| u.as_str()).is_some_and(|u| u.trim() == url);
    match edit {
        Edit::Add { url, name } => {
            if !add(&mut list, url, name).map_err(|e| e.to_string())? {
                return Ok(None);
            }
            let url = url.trim();
            let name = name_for(name, url);
            if doc.get("bookmarks").is_none() {
                doc.insert("bookmarks", toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()));
            }
            match doc.get_mut("bookmarks") {
                Some(toml_edit::Item::ArrayOfTables(a)) => {
                    let at = a.iter().position(|t| same(t, url));
                    match at.and_then(|i| a.get_mut(i)) {
                        Some(t) => {
                            t.insert("name", toml_edit::value(name));
                        }
                        None => {
                            let mut t = toml_edit::Table::new();
                            t.insert("name", toml_edit::value(name));
                            t.insert("url", toml_edit::value(url));
                            a.push(t);
                        }
                    }
                }
                Some(toml_edit::Item::Value(toml_edit::Value::Array(a))) => {
                    let at = a.iter().position(|v| v.as_inline_table().is_some_and(|t| same(t, url)));
                    match at {
                        Some(i) => {
                            if let Some(t) = a.get_mut(i).and_then(|v| v.as_inline_table_mut()) {
                                t.insert("name", name.into());
                            }
                        }
                        None => {
                            let mut t = toml_edit::InlineTable::new();
                            t.insert("name", name.into());
                            t.insert("url", url.into());
                            a.push(t);
                        }
                    }
                }
                _ => return Err("`bookmarks` in config.toml isn't a list of bookmarks: fix it first".into()),
            }
        }
        Edit::Remove { url } => {
            let url = url.trim();
            let empty = match doc.get_mut("bookmarks") {
                Some(toml_edit::Item::ArrayOfTables(a)) => {
                    let before = a.len();
                    a.retain(|t| !same(t, url));
                    if a.len() == before {
                        return Ok(None);
                    }
                    a.is_empty()
                }
                Some(toml_edit::Item::Value(toml_edit::Value::Array(a))) => {
                    let before = a.len();
                    a.retain(|v| !v.as_inline_table().is_some_and(|t| same(t, url)));
                    if a.len() == before {
                        return Ok(None);
                    }
                    a.is_empty()
                }
                _ => return Ok(None),
            };
            if empty {
                doc.remove("bookmarks");
            }
        }
    }
    Ok(Some(doc.to_string()))
}

/// The list to show after reading `config.toml` again: what it holds, or,
/// while it doesn't parse (`read` gave `None`), the list shown so far if it
/// came from the same file and nothing if it came from another workspace's.
pub fn reread(shown: Vec<Bookmark>, same_file: bool, read: Option<Vec<Bookmark>>) -> Vec<Bookmark> {
    match read {
        Some(list) => list,
        None if same_file => shown,
        None => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_a_web_address_once_and_renames_it() {
        let mut list = vec![];
        assert_eq!(add(&mut list, " https://linear.app/acme ", "Acme  board"), Ok(true));
        assert_eq!(list, vec![Bookmark { name: "Acme board".into(), url: "https://linear.app/acme".into() }]);
        assert_eq!(add(&mut list, "https://linear.app/acme", "Acme board"), Ok(false));
        assert_eq!(add(&mut list, "https://linear.app/acme", "Board"), Ok(true));
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "Board");
    }

    #[test]
    fn names_a_bookmark_by_its_address_when_it_has_no_title() {
        assert_eq!(name_for("", "https://app.checklyhq.com/accounts/"), "app.checklyhq.com/accounts");
        assert_eq!(name_for("  ", "http://mac:3000/"), "mac:3000");
        assert_eq!(name_for("", "https://deploy:s3cr3t@ci.internal/builds"), "ci.internal/builds");
        assert_eq!(name_for("", "https://deploy:s3cr3t@ci.internal"), "ci.internal");
        assert_eq!(name_for("", "https://ci.internal/a@b"), "ci.internal/a@b");
    }

    #[test]
    fn refuses_other_schemes_and_a_full_list() {
        let mut list = vec![];
        assert_eq!(add(&mut list, "javascript:alert(1)", "x"), Err(AddError::NotWeb));
        assert_eq!(add(&mut list, "file:///etc/passwd", "x"), Err(AddError::NotWeb));
        for i in 0..MAX_BOOKMARKS {
            add(&mut list, &format!("https://example.com/{i}"), "").unwrap();
        }
        assert_eq!(add(&mut list, "https://example.com/more", ""), Err(AddError::Full));
        // Renaming one that is there still works when full.
        assert_eq!(add(&mut list, "https://example.com/0", "zero"), Ok(true));
    }

    #[test]
    fn cleans_a_hand_written_list() {
        let b = |name: &str, url: &str| Bookmark { name: name.into(), url: url.into() };
        let list = clean(vec![b("A", "https://a.example"), b("again", "https://a.example"), b("x", "file:///etc"), b("", " https://b.example/ ")]);
        assert_eq!(list, vec![b("A", "https://a.example"), b("b.example", "https://b.example/")]);
    }

    #[test]
    fn edits_one_entry_and_keeps_the_rest_of_the_file() {
        let config = "# my workspace\nname = \"work\"   # the name\nfuture_key = 1\n\n[[repos]]\nname = \"api\"\nurl = \"git@x:api\"\n";
        let add = |c: &str, url: &str, name: &str| apply(c, &Edit::Add { url: url.into(), name: name.into() }).unwrap().unwrap();
        let out = add(config, "https://linear.app/acme", "Linear");
        assert!(out.starts_with("# my workspace\nname = \"work\"   # the name\nfuture_key = 1\n"), "{out}");
        assert!(out.contains("[[repos]]\nname = \"api\""), "{out}");
        assert_eq!(read(&out), Some(vec![Bookmark { name: "Linear".into(), url: "https://linear.app/acme".into() }]));
        assert_eq!(apply(&out, &Edit::Add { url: "https://linear.app/acme".into(), name: "Linear".into() }), Ok(None));
        // Empty again: the section goes, the rest stays.
        let none = apply(&out, &Edit::Remove { url: "https://linear.app/acme".into() }).unwrap().unwrap();
        assert!(!none.contains("bookmarks") && none.contains("future_key = 1"), "{none}");
        assert_eq!(apply(&none, &Edit::Remove { url: "https://linear.app/acme".into() }), Ok(None));
        assert_eq!(read("name = "), None);
        assert!(apply("name = ", &Edit::Remove { url: "x".into() }).is_err());
    }

    #[test]
    fn leaves_hand_written_entries_alone() {
        // A comment, an extra key, a long name, an entry tenx can't use.
        let long = "n".repeat(100);
        let config = format!(
            "name = \"w\"\n\n# prod first\n[[bookmarks]]\nname = \"{long}\"\nurl = \"https://prod.example\"\ngroup = \"prod\"\n\n[[bookmarks]]\nurl = \"file:///etc\"\n\n[[bookmarks]]\nurl = \"https://dev.example\"\n"
        );
        let list = read(&config).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[1], Bookmark { name: "dev.example".into(), url: "https://dev.example".into() });
        let out = apply(&config, &Edit::Add { url: "https://dev.example".into(), name: "Dev".into() }).unwrap().unwrap();
        assert!(out.contains("# prod first") && out.contains("group = \"prod\"") && out.contains(&long) && out.contains("file:///etc"), "{out}");
        // Named where it is: the key goes after the ones written.
        assert!(out.contains("url = \"https://dev.example\"\nname = \"Dev\""), "{out}");
        let out = apply(&out, &Edit::Remove { url: "https://dev.example".into() }).unwrap().unwrap();
        assert!(out.contains("group = \"prod\"") && !out.contains("dev.example"), "{out}");
    }

    #[test]
    fn a_bad_entry_or_value_never_fails_the_read() {
        assert_eq!(read("name = \"w\"\n[[bookmarks]]\nname = 3\nurl = \"https://a.example\"\n").unwrap()[0].name, "a.example");
        assert_eq!(read("bookmarks = 5\n"), Some(vec![]));
        assert!(apply("bookmarks = 5\n", &Edit::Add { url: "https://a.example".into(), name: "".into() }).is_err());
        let inline = "bookmarks = [{ url = \"https://a.example\" }]\n";
        assert_eq!(read(inline).unwrap().len(), 1);
        let out = apply(inline, &Edit::Add { url: "https://b.example".into(), name: "B".into() }).unwrap().unwrap();
        assert_eq!(read(&out).unwrap().len(), 2);
    }

    #[test]
    fn keeps_the_list_while_the_file_is_broken() {
        let a = vec![Bookmark { name: "a".into(), url: "https://a.example".into() }];
        assert_eq!(reread(a.clone(), true, None), a);
        assert_eq!(reread(a.clone(), false, None), vec![]);
        assert_eq!(reread(a, true, Some(vec![])), vec![]);
    }
}
