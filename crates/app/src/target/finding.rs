//! What an inspection measured, and what repair can do about it.
//!
//! A finding is a fact and nothing else: no advice, no sentence, no name. It travels from the
//! inspection that took it to the three callers that read it — `mix doctor` prints it as a list,
//! the health gate in front of the other commands prints one of them as a refusal, and
//! `mix repair` decides from it what to reconcile.

/// Why an artifact is beyond repair's reach.
///
/// The reason is a value rather than a sentence: what a reader should do about it differs per
/// command — `mix repair` offers a way out, the health gate in front of the other commands only
/// says why it stopped — so the words are chosen where the command is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Unfixable {
    /// Something is in the way that repair will not delete.
    #[error("exists but is not a directory")]
    NotADirectory,

    /// The user the membership was for is gone.
    #[error("the user no longer exists")]
    MissingUser,

    /// Part of the Nix runtime itself, which repair does not install.
    #[error("missing, and `mix repair` can't restore it")]
    MissingRuntime,
}

/// What an inspection measured about an artifact that is not as it should be.
///
/// Every variant is a fact and nothing else: no advice, no sentence, no name — the artifact is
/// named by the report that carries the finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finding {
    /// Nothing is at the path.
    Missing,

    /// The path is there but could not be read.
    Unreadable { kind: std::io::ErrorKind },

    /// Something else is in the place of the directory.
    NotADirectory,

    /// The permission bits drifted from the ones mix sets.
    Mode { actual: u32, expected: u32 },

    /// The owning uid/gid drifted from the ones mix sets.
    Owner {
        actual: (u32, u32),
        expected: (u32, u32),
    },

    /// The file is mix's to write, and its contents are no longer the ones mix wrote.
    ContentDrift,

    /// There is no such group.
    GroupMissing,

    /// The group exists under a different gid.
    GroupGid { actual: u32, expected: u32 },

    /// The user exists but is not enrolled in the group.
    NotAMember { group: &'static str },

    /// The user the membership was for is gone.
    NoSuchUser,

    /// There is no such user.
    UserMissing,

    /// The user exists under different ids.
    UserIds {
        actual: (u32, u32),
        expected: (u32, u32),
    },

    /// The unit file is not installed.
    UnitMissing,

    /// The installed unit file drifted from the one mix ships.
    UnitDrift,

    /// The unit is installed but not running.
    UnitInactive,

    /// Part of the Nix runtime itself is not there.
    RuntimeMissing,
}

impl Finding {
    /// Why `mix repair` cannot reconcile this finding, or `None` when it can.
    ///
    /// Repair's reach is a fact about repair rather than a choice of words, so it is bound to
    /// the finding here — and both commands then read the same answer instead of each deciding
    /// for itself what the reader can be promised.
    pub fn unfixable(self) -> Option<Unfixable> {
        match self {
            Finding::NotADirectory => Some(Unfixable::NotADirectory),
            Finding::NoSuchUser => Some(Unfixable::MissingUser),
            Finding::RuntimeMissing => Some(Unfixable::MissingRuntime),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The binding between what an inspection found and what repair can do about it.
    #[test]
    fn a_finding_says_whether_repair_can_reconcile_it() {
        assert_eq!(
            Finding::NotADirectory.unfixable(),
            Some(Unfixable::NotADirectory)
        );
        assert_eq!(
            Finding::NoSuchUser.unfixable(),
            Some(Unfixable::MissingUser)
        );
        assert_eq!(
            Finding::RuntimeMissing.unfixable(),
            Some(Unfixable::MissingRuntime)
        );
    }

    #[test]
    fn everything_repair_reconciles_is_bound_to_no_reason() {
        for finding in [
            Finding::Missing,
            Finding::Unreadable {
                kind: std::io::ErrorKind::PermissionDenied,
            },
            Finding::Mode {
                actual: 0o700,
                expected: 0o755,
            },
            Finding::Owner {
                actual: (0, 0),
                expected: (1000, 1000),
            },
            Finding::ContentDrift,
            Finding::GroupMissing,
            Finding::GroupGid {
                actual: 1,
                expected: 30_000,
            },
            Finding::NotAMember { group: "mix-users" },
            Finding::UserMissing,
            Finding::UserIds {
                actual: (1, 1),
                expected: (30_000, 30_000),
            },
            Finding::UnitMissing,
            Finding::UnitDrift,
            Finding::UnitInactive,
        ] {
            assert_eq!(finding.unfixable(), None, "{finding:?}");
        }
    }
}
