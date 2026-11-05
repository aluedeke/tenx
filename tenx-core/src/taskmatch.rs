//! A task named the way a person (or a voice assistant) names it: its slug,
//! `workspace/slug`, its title, or a few words of the title — so an MCP
//! client can act on a task without listing every task first to look up its
//! slug. One task matches, or the error names the candidates.

use crate::slug::slugify;

/// One task as the matcher sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskName {
    pub workspace: String,
    pub slug: String,
    pub title: String,
}

/// How many candidates an ambiguous name lists.
const SHOWN: usize = 8;

/// The index in `tasks` that `query` names, `workspace` narrowing the search.
/// Tried in order, the first rule with any match decides: the exact slug,
/// the exact title, then every word of the query starting a word of the slug
/// or title ("voice app" finds "Voice app for tenx"), then the query's
/// letters run together inside the slug's ("pipe cat" finds `pipecat` —
/// speech recognition splits and joins words). `tasks` comes in the order a
/// list shows, so candidates are listed most recently active first.
pub fn find(tasks: &[TaskName], workspace: Option<&str>, query: &str) -> Result<usize, String> {
    let query = query.trim();
    let (workspace, query) = match (workspace, query.split_once('/')) {
        (None, Some((ws, rest))) if tasks.iter().any(|t| t.workspace == ws) => (Some(ws), rest),
        _ => (workspace, query),
    };
    if slugify(query).is_empty() {
        return Err("name a task: its slug or words from its title".into());
    }
    let pool: Vec<usize> = (0..tasks.len()).filter(|&i| workspace.is_none_or(|w| tasks[i].workspace == w)).collect();
    if pool.is_empty() {
        return Err(format!("no workspace named {:?} (tenx_list_workspaces lists them)", workspace.unwrap_or_default()));
    }
    let wanted = slugify(query);
    let words: Vec<&str> = wanted.split('-').collect();
    let joined = wanted.replace('-', "");
    let rules: [&dyn Fn(&TaskName) -> bool; 4] = [
        &|t| t.slug == query || t.slug == wanted,
        &|t| t.title.trim().eq_ignore_ascii_case(query),
        &|t| {
            let own: Vec<String> = [slugify(&t.slug), slugify(&t.title)].join("-").split('-').map(str::to_string).collect();
            words.iter().all(|w| own.iter().any(|o| o.starts_with(w)))
        },
        &|t| joined.len() >= 3 && t.slug.replace('-', "").contains(&joined),
    ];
    for rule in rules {
        let hits: Vec<usize> = pool.iter().copied().filter(|&i| rule(&tasks[i])).collect();
        match hits.as_slice() {
            [] => continue,
            [one] => return Ok(*one),
            many => return Err(ambiguous(tasks, query, many)),
        }
    }
    Err(format!("no task matches {query:?} (tenx_list_tasks lists them)"))
}

/// Every task `query`'s words (or letters run together) match, in list
/// order: `tenx_list_tasks`'s search. An empty query matches everything.
pub fn search(tasks: &[TaskName], query: &str) -> Vec<usize> {
    let wanted = slugify(query);
    if wanted.is_empty() {
        return (0..tasks.len()).collect();
    }
    let words: Vec<&str> = wanted.split('-').collect();
    let joined = wanted.replace('-', "");
    (0..tasks.len())
        .filter(|&i| {
            let t = &tasks[i];
            let own = [slugify(&t.workspace), slugify(&t.slug), slugify(&t.title)].join("-");
            let own: Vec<&str> = own.split('-').collect();
            words.iter().all(|w| own.iter().any(|o| o.starts_with(w))) || t.slug.replace('-', "").contains(&joined)
        })
        .collect()
}

fn ambiguous(tasks: &[TaskName], query: &str, hits: &[usize]) -> String {
    let names: Vec<String> =
        hits.iter().take(SHOWN).map(|&i| format!("{}/{} \"{}\"", tasks[i].workspace, tasks[i].slug, tasks[i].title)).collect();
    let more = if hits.len() > SHOWN { format!(" and {} more", hits.len() - SHOWN) } else { String::new() };
    format!("{} tasks match {query:?}: {}{more}. Name one by workspace/slug.", hits.len(), names.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(workspace: &str, slug: &str, title: &str) -> TaskName {
        TaskName { workspace: workspace.into(), slug: slug.into(), title: title.into() }
    }

    fn tasks() -> Vec<TaskName> {
        vec![
            t("tenx-workspace", "pipecat", "pipecat"),
            t("mealstack", "sentry", "Sentry"),
            t("checkly-rum", "checkly-in-sentry", "checkly in sentry"),
            t("tenx-workspace", "ios-app-for-tenx-web", "iOS app for tenx web"),
            t("homelab", "agent", "agent"),
            t("agent-agency", "agent-agency-assistant", "agent-agency-assistant"),
            t("checkly-rum", "claude-design", "claude design"),
            t("mealstack", "claude-design", "claude design"),
        ]
    }

    #[test]
    fn an_exact_slug_wins_over_words_that_match_more() {
        assert_eq!(find(&tasks(), None, "sentry"), Ok(1));
        assert_eq!(find(&tasks(), None, "agent"), Ok(4));
    }

    #[test]
    fn workspace_slash_slug_names_one_of_two_namesakes() {
        assert_eq!(find(&tasks(), None, "mealstack/claude-design"), Ok(7));
        assert_eq!(find(&tasks(), Some("checkly-rum"), "claude design"), Ok(6));
    }

    #[test]
    fn namesakes_without_a_workspace_are_listed() {
        let err = find(&tasks(), None, "claude design").unwrap_err();
        assert!(err.starts_with("2 tasks match"), "{err}");
        assert!(err.contains("checkly-rum/claude-design") && err.contains("mealstack/claude-design"), "{err}");
    }

    #[test]
    fn title_words_find_a_task_in_any_case() {
        assert_eq!(find(&tasks(), None, "iOS App"), Ok(3));
        assert_eq!(find(&tasks(), None, "the ios app").unwrap_err(), "no task matches \"the ios app\" (tenx_list_tasks lists them)");
        assert_eq!(find(&tasks(), None, "checkly sentry"), Ok(2));
    }

    #[test]
    fn words_split_by_speech_still_find_the_slug() {
        assert_eq!(find(&tasks(), None, "pipe cat"), Ok(0));
        assert_eq!(find(&tasks(), None, "Pipecat."), Ok(0));
    }

    #[test]
    fn an_unknown_workspace_or_an_empty_name_is_an_error() {
        assert!(find(&tasks(), Some("nope"), "pipecat").unwrap_err().contains("no workspace named"));
        assert!(find(&tasks(), None, " ? ").is_err());
    }

    #[test]
    fn a_slash_that_is_no_workspace_stays_in_the_query() {
        assert_eq!(find(&tasks(), None, "ios/app"), Ok(3));
    }

    #[test]
    fn search_lists_every_match_in_order() {
        assert_eq!(search(&tasks(), "sentry"), vec![1, 2]);
        assert_eq!(search(&tasks(), "mealstack"), vec![1, 7]);
        assert_eq!(search(&tasks(), "").len(), tasks().len());
    }
}
