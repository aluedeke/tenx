//! Decisions behind `tenx secrets` — what a task's queues and sealed files
//! *mean*, given only facts the binary reads from disk.
//!
//! An agent's `need` call has no terminal, so all it can do is enqueue a
//! request and then wait for a human to act on it from a real shell or the
//! column. Three different things make a name leave the queues: the human
//! granted it (an output was written, then the name was cleared), the human
//! denied it (a denial was recorded, then the name was cleared), or someone
//! withdrew it (`tenx secrets cancel`). Granting and withdrawing are not
//! recorded at all, because the disk already knows: fulfilment rewrites an
//! output file, withdrawal doesn't. So the waiter compares the outputs'
//! modification times against the instant it made its request. A denial is
//! the one outcome that carries information the disk otherwise wouldn't
//! have — the human's note — so it is written down.
//!
//! The routing half — is a name already released, sealed and only waiting
//! for a passphrase, or missing so that someone must type a value — rests on
//! one property of `sops`: it encrypts a document's *values* only, so the
//! key names of a sealed file are readable without the identity
//! ([`sealed_keys`]). Where a name is looked for, and in which order — the
//! task's bundle, the workspace bundle, a repo's own sops files — is
//! [`route`]/[`sources`]; which bundle a release reads ([`bundle_release`]:
//! the task's overrides the workspace's) and where a typed value is kept
//! ([`Scope`], [`seal_scope`]) are the rest of the rules.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::SystemTime;

/// What a waiting request should do after one look at the disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitOutcome {
    /// The name is still queued — keep waiting.
    Pending,
    /// The name left the queue and an output was written since the request
    /// was made — the human acted on it.
    Fulfilled,
    /// The name left the queue with a denial recorded — the human said no.
    Denied,
    /// The name left the queue, nothing was written and nothing denied —
    /// withdrawn.
    Cancelled,
}

/// Decide the outcome from `still_pending` (is the name still in a queue?),
/// `denied` (is a denial recorded for it? — enqueueing clears any earlier
/// one, so a present denial always postdates the request) and `outputs`
/// (modification times of every file a fulfilment would have written;
/// missing files simply aren't in the slice) relative to `requested_at`. An
/// output touched *at or after* the request counts: fulfilment always
/// happens after the request, and callers pad `requested_at` backwards for
/// coarse filesystem timestamps rather than this function guessing at a
/// tolerance.
pub fn wait_outcome(still_pending: bool, denied: bool, outputs: &[SystemTime], requested_at: SystemTime) -> WaitOutcome {
    if still_pending {
        return WaitOutcome::Pending;
    }
    if denied {
        return WaitOutcome::Denied;
    }
    if outputs.iter().any(|t| *t >= requested_at) {
        WaitOutcome::Fulfilled
    } else {
        WaitOutcome::Cancelled
    }
}

/// Exit code for a finished wait over several names, so an agent can branch
/// without parsing prose: 0 when everything was granted, otherwise the
/// outcome that most changes what it should do next — a denial (don't ask
/// again without a reason) over a withdrawal over a timeout (just re-run).
pub fn exit_code(outcomes: &[WaitOutcome]) -> i32 {
    if outcomes.contains(&WaitOutcome::Denied) {
        EXIT_DENIED
    } else if outcomes.contains(&WaitOutcome::Cancelled) {
        EXIT_WITHDRAWN
    } else if outcomes.contains(&WaitOutcome::Pending) {
        EXIT_PENDING
    } else {
        0
    }
}

/// Still queued when the wait ran out; re-running resumes it.
pub const EXIT_PENDING: i32 = 3;
/// A human denied at least one name.
pub const EXIT_DENIED: i32 = 4;
/// At least one name was withdrawn (`cancel`) before anyone answered.
pub const EXIT_WITHDRAWN: i32 = 5;

/// Whether `name` can be a secret name: a dotenv key or a filename fragment,
/// and a line in the queue files — so no whitespace (the side files are
/// tab-separated), no path separators, no quotes.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.chars().any(|c| c.is_whitespace() || matches!(c, '/' | '\\' | '"' | '='))
}

/// Key names of a dotenv document — a `sops`-sealed one (values are
/// `ENC[...]`, keys are plain) or a released one. Skips comments, blank
/// lines and `sops`'s own metadata keys (`sops_version`, `sops_mac`, ...).
pub fn dotenv_keys(text: &str) -> Vec<String> {
    text.lines().filter_map(dotenv_key).filter(|k| !k.starts_with("sops_")).map(str::to_string).collect()
}

