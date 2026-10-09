pub const NIXBLD_GROUP: &str = "nixbld";
pub const NIXBLD_GID: u32 = 30_000;
pub const NIXBLD_USER_COUNT: u32 = 32;
pub const NIXBLD_UID_BASE: u32 = 30_000;
pub const NIXBLD_HOME: &str = "/var/empty";
pub const NIXBLD_SHELL: &str = "/usr/sbin/nologin";

pub const MIX_USERS_GROUP: &str = "mix-users";
pub const MIX_USERS_GID: u32 = 30_100;

const NIXBLD_USER_NAMES: [&str; NIXBLD_USER_COUNT as usize] = [
    "nixbld1", "nixbld2", "nixbld3", "nixbld4", "nixbld5", "nixbld6", "nixbld7", "nixbld8",
    "nixbld9", "nixbld10", "nixbld11", "nixbld12", "nixbld13", "nixbld14", "nixbld15", "nixbld16",
    "nixbld17", "nixbld18", "nixbld19", "nixbld20", "nixbld21", "nixbld22", "nixbld23", "nixbld24",
    "nixbld25", "nixbld26", "nixbld27", "nixbld28", "nixbld29", "nixbld30", "nixbld31", "nixbld32",
];

pub fn user_name(n: u32) -> std::borrow::Cow<'static, str> {
    match NIXBLD_USER_NAMES.get((n as usize).wrapping_sub(1)) {
        Some(&name) => std::borrow::Cow::Borrowed(name),
        None => std::borrow::Cow::Owned(format!("{NIXBLD_GROUP}{n}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_name_borrows_a_static_name_for_every_managed_build_user() {
        for n in 1..=NIXBLD_USER_COUNT {
            let name = user_name(n);
            assert_eq!(name, format!("{NIXBLD_GROUP}{n}"));
            assert!(matches!(name, std::borrow::Cow::Borrowed(_)));
        }
    }

    #[test]
    fn user_name_falls_back_to_formatting_outside_the_managed_range() {
        assert_eq!(user_name(0), "nixbld0");
        assert_eq!(user_name(NIXBLD_USER_COUNT + 1), "nixbld33");
        assert!(matches!(user_name(0), std::borrow::Cow::Owned(_)));
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct InvokingUser {
    pub uid: u32,
    pub gid: u32,
    pub name: String,
    pub home: std::path::PathBuf,
}

#[derive(Clone)]
pub enum UserRef<'a> {
    Borrowed(&'a InvokingUser),
    Owned(Box<InvokingUser>),
}

impl UserRef<'_> {
    pub fn into_static(self) -> UserRef<'static> {
        match self {
            UserRef::Borrowed(user) => UserRef::Owned(Box::new(user.clone())),
            UserRef::Owned(user) => UserRef::Owned(user),
        }
    }
}

impl std::ops::Deref for UserRef<'_> {
    type Target = InvokingUser;

    fn deref(&self) -> &InvokingUser {
        match self {
            UserRef::Borrowed(user) => user,
            UserRef::Owned(user) => user,
        }
    }
}

impl std::fmt::Debug for UserRef<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        InvokingUser::fmt(self, f)
    }
}

impl<'a> From<&'a InvokingUser> for UserRef<'a> {
    fn from(user: &'a InvokingUser) -> Self {
        UserRef::Borrowed(user)
    }
}

impl From<InvokingUser> for UserRef<'static> {
    fn from(user: InvokingUser) -> Self {
        UserRef::Owned(Box::new(user))
    }
}
