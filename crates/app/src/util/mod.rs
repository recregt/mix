pub async fn path_exists(path: &str) -> bool {
    tokio::fs::try_exists(path).await.unwrap_or(false)
}

pub async fn files_match(a: &str, b: &str) -> bool {
    let (a, b) = (tokio::fs::read(a).await, tokio::fs::read(b).await);
    matches!((a, b), (Ok(a), Ok(b)) if a == b)
}

pub async fn systemd_unit_is_active(name: &str) -> bool {
    tokio::process::Command::new("systemctl")
        .args(["is-active", "--quiet", name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
}

pub fn group_has_gid(name: &str, gid: u32) -> bool {
    nix::unistd::Group::from_name(name)
        .ok()
        .flatten()
        .is_some_and(|group| group.gid.as_raw() == gid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn path_exists_true_for_a_real_path() {
        let dir = tempfile::tempdir().unwrap();
        assert!(path_exists(dir.path().to_str().unwrap()).await);
    }

    #[tokio::test]
    async fn path_exists_false_when_missing() {
        assert!(!path_exists("/does/not/exist/mix-test").await);
    }

    #[tokio::test]
    async fn files_match_true_for_identical_content() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"same").unwrap();
        std::fs::write(&b, b"same").unwrap();
        assert!(files_match(a.to_str().unwrap(), b.to_str().unwrap()).await);
    }

    #[tokio::test]
    async fn files_match_false_for_different_content() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"one").unwrap();
        std::fs::write(&b, b"two").unwrap();
        assert!(!files_match(a.to_str().unwrap(), b.to_str().unwrap()).await);
    }

    #[tokio::test]
    async fn files_match_false_when_one_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        std::fs::write(&a, b"one").unwrap();
        let missing = dir.path().join("missing");
        assert!(!files_match(a.to_str().unwrap(), missing.to_str().unwrap()).await);
    }

    #[test]
    fn group_has_gid_true_for_a_known_system_group() {
        assert!(group_has_gid("root", 0));
    }

    #[test]
    fn group_has_gid_false_for_the_wrong_gid() {
        assert!(!group_has_gid("root", 9999));
    }

    #[test]
    fn group_has_gid_false_for_a_nonexistent_group() {
        assert!(!group_has_gid("mix-test-nonexistent-group-xyz", 0));
    }
}
