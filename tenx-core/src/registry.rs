//! The workspace registry (`~/.config/tenx/workspaces.d/`): which entries
//! are dead and may be deleted.

/// What reading one registry entry and its workspace came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Load {
    /// The workspace's `config.toml` was read and parsed.
    Loaded,
    /// The entry file itself isn't a registry entry.
    BadEntry,
    /// The directory it names holds no `config.toml`: moved or deleted.
    Missing,
    /// `config.toml` is there, but reading or parsing it failed — a folder
    /// macOS keeps from a background process (iCloud Drive without the
    /// privacy permission is "Operation not permitted"), a typo in the file.
    Unreadable,
    /// The read hasn't returned in time and is still going: macOS holds a
    /// LaunchAgent's `open` of an iCloud Drive file until the permission is
    /// answered.
    Slow,
}

/// Whether to delete the entry. Only one that names nothing: a workspace that
/// exists but can't be read *now* is skipped and kept, since a permission
/// granted later, or a fixed `config.toml`, brings it back.
pub fn prune(load: Load) -> bool {
    matches!(load, Load::BadEntry | Load::Missing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_dead_entries_are_pruned() {
        assert!(prune(Load::BadEntry));
        assert!(prune(Load::Missing));
        assert!(!prune(Load::Loaded));
        assert!(!prune(Load::Unreadable));
        assert!(!prune(Load::Slow));
    }
}
