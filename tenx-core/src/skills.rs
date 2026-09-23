//! Keeping the skill files `tenx init` installed current.
//!
//! `tenx init` writes the `/tenx` and `/standup` skills (a Claude copy and a
//! portable one each) and `AGENTS.md` into a workspace once, and nothing else
//! ever touched them — so a workspace kept telling its agents whatever the
//! tenx it was created with said, long after the commands changed. The fix
//! is to refresh them, but they are the user's files too: an `AGENTS.md` in
//! particular is made to be edited. So a file is only replaced when it is
//! provably tenx's own words: byte-for-byte some version tenx has shipped,
//! recognised by its [`content_hash`] in the binary's list of every shipped
//! rendering. Anything else was edited and is left alone.

/// FNV-1a over the bytes — identifies a shipped rendering, nothing more
/// (not a security boundary: the worst a collision does is refresh a file
/// that happens to hash like one tenx shipped).
pub fn content_hash(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Where an installed skill file stands against what this binary ships.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillState {
    /// Identical to the current version.
    Current,
    /// An older version tenx shipped, untouched since — safe to replace.
    Stale,
    /// Neither: someone edited it. Never replaced automatically.
    Edited,
}

/// Decide `installed`'s state given the `current` rendering and the hashes of
/// every rendering tenx has ever shipped (`shipped`).
pub fn skill_state(installed: &str, current: &str, shipped: &[u64]) -> SkillState {
    if installed == current {
        SkillState::Current
    } else if shipped.contains(&content_hash(installed.as_bytes())) {
        SkillState::Stale
    } else {
        SkillState::Edited
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_fnv1a() {
        assert_eq!(content_hash(b""), 0xcbf29ce484222325);
        assert_eq!(content_hash(b"a"), 0xaf63dc4c8601ec8c);
    }

    #[test]
    fn only_a_shipped_version_is_replaced() {
        let old = "old skill\n";
        let shipped = [content_hash(old.as_bytes())];
        assert_eq!(skill_state("new skill\n", "new skill\n", &shipped), SkillState::Current);
        assert_eq!(skill_state(old, "new skill\n", &shipped), SkillState::Stale);
        assert_eq!(skill_state("old skill\nmy note\n", "new skill\n", &shipped), SkillState::Edited);
    }
}