fn dotenv_key(line: &str) -> Option<&str> {
    let line = line.trim_start();
    if line.starts_with('#') {
        return None;
    }
    let line = line.strip_prefix("export ").unwrap_or(line);
    let (key, _) = line.split_once('=')?;
    let key = key.trim();
    (!key.is_empty()).then_some(key)
}

/// Key names of a `sops`-sealed document in any of the formats `sops`
/// writes — dotenv (`KEY=ENC[...]`), YAML (`key: ENC[...]`) or JSON
/// (`"key": "ENC[...]"`) — found by their encrypted values, which is what
/// lets a request be routed without the identity. Nested keys count as
/// well; the scan stops at `sops`'s own metadata block (`sops:` / `"sops":`,
/// always written last), whose `mac` is also an `ENC[...]` value.
pub fn sealed_keys(text: &str) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for line in text.lines() {
        let l = line.trim_start().trim_start_matches("- ");
        if l.starts_with("sops:") || l.starts_with("\"sops\":") {
            break;
        }
        let l = l.strip_prefix("export ").unwrap_or(l);
        let (key, rest) = if let Some(r) = l.strip_prefix('"') {
            let Some((k, rest)) = r.split_once('"') else { continue };
            let Some(rest) = rest.trim_start().strip_prefix(':') else { continue };
            (k, rest)
        } else if let Some(i) = l.find(['=', ':']) {
            (l[..i].trim(), &l[i + 1..])
        } else {
            continue;
        };
        let value = rest.trim_start().trim_start_matches(['"', '\'']);
        if value.starts_with("ENC[") && !key.is_empty() && !key.starts_with("sops_") && !keys.iter().any(|k| k == key) {
            keys.push(key.to_string());
        }
    }
    keys
}

/// Merge the lines of `incoming` (a freshly decrypted dotenv document) into
/// `existing` (what was released before), keeping only keys in `only` when
/// given. A key already present is replaced in place, a new one appended —
/// so releasing one more name never drops what an earlier release granted.
pub fn merge_dotenv(existing: &str, incoming: &str, only: Option<&[String]>) -> String {
    let mut lines: Vec<String> = existing.lines().filter(|l| !l.trim().is_empty()).map(str::to_string).collect();
    for line in incoming.lines() {
        let Some(key) = dotenv_key(line) else { continue };
        if key.starts_with("sops_") || only.is_some_and(|o| !o.iter().any(|n| n == key)) {
            continue;
        }
        match lines.iter_mut().find(|l| dotenv_key(l) == Some(key)) {
            Some(slot) => *slot = line.to_string(),
            None => lines.push(line.to_string()),
        }
    }
    if lines.is_empty() { String::new() } else { lines.join("\n") + "\n" }
}

/// Bracketed-paste markers (`ESC[200~` … `ESC[201~`): a terminal wraps a
/// paste in them whenever the program in front asked for bracketed paste —
/// the tenx client does, for its embedded terminal — and a prompt reading
/// raw bytes gets them as part of the value.
const PASTE_START: &str = "\x1b[200~";
const PASTE_END: &str = "\x1b[201~";

/// Strip bracketed-paste markers from terminal input.
pub fn strip_paste_markers(raw: &str) -> String {
    raw.replace(PASTE_START, "").replace(PASTE_END, "")
}

/// Clean a secret value typed or pasted at tenx's masked prompt: drop
/// bracketed-paste markers and the line ending, then refuse anything that
/// still carries control characters (an escape sequence from the terminal,
/// a stray Ctrl key) — sealing those silently stores a value that differs
/// from the one the human meant, and nobody can see it: the prompt is masked.
/// A tab is allowed; everything else below space and DEL is not.
pub fn clean_typed_value(raw: &str) -> Result<String, String> {
    let value = strip_paste_markers(raw).trim_end_matches(['\n', '\r']).to_string();
    if let Some(c) = value.chars().find(|c| c.is_control() && *c != '\t') {
        return Err(format!(
            "the value contains a control character ({:?}) — probably terminal escape codes from the paste; nothing was set",
            c
        ));
    }
    Ok(value)
}

