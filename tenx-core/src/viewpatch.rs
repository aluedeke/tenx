//! What changed between two column views, as a `patch` message's body
//! (`docs/web-protocol.md`): `tenx web` sends the whole `ColumnView` once per
//! socket, then only this. A view is ~40 KB, almost all of it the list's
//! rows, and a cursor move changes two of them — over a relayed connection
//! the whole view after every key is what made the page slow.
//!
//! The body has up to three parts:
//! - `set`: every top-level field whose value changed (or is new), whole;
//! - `unset`: every top-level field that is gone;
//! - `items`: when the rows changed, the new list, each entry either the
//!   index of an identical row in the old list (a number) or a row (an
//!   object). Rows are matched by value, not by position, so a task moving
//!   to another section — which shifts every row after it — still costs only
//!   the rows that really changed.
//!
//! The page applies it to the last view it got on the same socket
//! (`web/src/lib/viewpatch.ts`).

use std::collections::HashMap;

use serde_json::{Map, Value};

/// The patch from `old` to `new`, or `None` when they are equal. Both are
/// `ColumnView`s serialized to JSON objects; anything else gets the whole
/// of `new` under `set`.
pub fn view_patch(old: &Value, new: &Value) -> Option<Value> {
    if old == new {
        return None;
    }
    let (Some(old), Some(new)) = (old.as_object(), new.as_object()) else {
        let mut set = Map::new();
        if let Some(new) = new.as_object() {
            set.clone_from(new);
        }
        return Some(Value::Object(Map::from_iter([("set".to_string(), Value::Object(set))])));
    };
    let mut set = Map::new();
    let mut items = None;
    for (key, value) in new {
        if old.get(key) == Some(value) {
            continue;
        }
        match (key.as_str(), old.get(key), value) {
            ("items", Some(Value::Array(was)), Value::Array(now)) => items = Some(Value::Array(rows_patch(was, now))),
            _ => {
                set.insert(key.clone(), value.clone());
            }
        }
    }
    let unset: Vec<Value> = old.keys().filter(|k| !new.contains_key(*k)).map(|k| Value::String(k.clone())).collect();
    let mut patch = Map::new();
    if !set.is_empty() {
        patch.insert("set".into(), Value::Object(set));
    }
    if !unset.is_empty() {
        patch.insert("unset".into(), Value::Array(unset));
    }
    if let Some(items) = items {
        patch.insert("items".into(), items);
    }
    Some(Value::Object(patch))
}

/// `now`, with every row that `was` already has replaced by its index there.
/// Equal rows are paired in order, each old row used once.
fn rows_patch(was: &[Value], now: &[Value]) -> Vec<Value> {
    let mut free: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, row) in was.iter().enumerate().rev() {
        free.entry(row.to_string()).or_default().push(i);
    }
    now.iter()
        .map(|row| match free.get_mut(&row.to_string()).and_then(Vec::pop) {
            Some(i) => Value::from(i),
            None => row.clone(),
        })
        .collect()
}

/// `patch` applied to `old` — what the page does, for the tests and for
/// checking a patch before it is sent.
pub fn apply(old: &Value, patch: &Value) -> Value {
    let mut out = old.as_object().cloned().unwrap_or_default();
    if let Some(set) = patch.get("set").and_then(Value::as_object) {
        for (k, v) in set {
            out.insert(k.clone(), v.clone());
        }
    }
    if let Some(unset) = patch.get("unset").and_then(Value::as_array) {
        for k in unset.iter().filter_map(Value::as_str) {
            out.remove(k);
        }
    }
    if let Some(items) = patch.get("items").and_then(Value::as_array) {
        let was = old.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
        let rows = items
            .iter()
            .map(|e| match e.as_u64() {
                Some(i) => was.get(i as usize).cloned().unwrap_or(Value::Null),
                None => e.clone(),
            })
            .collect();
        out.insert("items".into(), Value::Array(rows));
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn task(id: &str, selected: bool) -> Value {
        json!({ "kind": "task", "id": id, "title": format!("Task {id}"), "selected": selected })
    }

    fn view(items: Vec<Value>, focus: &str) -> Value {
        json!({ "tabs": [{ "label": "Tasks", "active": true }], "focus": focus, "items": items, "help": [["keys", "…"]] })
    }

    #[test]
    fn equal_views_need_no_patch() {
        let v = view(vec![task("a", false)], "list");
        assert_eq!(view_patch(&v, &v.clone()), None);
    }

    #[test]
    fn a_cursor_move_sends_only_the_two_rows_it_changed() {
        let old = view(vec![task("a", true), task("b", false), task("c", false)], "list");
        let new = view(vec![task("a", false), task("b", true), task("c", false)], "list");
        let p = view_patch(&old, &new).unwrap();
        assert_eq!(p.get("set"), None, "nothing but the rows changed");
        assert_eq!(p["items"], json!([task("a", false), task("b", true), 2]));
        assert_eq!(apply(&old, &p), new);
    }

    #[test]
    fn a_task_moving_section_reuses_the_shifted_rows() {
        let h = |l: &str| json!({ "kind": "header", "label": l });
        let old = view(vec![h("WORKING"), task("a", false), task("b", false), h("IDLE"), task("c", false)], "list");
        let new = view(vec![h("WORKING"), task("b", false), h("IDLE"), task("a", false), task("c", false)], "list");
        let p = view_patch(&old, &new).unwrap();
        assert_eq!(p["items"], json!([0, 2, 3, 1, 4]));
        assert_eq!(apply(&old, &p), new);
    }

    #[test]
    fn equal_rows_each_pair_with_one_old_row() {
        let e = json!({ "kind": "empty", "lines": [] });
        let old = view(vec![e.clone()], "list");
        let new = view(vec![e.clone(), e.clone()], "list");
        let p = view_patch(&old, &new).unwrap();
        assert_eq!(p["items"], json!([0, e]));
        assert_eq!(apply(&old, &p), new);
    }

    #[test]
    fn top_level_fields_are_set_whole_and_dropped_ones_unset() {
        let old = view(vec![task("a", false)], "list");
        let mut new = view(vec![task("a", false)], "search");
        new.as_object_mut().unwrap().remove("help");
        new.as_object_mut().unwrap().insert("status".into(), json!("saved"));
        let p = view_patch(&old, &new).unwrap();
        assert_eq!(p["set"], json!({ "focus": "search", "status": "saved" }));
        assert_eq!(p["unset"], json!(["help"]));
        assert_eq!(p.get("items"), None);
        assert_eq!(apply(&old, &p), new);
    }
}
