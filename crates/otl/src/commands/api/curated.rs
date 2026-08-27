//! Which curated command covers which operation.
//!
//! # Why this table exists
//!
//! `otl api list` names 116 operations. `otl` also ships around two dozen
//! curated commands whose flags and output are a semver contract, while
//! `otl api` output is explicitly unstable. Nothing in `api list` said which
//! of the 116 already had a stable front door, so the natural reading of a
//! long list of operations was "call the operation" - the less stable of the
//! two paths, chosen because the more stable one was invisible.
//!
//! This is the reverse index. `otl api list --json` carries it as
//! `curated_command`: a string naming the command to prefer, or null when
//! `otl api` really is the only way in.
//!
//! # What belongs here
//!
//! The PRIMARY subject of a curated command, not every operation it happens
//! to touch. `otl docs search` also calls `collections.list` to label a
//! table column, and `otl collections list` calls `collections.documents`
//! to count rows; neither makes those commands the way to reach those
//! operations. Where a flag is what selects the operation, the flag is part
//! of the answer - `documents.archive` is `otl docs delete --archive`, not
//! `otl docs delete`.
//!
//! `tests/curated_index.rs` holds this to three rules: every operation named
//! here exists in the table this binary dispatches from, every command named
//! here exists in the command tree, and every operation a curated command's
//! own `--help` names appears here somewhere. The third is the one that
//! catches a new curated command whose author forgot this file.

/// Operation name -> the curated command to prefer for it.
///
/// Kept in operation-name order, which is the order `api list` prints.
pub(super) const CURATED_COMMANDS: &[(&str, &str)] = &[
    ("attachments.create", "otl attachments create"),
    ("attachments.redirect", "otl fetch attachment"),
    ("auth.info", "otl auth info"),
    ("collections.archive", "otl collections delete --archive"),
    ("collections.create", "otl collections create"),
    ("collections.delete", "otl collections delete"),
    ("collections.documents", "otl fetch collection"),
    ("collections.info", "otl fetch collection"),
    ("collections.list", "otl collections list"),
    ("collections.update", "otl collections update"),
    ("comments.create", "otl comments create"),
    ("comments.delete", "otl comments delete"),
    ("comments.list", "otl comments list"),
    ("comments.resolve", "otl comments update --resolve"),
    ("comments.unresolve", "otl comments update --unresolve"),
    ("comments.update", "otl comments update"),
    ("documents.archive", "otl docs delete --archive"),
    ("documents.create", "otl docs create"),
    ("documents.delete", "otl docs delete"),
    ("documents.info", "otl docs view"),
    ("documents.list", "otl docs list"),
    ("documents.move", "otl docs move"),
    ("documents.search", "otl docs search"),
    ("documents.update", "otl docs update"),
    ("users.info", "otl fetch user"),
    ("users.list", "otl users list"),
];

/// The curated command for `operation`, if there is one.
pub(super) fn curated_command(operation: &str) -> Option<&'static str> {
    CURATED_COMMANDS
        .iter()
        .find(|(name, _)| *name == operation)
        .map(|(_, command)| *command)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn the_table_is_sorted_so_a_new_entry_lands_where_it_is_looked_for() {
        let names: Vec<&str> = CURATED_COMMANDS.iter().map(|(name, _)| *name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
    }

    #[test]
    fn no_operation_is_listed_twice() {
        let mut names: Vec<&str> = CURATED_COMMANDS.iter().map(|(name, _)| *name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "duplicate operation in the table");
    }

    #[test]
    fn a_flag_selected_operation_names_the_flag() {
        assert_eq!(
            curated_command("documents.archive"),
            Some("otl docs delete --archive")
        );
        assert_eq!(curated_command("documents.delete"), Some("otl docs delete"));
    }

    #[test]
    fn an_operation_with_no_curated_front_door_answers_none() {
        assert_eq!(curated_command("documents.duplicate"), None);
        assert_eq!(curated_command("nope.nope"), None);
    }
}
