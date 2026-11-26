//! A task's pages: web pages kept as tabs beside its terminal tabs (agent,
//! shell, nvim). The list is the task's, in [`FILE`] in its directory, so every
//! device shows the same tabs; which one a device has on screen is that
//! device's own. The iOS app shows a page in a web view of its own, over the
//! terminal, with the cookies and logins of the task's workspace
//! ([`store_id`]): every task of a workspace shares them, other workspaces
//! never see them.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The list, in the task's directory.
pub const FILE: &str = ".tenx-pages.json";

/// Pages a task keeps at most: a bar of chips, not a browser history.
pub const MAX_PAGES: usize = 12;

/// A title longer than this is cut; the chip shows far less.
const MAX_TITLE: usize = 200;

/// An address longer than this is refused.
const MAX_URL: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page {
    /// `p1`, `p2`, …: stable while the page is open, never reused by a
    /// later one in the same list (a device may still show it).
    pub id: String,
    /// Where the page is now: it follows the page's own navigation.
    pub url: String,
    /// The page's title, empty until it loaded once.
    #[serde(default)]
    pub title: String,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    pages: Vec<Page>,
    /// The last id handed out, so a closed page's id isn't reused.
    #[serde(default)]
    last: u64,
}

/// A task's pages and the counter their ids come from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pages {
    pub pages: Vec<Page>,
    last: u64,
}

/// Why a page wasn't opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    NotWeb,
    Full,
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::NotWeb => write!(f, "only http:// and https:// addresses open as a page"),
            OpenError::Full => write!(f, "this task has {MAX_PAGES} pages open already; close one first"),
        }
    }
}

impl Pages {
    /// The file's text; anything unreadable is no pages, and an entry
    /// that isn't a web address is dropped.
    pub fn parse(text: &str) -> Pages {
        let file: File = serde_json::from_str(text).unwrap_or_default();
        let mut pages: Vec<Page> = file.pages.into_iter().filter(|p| web_url(&p.url) && !p.id.is_empty()).collect();
        pages.truncate(MAX_PAGES);
        let highest = pages.iter().filter_map(|p| number(&p.id)).max().unwrap_or(0);
        Pages { pages, last: file.last.max(highest) }
    }

    pub fn render(&self) -> String {
        let file = File { pages: self.pages.clone(), last: self.last };
        serde_json::to_string_pretty(&file).unwrap_or_default() + "\n"
    }

    /// Open `url` as a page and return its id; an address already open is
    /// that page, not a second one.
    pub fn open(&mut self, url: &str) -> Result<String, OpenError> {
        let url = url.trim();
        if !web_url(url) {
            return Err(OpenError::NotWeb);
        }
        if let Some(page) = self.pages.iter().find(|p| p.url == url) {
            return Ok(page.id.clone());
        }
        if self.pages.len() >= MAX_PAGES {
            return Err(OpenError::Full);
        }
        self.last += 1;
        let id = format!("p{}", self.last);
        self.pages.push(Page { id: id.clone(), url: url.to_string(), title: String::new() });
        Ok(id)
    }

    /// Whether there was such a page.
    pub fn close(&mut self, id: &str) -> bool {
        let before = self.pages.len();
        self.pages.retain(|p| p.id != id);
        self.pages.len() != before
    }

    /// The page went somewhere else, or learned its title: whether that
    /// changed anything. An address that isn't a web one (`about:blank`
    /// while it loads) keeps the last one.
    pub fn navigated(&mut self, id: &str, url: &str, title: &str) -> bool {
        let Some(page) = self.pages.iter_mut().find(|p| p.id == id) else { return false };
        let title: String = title.trim().chars().take(MAX_TITLE).collect();
        let url = url.trim();
        let mut changed = false;
        if web_url(url) && page.url != url {
            page.url = url.to_string();
            changed = true;
        }
        if !title.is_empty() && page.title != title {
            page.title = title;
            changed = true;
        }
        changed
    }
}

fn number(id: &str) -> Option<u64> {
    id.strip_prefix('p')?.parse().ok()
}

