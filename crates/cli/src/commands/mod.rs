pub mod bootstrap;
pub mod doctor;
pub mod install;
pub mod remove;
pub mod repair;

use mix_core::models::UserConfig;
use mix_shell::effect::lock::LockGuard;
use mix_shell::profile::change::Error;

fn exclusive_lock() -> mix_core::Result<LockGuard> {
    mix_shell::effect::lock::acquire_exclusive(mix_core::paths::LOCK_FILE)
}

pub fn enrolled_user() -> Option<UserConfig> {
    mix_shell::effect::accounts::invoking_user()
        .and_then(mix_shell::profile::existing_user_config_for)
}

pub fn host_config() -> mix_shell::HostConfig {
    mix_shell::HostConfig {
        git_binary: std::env::var_os("MIX_GIT_PATH").map(std::path::PathBuf::from),
    }
}

pub fn policy() -> mix_core::policy::Policy {
    let stored = std::fs::read_to_string(mix_core::paths::POLICY_FILE).ok();
    mix_core::policy::Policy::load(stored.as_deref())
}

pub fn requested_policy(
    mirror: Option<&str>,
    mirror_key: Option<&str>,
) -> Result<mix_core::policy::Policy, mix_shell::ops::bootstrap::Error> {
    mix_core::policy::Policy::new(mirror, mirror_key)
        .map_err(|invalid| mix_shell::ops::bootstrap::Error::InvalidMirror(invalid.to_string()))
}

pub fn acquire_lock() -> anyhow::Result<LockGuard> {
    Ok(exclusive_lock()?)
}

pub const MIRROR_VAR: &str = "MIX_NIX_MIRROR";
pub const MIRROR_KEY_VAR: &str = "MIX_NIX_MIRROR_KEY";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct MirrorSetting {
    pub url: Option<String>,
    pub key: Option<String>,
}

pub fn mirror_setting(
    url: Option<&str>,
    key: Option<&str>,
    environment: impl Fn(&str) -> Option<String>,
) -> MirrorSetting {
    let key = key.map(str::to_string);
    match url {
        Some(url) => MirrorSetting {
            url: Some(url.to_string()),
            key,
        },
        None => MirrorSetting {
            url: environment(MIRROR_VAR),
            key: key.or_else(|| environment(MIRROR_KEY_VAR)),
        },
    }
}

fn refuse_root(root: bool) -> Result<(), Error> {
    if root { Err(Error::NotRoot) } else { Ok(()) }
}

pub fn acquire_profile() -> Result<(LockGuard, UserConfig), Error> {
    refuse_root(mix_shell::effect::accounts::is_root())?;
    let lock = exclusive_lock()?;

    let Some(user_config) = enrolled_user() else {
        return Err(Error::NotBootstrapped);
    };

    Ok((lock, user_config))
}

#[cfg(test)]
mod tests {
    use mix_events::Diagnose;
    use mix_events::v1::Code;

    use super::*;

    fn environment(
        url: Option<&'static str>,
        key: Option<&'static str>,
    ) -> impl Fn(&str) -> Option<String> {
        move |name| match name {
            MIRROR_VAR => url.map(str::to_string),
            MIRROR_KEY_VAR => key.map(str::to_string),
            _ => None,
        }
    }

    #[test]
    fn a_mirror_on_the_command_line_wins_over_the_environment_with_its_own_key() {
        let setting = mirror_setting(
            Some("http://flag.internal"),
            None,
            environment(Some("http://env.internal"), Some("env:KEY")),
        );

        assert_eq!(
            setting,
            MirrorSetting {
                url: Some("http://flag.internal".into()),
                key: None,
            }
        );
    }

    #[test]
    fn a_mirror_set_only_in_the_environment_is_used_with_the_environments_key() {
        let setting = mirror_setting(
            None,
            None,
            environment(Some("http://env.internal"), Some("env:KEY")),
        );

        assert_eq!(
            setting,
            MirrorSetting {
                url: Some("http://env.internal".into()),
                key: Some("env:KEY".into()),
            }
        );
        assert_eq!(
            mirror_setting(None, None, environment(None, None)),
            MirrorSetting::default()
        );
    }

    #[test]
    fn root_is_refused_before_anything_is_locked_or_read() {
        let refused = refuse_root(true).unwrap_err();

        assert!(matches!(refused, Error::NotRoot));
        assert_eq!(refused.code(), Some(Code::RootNotAllowed));
        assert!(refuse_root(false).is_ok());
    }
}