/// What the human is shown after a masked entry, so a wrong paste is caught
/// before it's sealed: the length, and the last four characters of anything
/// long enough that four don't give it away.
pub fn describe_value(value: &str) -> String {
    let n = value.chars().count();
    if n >= 16 {
        let tail: String = value.chars().skip(n - 4).collect();
        format!("{n} characters, ending …{tail}")
    } else {
        format!("{n} characters")
    }
}

/// Parse a `NAME<TAB>text` side file (the request reasons, the denials).
/// A line without a tab is a name with empty text.
pub fn parse_notes(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| match l.split_once('\t') {
            Some((n, t)) => (n.trim().to_string(), t.trim().to_string()),
            None => (l.trim().to_string(), String::new()),
        })
        .collect()
}

/// Inverse of [`parse_notes`]. Tabs and newlines inside the text are folded
/// to spaces so one note stays one line.
pub fn render_notes(notes: &[(String, String)]) -> String {
    notes
        .iter()
        .map(|(n, t)| format!("{n}\t{}\n", t.replace(['\t', '\n', '\r'], " ")))
        .collect()
}

/// `notes` with `name` set to `text`, replacing any earlier entry for it.
pub fn upsert_note(mut notes: Vec<(String, String)>, name: &str, text: &str) -> Vec<(String, String)> {
    notes.retain(|(n, _)| n != name);
    notes.push((name.to_string(), text.to_string()));
    notes
}

/// Tasks whose window was open in the previous listing and is gone from
/// this one — their released secrets are deleted, the same as for a window
/// closed through tenx. `now` is `None` when this listing can't be trusted
/// (tmux failed, or listed no window at all, which a running session never
/// does since it always has its home window): a failed look is not proof
/// that everything closed, so nothing is reported.
pub fn closed_windows(prev: &HashSet<PathBuf>, now: Option<&HashSet<PathBuf>>) -> Vec<PathBuf> {
    let Some(now) = now else { return Vec::new() };
    let mut closed: Vec<PathBuf> = prev.iter().filter(|p| !now.contains(p.as_path())).cloned().collect();
    closed.sort();
    closed
}

/// What a pending request asks of the human who answers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Ask {
    /// The name is sealed somewhere: release it (the passphrase does).
    Release,
    /// Nothing holds it: the human types a value, which is sealed and released.
    Value,
}

/// A whole sitting answered at once — `tenx web`'s secrets form — as what
/// to do with each name.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Refused, with the sitting's note.
    pub deny: Vec<String>,
    /// Granted value requests, the values typed for them, and where the
    /// human chose to keep each (before [`seal_scope`]).
    pub seal: Vec<(String, String, Scope)>,
    /// Granted release requests.
    pub release: Vec<String>,
    /// Granted value requests left empty: they stay pending, as an empty
    /// entry at the terminal's prompt skips one.
    pub skipped: Vec<String>,
}

impl Plan {
    /// Whether anything needs the identity unwrapped.
    pub fn needs_identity(&self) -> bool {
        !self.seal.is_empty() || !self.release.is_empty()
    }
}

/// Turn a form's answers into a [`Plan`]. `pending` is what is queued *now*,
/// `shown` what the form listed when it opened: a name answered elsewhere
/// since then is left alone, and a request that arrived after the form opened
/// is neither granted nor denied — nobody saw it, so it stays pending. Of the
/// shown names, the `granted` ones are granted and the rest denied. A typed
/// value is kept for the workspace when its name is in `workspace`, else for
/// the task. A value is cleaned like one typed at the terminal ([`clean_typed_value`]); a bad
/// one refuses the whole sitting, before anything is changed.
pub fn plan_answers(
    pending: &[(String, Ask)],
    shown: &[String],
    granted: &[String],
    values: &[(String, String)],
    workspace: &[String],
) -> Result<Plan, String> {
    let mut plan = Plan::default();
    for (name, ask) in pending.iter().filter(|(n, _)| shown.contains(n)) {
        if !granted.contains(name) {
            plan.deny.push(name.clone());
            continue;
        }
        match ask {
            Ask::Release => plan.release.push(name.clone()),
            Ask::Value => {
                let raw = values.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str()).unwrap_or("");
                let value = clean_typed_value(raw).map_err(|e| format!("{name}: {e}"))?;
                if value.is_empty() {
                    plan.skipped.push(name.clone());
                } else {
                    let scope = if workspace.contains(name) { Scope::Workspace } else { Scope::Task };
                    plan.seal.push((name.clone(), value, scope));
                }
            }
        }
    }
    Ok(plan)
}