/// An `http://` or `https://` address with a host and nothing a URL can't hold.
pub fn web_url(url: &str) -> bool {
    if url.len() > MAX_URL || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    let lower = url.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://")) else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit('@').next().unwrap_or("");
    !host.is_empty() && !host.starts_with(':')
}

/// The cookie store of a workspace's pages, as a UUID (the iOS app's
/// `WKWebsiteDataStore(forIdentifier:)`): made from the workspace's
/// directory, so it stays the same however its tasks come and go, and a
/// workspace moved or renamed starts with no logins.
pub fn store_id(workspace_dir: &str) -> String {
    let hash = Sha256::digest(format!("tenx-pages:{workspace_dir}").as_bytes());
    let mut b = [0u8; 16];
    b.copy_from_slice(&hash[..16]);
    // A name-based UUID (version 5's bits, RFC 9562), so anything that
    // checks the form accepts it.
    b[6] = (b[6] & 0x0f) | 0x50;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_opens_once_and_ids_are_never_reused() {
        let mut p = Pages::default();
        assert_eq!(p.open("https://linear.app/acme").unwrap(), "p1");
        assert_eq!(p.open(" https://github.com/acme/shop ").unwrap(), "p2");
        assert_eq!(p.open("https://linear.app/acme").unwrap(), "p1", "already open");
        assert!(p.close("p2"));
        assert!(!p.close("p2"));
        assert_eq!(p.open("https://app.checklyhq.com").unwrap(), "p3");
        let back = Pages::parse(&p.render());
        assert_eq!(back, p);
        let mut back = back;
        assert!(back.close("p3"));
        assert_eq!(Pages::parse(&back.render()).clone().open("https://x.dev").unwrap(), "p4", "the counter is saved");
    }

    #[test]
    fn only_web_addresses_open_and_a_task_keeps_at_most_twelve() {
        let mut p = Pages::default();
        for bad in ["", "javascript:alert(1)", "file:///etc/passwd", "https://", "https:///x", "https://a b", "ftp://x"] {
            assert_eq!(p.open(bad), Err(OpenError::NotWeb), "{bad:?}");
        }
        assert!(p.open("http://localhost:3000/checkout").is_ok());
        for i in 1..MAX_PAGES {
            p.open(&format!("https://e.dev/{i}")).unwrap();
        }
        assert_eq!(p.open("https://one-more.dev"), Err(OpenError::Full));
    }

    #[test]
    fn navigation_moves_the_page_and_names_it() {
        let mut p = Pages::default();
        let id = p.open("https://github.com/login").unwrap();
        assert!(p.navigated(&id, "https://github.com/", "GitHub"));
        assert!(!p.navigated(&id, "https://github.com/", "GitHub"));
        assert!(!p.navigated(&id, "about:blank", ""), "a non-web address keeps the last one");
        assert!(!p.navigated("p9", "https://x.dev", "X"));
        assert_eq!(p.pages[0].url, "https://github.com/");
        assert_eq!(p.pages[0].title, "GitHub");
        p.navigated(&id, "https://github.com/", &"t".repeat(500));
        assert_eq!(p.pages[0].title.len(), MAX_TITLE);
    }

    #[test]
    fn a_broken_file_is_no_pages() {
        assert_eq!(Pages::parse("not json"), Pages::default());
        let p = Pages::parse(r#"{"pages":[{"id":"p7","url":"https://ok.dev"},{"id":"p8","url":"javascript:x"}]}"#);
        assert_eq!(p.pages.len(), 1);
        assert_eq!(p.clone().open("https://new.dev").unwrap(), "p8", "ids go on from the highest");
    }

    #[test]
    fn each_workspace_has_a_store_of_its_own() {
        let a = store_id("/Users/a/acme");
        assert_eq!(a, store_id("/Users/a/acme"));
        assert_ne!(a, store_id("/Users/a/tenx"));
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "5");
        assert!(matches!(&a[19..20], "8" | "9" | "a" | "b"));
    }
}
