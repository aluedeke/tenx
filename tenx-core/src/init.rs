//! Where `tenx init` puts a workspace.
//!
//! `tenx init` with no name makes the current directory the workspace;
//! `tenx init <name>` makes a new `<name>/` inside it. Typed from inside a
//! directory already called `<name>` — `mkdir homelab && cd homelab && tenx
//! init homelab` — that is `homelab/homelab`, which is never what was meant,
//! and moving it up afterwards strands the registry entry at the old path.

/// The directory `tenx init` would nest in is already named after the
/// workspace: refuse rather than create `<name>/<name>`.
pub fn nests_in_namesake(cwd_name: &str, name: &str) -> bool {
    !name.is_empty() && cwd_name == name.trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_matching_the_directory_would_nest() {
        assert!(nests_in_namesake("homelab", "homelab"));
        assert!(nests_in_namesake("homelab", "homelab/"));
    }

    #[test]
    fn another_name_is_a_new_subdirectory() {
        assert!(!nests_in_namesake("aluedeke", "homelab"));
        assert!(!nests_in_namesake("homelab", "homelab-infra"));
        assert!(!nests_in_namesake("homelab", ""));
    }
}
