pub mod bootstrap;
pub mod doctor;
pub mod install;
pub mod remove;
pub mod repair;

pub fn requested_policy(
    mirror: Option<&str>,
    mirror_key: Option<&str>,
) -> Result<mix_core::policy::Policy, mix_shell::ops::bootstrap::Error> {
    mix_core::policy::Policy::new(mirror, mirror_key)
        .map_err(|invalid| mix_shell::ops::bootstrap::Error::InvalidMirror(invalid.to_string()))
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

#[cfg(test)]
mod tests {
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
}
