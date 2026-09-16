use std::path::Path;

use crate::models::UserConfig;
use crate::paths::MIX_MANAGED_USERS_DIR;

pub fn managed_uids() -> Vec<u32> {
    read_uids(Path::new(MIX_MANAGED_USERS_DIR))
}

pub fn trusted_users(user_config: Option<&UserConfig>) -> Vec<String> {
    let names = union_names(
        managed_uids(),
        name_of_uid,
        user_config.map(|cfg| cfg.user.name.as_str()),
    );
    tracing::debug!("trusted users: {}", names.join(", "));
    names
}

fn read_uids(dir: &Path) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut uids: Vec<u32> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
        .collect();
    uids.sort_unstable();
    uids.dedup();
    uids
}

fn name_of_uid(uid: u32) -> Option<String> {
    nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))
        .ok()
        .flatten()
        .map(|user| user.name)
}

fn union_names(
    uids: Vec<u32>,
    resolve: impl Fn(u32) -> Option<String>,
    invoking: Option<&str>,
) -> Vec<String> {
    let mut names: Vec<String> = uids.into_iter().filter_map(resolve).collect();
    names.extend(invoking.map(str::to_string));
    names.sort_unstable();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolver(pairs: &'static [(u32, &'static str)]) -> impl Fn(u32) -> Option<String> {
        move |uid| {
            pairs
                .iter()
                .find(|(candidate, _)| *candidate == uid)
                .map(|(_, name)| (*name).to_string())
        }
    }

    #[test]
    fn read_uids_is_empty_when_the_marker_directory_does_not_exist() {
        let root = tempfile::tempdir().unwrap();
        assert!(read_uids(&root.path().join("absent")).is_empty());
    }

    #[test]
    fn read_uids_returns_every_marker_sorted() {
        let dir = tempfile::tempdir().unwrap();
        for uid in ["1001", "1000", "1002"] {
            std::fs::write(dir.path().join(uid), "").unwrap();
        }
        assert_eq!(read_uids(dir.path()), vec![1000, 1001, 1002]);
    }

    #[test]
    fn read_uids_ignores_entries_that_are_not_uids() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("1000"), "").unwrap();
        std::fs::write(dir.path().join("not-a-uid"), "").unwrap();
        std::fs::write(dir.path().join("-1"), "").unwrap();
        assert_eq!(read_uids(dir.path()), vec![1000]);
    }

    #[test]
    fn union_names_resolves_every_marked_uid() {
        let names = union_names(
            vec![1000, 1001],
            resolver(&[(1000, "alice"), (1001, "bob")]),
            None,
        );
        assert_eq!(names, vec!["alice".to_string(), "bob".to_string()]);
    }

    #[test]
    fn union_names_is_sorted_regardless_of_uid_order() {
        let names = union_names(
            vec![1000, 1001],
            resolver(&[(1000, "zoe"), (1001, "adam")]),
            None,
        );
        assert_eq!(names, vec!["adam".to_string(), "zoe".to_string()]);
    }

    #[test]
    fn union_names_includes_the_invoking_user_that_has_no_marker_yet() {
        let names = union_names(vec![1000], resolver(&[(1000, "alice")]), Some("bob"));
        assert_eq!(names, vec!["alice".to_string(), "bob".to_string()]);
    }

    #[test]
    fn union_names_does_not_repeat_the_invoking_user() {
        let names = union_names(vec![1000], resolver(&[(1000, "alice")]), Some("alice"));
        assert_eq!(names, vec!["alice".to_string()]);
    }

    #[test]
    fn union_names_skips_a_uid_that_no_longer_resolves() {
        let names = union_names(vec![1000, 1001], resolver(&[(1000, "alice")]), None);
        assert_eq!(names, vec!["alice".to_string()]);
    }

    #[test]
    fn union_names_is_empty_without_markers_or_an_invoking_user() {
        assert!(union_names(Vec::new(), resolver(&[]), None).is_empty());
    }
}