// ── Where a value is kept ───────────────────────────────────────────────────

/// A sealed file a task's request can be answered from: the task's own
/// bundle, the workspace bundle every task of the workspace shares, or a
/// repo's own sops file (by its path inside the task).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "kind", content = "file")]
pub enum Source {
    Task,
    Workspace,
    Repo(String),
}

impl Source {
    /// How a human is shown it: `task bundle`, `workspace bundle`, the file.
    pub fn label(&self) -> String {
        match self {
            Source::Task => "task bundle".to_string(),
            Source::Workspace => "workspace bundle".to_string(),
            Source::Repo(file) => file.clone(),
        }
    }
}

/// Every source that holds `name`, in the order a request looks — the
/// task's bundle, then the workspace bundle, then the repos' files
/// (`repo_files`: those that match the name). The first one is what a
/// release uses for a bundle key, so a task's own value overrides the
/// workspace's.
pub fn sources(in_task: bool, in_workspace: bool, repo_files: &[String]) -> Vec<Source> {
    let mut found = Vec::new();
    if in_task {
        found.push(Source::Task);
    }
    if in_workspace {
        found.push(Source::Workspace);
    }
    found.extend(repo_files.iter().cloned().map(Source::Repo));
    found
}

/// What `need NAME` does with one name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// Already in the task's released files: nothing to do.
    Released,
    /// Sealed: a human grants it and the passphrase releases it, from here.
    Release(Source),
    /// Held nowhere: a human types a value.
    Value,
}

/// Route one requested name: already released → done; else the first of
/// [`sources`] → the release queue; nowhere → the value queue.
pub fn route(released: bool, in_task: bool, in_workspace: bool, repo_files: &[String]) -> Route {
    if released {
        return Route::Released;
    }
    match sources(in_task, in_workspace, repo_files).into_iter().next() {
        Some(source) => Route::Release(source),
        None => Route::Value,
    }
}

/// Split the names a release wants between the two bundles: a key of the
/// task's bundle comes from there, one only the workspace bundle has from
/// the workspace's. Names neither holds are left out (an adopted file, or
/// nothing).
pub fn bundle_release(want: &[String], task_keys: &[String], workspace_keys: &[String]) -> (Vec<String>, Vec<String>) {
    let from_task: Vec<String> = want.iter().filter(|n| task_keys.contains(n)).cloned().collect();
    let from_workspace = want.iter().filter(|n| !task_keys.contains(n) && workspace_keys.contains(n)).cloned().collect();
    (from_task, from_workspace)
}

/// Where a value a human types is kept.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// The task's own bundle: gone with the task.
    #[default]
    Task,
    /// The workspace bundle: every later task of the workspace can ask for
    /// it, and each one still needs a human to grant it.
    Workspace,
}

impl Scope {
    /// `task` / `workspace`, as `--scope` and the forms spell it.
    pub fn parse(s: &str) -> Option<Scope> {
        match s.trim().to_lowercase().as_str() {
            "task" | "t" => Some(Scope::Task),
            "workspace" | "ws" | "w" => Some(Scope::Workspace),
            _ => None,
        }
    }

    pub fn source(self) -> Source {
        match self {
            Scope::Task => Source::Task,
            Scope::Workspace => Source::Workspace,
        }
    }
}

/// Where a typed value for a name is actually sealed: where the human chose,
/// except that a name the task's bundle already holds stays there — the task
/// value overrides the workspace one, so a new workspace value would never
/// reach this task and the old task value would be released instead.
pub fn seal_scope(chosen: Scope, in_task_bundle: bool) -> Scope {
    if in_task_bundle { Scope::Task } else { chosen }
}

/// Whether `git rev-parse --is-inside-work-tree`, run in the workspace
/// directory, proves the directory is outside git — the one place the
/// workspace bundle may be written. `run` is `None` when git couldn't be
/// started, else (exit success, stdout, stderr). Only git's own "not a git
/// repository" proves it: a run that failed another way (a repo git refuses,
/// "dubious ownership") or never ran proves nothing, and the bundle is then
/// refused rather than risked inside a work tree.
pub fn outside_git(run: Option<(bool, &str, &str)>) -> bool {
    match run {
        Some((false, _, stderr)) => stderr.to_lowercase().contains("not a git repository"),
        _ => false,
    }
}

