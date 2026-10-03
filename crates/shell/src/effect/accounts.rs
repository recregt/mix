use mix_core::identity::InvokingUser;

pub fn is_root() -> bool {
    nix::unistd::Uid::effective().is_root()
}

pub fn invoking_user(sudo_uid: Option<&str>) -> Option<InvokingUser> {
    resolve_invoking_user(is_root(), nix::unistd::Uid::current(), sudo_uid)
}

fn resolve_invoking_user(
    is_root: bool,
    current: nix::unistd::Uid,
    sudo_uid: Option<&str>,
) -> Option<InvokingUser> {
    if is_root {
        invoking_user_from(sudo_uid)
    } else {
        current_user(current)
    }
}

pub fn user_by_uid(uid: u32) -> Option<InvokingUser> {
    if uid == 0 {
        return None;
    }
    current_user(nix::unistd::Uid::from_raw(uid))
}

fn invoking_user_from(sudo_uid: Option<&str>) -> Option<InvokingUser> {
    let uid: u32 = sudo_uid?.parse().ok()?;
    current_user(nix::unistd::Uid::from_raw(uid))
}

fn current_user(uid: nix::unistd::Uid) -> Option<InvokingUser> {
    let user = nix::unistd::User::from_uid(uid).ok().flatten()?;
    if !usable(&user.name, &user.dir) {
        return None;
    }
    Some(InvokingUser {
        uid: uid.as_raw(),
        gid: user.gid.as_raw(),
        name: user.name,
        home: user.dir,
    })
}

fn usable(name: &str, home: &std::path::Path) -> bool {
    !name.contains('"') && home.is_absolute() && home.to_str().is_some()
}

pub fn user_in_group(name: &str, user: &InvokingUser) -> bool {
    nix::unistd::Group::from_name(name)
        .ok()
        .flatten()
        .is_some_and(|group| group.gid.as_raw() == user.gid || group.mem.contains(&user.name))
}

pub fn group_has_member(name: &str, user: &str) -> bool {
    let Some(group) = nix::unistd::Group::from_name(name).ok().flatten() else {
        return false;
    };
    group.mem.iter().any(|member| member == user)
        || nix::unistd::User::from_name(user)
            .ok()
            .flatten()
            .is_some_and(|resolved| resolved.gid == group.gid)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn an_account_nix_cannot_name_is_not_usable() {
        assert!(usable("mix", std::path::Path::new("/home/mix")));
        assert!(!usable("mi\"x", std::path::Path::new("/home/mix")));
        assert!(!usable("mix", std::path::Path::new("home/mix")));
        let not_utf8 = std::ffi::OsStr::from_bytes(b"/home/\xff");
        assert!(!usable("mix", std::path::Path::new(not_utf8)));
    }

    #[test]
    fn invoking_user_from_resolves_a_known_uid() {
        let user = invoking_user_from(Some("0")).unwrap();

        assert_eq!(user.uid, 0);
        assert_eq!(user.gid, 0);
        assert_eq!(user.name, "root");
        assert!(user.home.is_absolute());
    }

    #[test]
    fn invoking_user_from_none_when_sudo_uid_is_unset() {
        assert!(invoking_user_from(None).is_none());
    }

    #[test]
    fn invoking_user_from_none_when_sudo_uid_is_not_a_number() {
        assert!(invoking_user_from(Some("not-a-uid")).is_none());
    }

    #[test]
    fn invoking_user_resolves_the_current_process_when_not_root() {
        if is_root() {
            return;
        }
        let user = invoking_user(None).expect("the current uid should have a passwd entry");
        assert_eq!(user.uid, nix::unistd::Uid::current().as_raw());
    }

    #[test]
    fn resolve_invoking_user_none_for_bare_root_without_sudo_uid() {
        assert!(resolve_invoking_user(true, nix::unistd::Uid::current(), None).is_none());
    }

    #[test]
    fn resolve_invoking_user_uses_sudo_uid_when_root() {
        let user =
            resolve_invoking_user(true, nix::unistd::Uid::from_raw(4_294_967_295), Some("0"))
                .unwrap();

        assert_eq!(user.uid, 0);
        assert_eq!(user.name, "root");
    }

    #[test]
    fn resolve_invoking_user_ignores_sudo_uid_when_not_root() {
        let user = resolve_invoking_user(false, nix::unistd::Uid::from_raw(0), Some("4294967295"))
            .unwrap();

        assert_eq!(user.uid, 0);
        assert_eq!(user.name, "root");
    }

    #[test]
    fn invoking_user_from_none_when_the_uid_has_no_passwd_entry() {
        assert!(invoking_user_from(Some("4294967295")).is_none());
    }

    #[test]
    fn group_has_member_counts_a_primary_group_as_membership() {
        assert!(group_has_member("root", "root"));
    }

    #[test]
    fn user_in_group_counts_a_primary_group_as_membership() {
        let root = InvokingUser {
            uid: 0,
            gid: 0,
            name: "root".into(),
            home: "/root".into(),
        };
        assert!(user_in_group("root", &root));
        assert!(!user_in_group("mix-test-nonexistent-group-xyz", &root));
    }

    #[test]
    fn group_has_member_false_for_a_user_outside_the_group() {
        assert!(!group_has_member("root", "mix-test-nonexistent-user-xyz"));
    }

    #[test]
    fn group_has_member_false_for_a_nonexistent_group() {
        assert!(!group_has_member("mix-test-nonexistent-group-xyz", "root"));
    }
}