/// Whether a value just sealed into the workspace bundle answers another
/// task's waiting request for the same name: a plain value request does
/// (it moves to that task's release queue, so nobody types the value
/// again). One asked from the clipboard does not — that task asked for a
/// new value on purpose — and neither does one whose own bundle holds the
/// name: its release would take the task's old value ([`bundle_release`]),
/// the very one it asked to replace.
pub fn answers_waiting(on_value_queue: bool, from_clipboard: bool, in_its_bundle: bool) -> bool {
    on_value_queue && !from_clipboard && !in_its_bundle
}

/// One name a task can get, for `status` and the sidebar: where it is
/// sealed (in request order) and whether the task has it released now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Known {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<Source>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub released: bool,
}

impl Known {
    /// Whether a task lists it (`status`, the sidebar): released here, or
    /// sealed somewhere of the task's own. A name only the workspace bundle
    /// holds is the workspace's until this task gets it — otherwise every
    /// task would list the whole workspace bundle.
    pub fn listed(&self) -> bool {
        self.released || self.sources.iter().any(|s| *s != Source::Workspace)
    }
}

/// How often the watcher reads a task's secret names anyway, when nothing it
/// stamps moved: an adopted file can change with a pull.
pub const KNOWN_TTL_SECS: u64 = 60;

/// Whether the watcher reads a task's secret names again this tick: the
/// stamp of the files that change with a seal or a release moved, the
/// names were never read (`checked` 0), or the last read is older than
/// [`KNOWN_TTL_SECS`].
pub fn known_due(stamp: u64, saved_stamp: u64, checked: u64, now: u64) -> bool {
    checked == 0 || stamp != saved_stamp || now.saturating_sub(checked) >= KNOWN_TTL_SECS
}

/// Every name a task can get, by name: the task's bundle keys, the
/// workspace bundle's, each repo file's (`repo`: the file and its keys; a
/// file with no readable keys is listed under its own name), and what is
/// released (a released name no source holds any more is still listed).
pub fn known(task_keys: &[String], workspace_keys: &[String], repo: &[(String, Vec<String>)], released: &[String]) -> Vec<Known> {
    let mut names: Vec<String> = Vec::new();
    let mut add = |n: &String| {
        if !names.contains(n) {
            names.push(n.clone());
        }
    };
    task_keys.iter().chain(workspace_keys).chain(released).for_each(&mut add);
    for (file, keys) in repo {
        if keys.is_empty() {
            add(file);
        }
        keys.iter().for_each(&mut add);
    }
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let files: Vec<String> = repo.iter().filter(|(f, k)| k.contains(&name) || (k.is_empty() && *f == name)).map(|(f, _)| f.clone()).collect();
            Known {
                sources: sources(task_keys.contains(&name), workspace_keys.contains(&name), &files),
                released: released.contains(&name),
                name,
            }
        })
        .collect()
}

/// The clipboard's text as a secret value: cleaned like a typed one, and
/// refused when empty, so an empty clipboard is never sealed as a value.
pub fn clipboard_value(text: &str) -> Result<String, String> {
    let value = clean_typed_value(text).map_err(|e| format!("not usable as a value: {e}"))?;
    if value.is_empty() {
        return Err("the clipboard holds no text".to_string());
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn an_empty_or_garbled_clipboard_is_no_value() {
        assert_eq!(clipboard_value("sk_live_abc\n"), Ok("sk_live_abc".to_string()));
        assert!(clipboard_value("").is_err());
        assert!(clipboard_value("\n").is_err());
        assert!(clipboard_value("a\x1b[Ab").is_err());
    }


    fn t(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn still_queued_is_pending_whatever_the_outputs_say() {
        assert_eq!(wait_outcome(true, false, &[t(200)], t(100)), WaitOutcome::Pending);
        assert_eq!(wait_outcome(true, true, &[], t(100)), WaitOutcome::Pending);
    }

    #[test]
    fn dequeued_with_a_fresh_output_is_fulfilled() {
        assert_eq!(wait_outcome(false, false, &[t(150)], t(100)), WaitOutcome::Fulfilled);
        // Written in the same instant as the request still counts.
        assert_eq!(wait_outcome(false, false, &[t(100)], t(100)), WaitOutcome::Fulfilled);
        // One fresh output among stale ones is enough.
        assert_eq!(wait_outcome(false, false, &[t(10), t(150)], t(100)), WaitOutcome::Fulfilled);
    }

    #[test]
    fn dequeued_with_only_stale_outputs_is_cancelled() {
        // A plaintext left over from an earlier unlock doesn't satisfy a
        // request made after it.
        assert_eq!(wait_outcome(false, false, &[t(50)], t(100)), WaitOutcome::Cancelled);
        assert_eq!(wait_outcome(false, false, &[], t(100)), WaitOutcome::Cancelled);
    }

    #[test]
    fn a_denial_wins_over_an_output_written_for_another_name() {
        assert_eq!(wait_outcome(false, true, &[t(150)], t(100)), WaitOutcome::Denied);
    }

    #[test]
    fn exit_code_prefers_the_outcome_that_changes_the_next_step() {
        use WaitOutcome::*;
        assert_eq!(exit_code(&[Fulfilled, Fulfilled]), 0);
        assert_eq!(exit_code(&[]), 0);
        assert_eq!(exit_code(&[Fulfilled, Pending]), EXIT_PENDING);
        assert_eq!(exit_code(&[Pending, Cancelled]), EXIT_WITHDRAWN);
        assert_eq!(exit_code(&[Cancelled, Denied, Pending]), EXIT_DENIED);
    }

    #[test]
    fn names_are_single_tokens() {
        assert!(valid_name("STRIPE_KEY"));
        assert!(valid_name("staging"));
        assert!(valid_name("secrets.prod.enc.env"));
        for bad in ["", ".", "..", "a b", "a\tb", "a/b", "a\"b", "A=1"] {
            assert!(!valid_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn keys_of_a_sealed_document_are_readable() {
        let sealed = "A=ENC[AES256_GCM,data:RQ==]\nB=ENC[AES256_GCM,data:DC==]\n\
                      sops_age__list_0__map_recipient=age1xyz\nsops_mac=ENC[...]\nsops_version=3.13.2\n";
        assert_eq!(dotenv_keys(sealed), ["A", "B"]);
        assert_eq!(dotenv_keys("# c\n\nexport X=1\n Y = 2\nnoequals\n"), ["X", "Y"]);
    }

    #[test]
    fn sealed_keys_in_every_sops_format() {
        let env = "A=ENC[AES256_GCM,data:RQ==]\nPLAIN_unencrypted=x\nsops_mac=ENC[AES256_GCM,data:x]\n";
        assert_eq!(sealed_keys(env), ["A"]);
        let yaml = "db:\n    password: ENC[AES256_GCM,data:x]\ntoken: ENC[AES256_GCM,data:y]\nsops:\n    mac: ENC[AES256_GCM,data:z]\n";
        assert_eq!(sealed_keys(yaml), ["password", "token"]);
        let json = "{\n\t\"API_KEY\": \"ENC[AES256_GCM,data:x]\",\n\t\"sops\": {\n\t\t\"mac\": \"ENC[AES256_GCM,data:z]\"\n\t}\n}\n";
        assert_eq!(sealed_keys(json), ["API_KEY"]);
    }

    #[test]
    fn merge_keeps_earlier_releases_and_filters_new_ones() {
        let existing = "A=old\nC=3\n";
        let incoming = "A=new\nB=2\nD=4\n";
        let only = ["A".to_string(), "B".to_string()];
        assert_eq!(merge_dotenv(existing, incoming, Some(&only)), "A=new\nC=3\nB=2\n");
        assert_eq!(merge_dotenv("", incoming, None), "A=new\nB=2\nD=4\n");
        assert_eq!(merge_dotenv("", incoming, Some(&[])), "");
    }

    #[test]
    fn pasted_values_lose_their_bracketed_paste_markers() {
        assert_eq!(clean_typed_value("\x1b[200~sntrys_abc123\x1b[201~\n"), Ok("sntrys_abc123".to_string()));
        assert_eq!(clean_typed_value("typed\r\n"), Ok("typed".to_string()));
        assert_eq!(clean_typed_value("with\ttab"), Ok("with\ttab".to_string()));
        assert_eq!(clean_typed_value(""), Ok(String::new()));
    }

    #[test]
    fn other_control_characters_are_refused_not_sealed() {
        assert!(clean_typed_value("abc\x1b[Adef").is_err());
        assert!(clean_typed_value("abc\x7f").is_err());
        assert!(clean_typed_value("a\nb").is_err());
    }

    #[test]
    fn describing_a_value_shows_a_tail_only_when_long() {
        assert_eq!(describe_value("short"), "5 characters");
        assert_eq!(describe_value("sntrys_0123456789abcdef"), "23 characters, ending …cdef");
    }

    #[test]
    fn notes_round_trip_one_line_each() {
        let notes = upsert_note(vec![("A".into(), "x".into())], "B", "run the\ttests\nnow");
        let notes = upsert_note(notes, "A", "y");
        let text = render_notes(&notes);
        assert_eq!(text, "B\trun the tests now\nA\ty\n");
        assert_eq!(parse_notes(&text), notes.iter().map(|(n, t)| (n.clone(), t.replace(['\t', '\n'], " "))).collect::<Vec<_>>());
        assert_eq!(parse_notes("LONE\n"), [("LONE".to_string(), String::new())]);
    }

    fn set(dirs: &[&str]) -> HashSet<PathBuf> {
        dirs.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn a_window_gone_since_the_last_listing_is_closed() {
        let prev = set(&["/ws/tasks/a", "/ws/tasks/b"]);
        let now = set(&["/ws/tasks/b", "/ws/tasks/c"]);
        assert_eq!(closed_windows(&prev, Some(&now)), vec![PathBuf::from("/ws/tasks/a")]);
    }

    #[test]
    fn a_listing_that_cannot_be_trusted_closes_nothing() {
        let prev = set(&["/ws/tasks/a", "/ws/tasks/b"]);
        assert!(closed_windows(&prev, None).is_empty());
    }

    #[test]
    fn a_newly_opened_window_is_not_closed() {
        let prev = set(&[]);
        let now = set(&["/ws/tasks/a"]);
        assert!(closed_windows(&prev, Some(&now)).is_empty());
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_form_grants_the_ticked_and_denies_the_rest_it_showed() {
        let pending = vec![("A".to_string(), Ask::Release), ("B".to_string(), Ask::Value), ("C".to_string(), Ask::Release)];
        let values = vec![("B".to_string(), "s3cret".to_string())];
        let plan = plan_answers(&pending, &names(&["A", "B", "C"]), &names(&["A", "B"]), &values, &[]).unwrap();
        assert_eq!(
            plan,
            Plan { deny: names(&["C"]), seal: vec![("B".into(), "s3cret".into(), Scope::Task)], release: names(&["A"]), skipped: vec![] }
        );
        assert!(plan.needs_identity());
    }

    #[test]
    fn a_form_leaves_alone_what_it_never_showed_or_what_was_answered_since() {
        // D arrived after the form opened; A was answered in another tab.
        let pending = vec![("B".to_string(), Ask::Release), ("D".to_string(), Ask::Value)];
        let plan = plan_answers(&pending, &names(&["A", "B"]), &names(&["A"]), &[], &[]).unwrap();
        assert_eq!(plan, Plan { deny: names(&["B"]), ..Plan::default() });
        assert!(!plan.needs_identity());
    }

    #[test]
    fn an_empty_value_skips_and_a_control_character_refuses_the_sitting() {
        let pending = vec![("B".to_string(), Ask::Value)];
        let plan = plan_answers(&pending, &names(&["B"]), &names(&["B"]), &[], &[]).unwrap();
        assert_eq!(plan, Plan { skipped: names(&["B"]), ..Plan::default() });
        let bad = vec![("B".to_string(), "x\x1b[A".to_string())];
        assert!(plan_answers(&pending, &names(&["B"]), &names(&["B"]), &bad, &[]).unwrap_err().starts_with("B: "));
        let pasted = vec![("B".to_string(), "token\n".to_string())];
        let plan = plan_answers(&pending, &names(&["B"]), &names(&["B"]), &pasted, &names(&["B"])).unwrap();
        assert_eq!(plan.seal, vec![("B".to_string(), "token".to_string(), Scope::Workspace)]);
    }

    #[test]
    fn a_request_looks_in_the_task_then_the_workspace_then_the_repos() {
        let repo = names(&["app/secrets.staging.enc.env"]);
        assert_eq!(route(true, true, true, &repo), Route::Released);
        assert_eq!(route(false, true, true, &repo), Route::Release(Source::Task));
        assert_eq!(route(false, false, true, &repo), Route::Release(Source::Workspace));
        assert_eq!(route(false, false, false, &repo), Route::Release(Source::Repo("app/secrets.staging.enc.env".into())));
        assert_eq!(route(false, false, false, &[]), Route::Value);
        assert_eq!(sources(true, true, &repo).iter().map(Source::label).collect::<Vec<_>>(), ["task bundle", "workspace bundle", "app/secrets.staging.enc.env"]);
    }

    #[test]
    fn a_task_value_overrides_the_workspace_one_on_release() {
        let want = names(&["A", "B", "C", "staging"]);
        let (task, ws) = bundle_release(&want, &names(&["A"]), &names(&["A", "B"]));
        assert_eq!(task, names(&["A"]));
        assert_eq!(ws, names(&["B"]));
        let (task, ws) = bundle_release(&want, &[], &[]);
        assert!(task.is_empty() && ws.is_empty());
    }

    #[test]
    fn a_typed_value_goes_where_the_human_chose_unless_the_task_bundle_holds_it() {
        assert_eq!(seal_scope(Scope::Task, false), Scope::Task);
        assert_eq!(seal_scope(Scope::Workspace, false), Scope::Workspace);
        // The task's value would hide a new workspace one from this task.
        assert_eq!(seal_scope(Scope::Workspace, true), Scope::Task);
        assert_eq!(Scope::default(), Scope::Task);
        assert_eq!(Scope::parse("workspace"), Some(Scope::Workspace));
        assert_eq!(Scope::parse(" W "), Some(Scope::Workspace));
        assert_eq!(Scope::parse(""), None);
        assert_eq!(Scope::parse("repo"), None);
    }

    #[test]
    fn known_names_say_where_each_is_sealed_and_whether_it_is_released() {
        let repo = vec![("app/secrets.enc.env".to_string(), names(&["DB_URL"])), ("app/cert.enc.pem".to_string(), vec![])];
        let k = known(&names(&["A"]), &names(&["A", "W"]), &repo, &names(&["W", "OLD"]));
        let by = |n: &str| k.iter().find(|x| x.name == n).unwrap().clone();
        assert_eq!(k.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["A", "DB_URL", "OLD", "W", "app/cert.enc.pem"]);
        assert_eq!(by("A").sources, [Source::Task, Source::Workspace]);
        assert!(!by("A").released);
        assert_eq!(by("W"), Known { name: "W".into(), sources: vec![Source::Workspace], released: true });
        assert_eq!(by("DB_URL").sources, [Source::Repo("app/secrets.enc.env".into())]);
        assert_eq!(by("app/cert.enc.pem").sources, [Source::Repo("app/cert.enc.pem".into())]);
        assert!(by("OLD").sources.is_empty() && by("OLD").released);
    }

    #[test]
    fn only_git_saying_no_repository_lets_the_workspace_bundle_be_written() {
        assert!(outside_git(Some((false, "", "fatal: not a git repository (or any of the parent directories): .git"))));
        assert!(!outside_git(Some((true, "true\n", ""))));
        assert!(!outside_git(Some((true, "false\n", ""))), "inside a .git directory is still git's");
        assert!(!outside_git(Some((false, "", "fatal: detected dubious ownership in repository"))));
        assert!(!outside_git(None), "git that never ran proves nothing");
    }

    #[test]
    fn a_new_workspace_value_answers_plain_waiting_requests_only() {
        assert!(answers_waiting(true, false, false));
        assert!(!answers_waiting(true, true, false));
        assert!(!answers_waiting(false, false, false));
        // A rotation of the task's own value: a release would hand back the old one.
        assert!(!answers_waiting(true, false, true));
    }

    #[test]
    fn a_task_lists_workspace_names_only_once_it_has_them() {
        let ws_only = Known { name: "W".into(), sources: vec![Source::Workspace], released: false };
        assert!(!ws_only.listed());
        assert!(Known { released: true, ..ws_only.clone() }.listed());
        assert!(Known { sources: vec![Source::Task, Source::Workspace], ..ws_only.clone() }.listed());
        assert!(Known { sources: vec![Source::Repo("a.enc.env".into())], ..ws_only }.listed());
    }

    #[test]
    fn secret_names_are_read_again_when_their_files_move_or_after_a_while() {
        assert!(!known_due(7, 7, 100, 130));
        assert!(known_due(8, 7, 100, 130));
        assert!(known_due(7, 7, 100, 100 + KNOWN_TTL_SECS));
        assert!(known_due(7, 7, 0, 5), "never read");
    }
}
