use std::sync::Arc;

use mix_core::models::UserConfig;
use mix_core::{ActivityReporter, DownloadProgress, Step};

use crate::bootstrap::error::Error;
use crate::bootstrap::steps::{
    ActivateHomeManagerConfig, ConfigureNixConf, ConfigureSystemdService, CreateNixDir,
    CreateNixTree, CreateUsersAndGroups, FetchAndUnpack, RemoveExistingInstallation,
    WriteHomeManagerConfig,
};
use crate::profile::config::resolve_user_config;

pub fn bootstrap_steps(
    mirror: Option<&str>,
    mirror_key: Option<&str>,
    force: bool,
    progress: Arc<dyn DownloadProgress>,
    activity: Arc<dyn ActivityReporter>,
) -> Vec<Box<dyn Step<Error = Error>>> {
    steps_for(
        mirror,
        mirror_key,
        force,
        progress,
        activity,
        resolve_user_config(),
    )
}

fn steps_for(
    mirror: Option<&str>,
    mirror_key: Option<&str>,
    force: bool,
    progress: Arc<dyn DownloadProgress>,
    activity: Arc<dyn ActivityReporter>,
    user_config: Option<UserConfig>,
) -> Vec<Box<dyn Step<Error = Error>>> {
    let mut steps: Vec<Box<dyn Step<Error = Error>>> = Vec::new();
    if force {
        steps.push(Box::new(RemoveExistingInstallation));
    }
    steps.push(Box::new(CreateNixDir::default()));
    steps.push(Box::new(CreateNixTree::default()));
    steps.push(Box::new(CreateUsersAndGroups::default()));
    steps.push(Box::new(FetchAndUnpack::new(mirror, progress)));
    steps.push(Box::new(ConfigureNixConf::default()));
    steps.push(Box::new(ConfigureSystemdService::default()));
    steps.push(Box::new(WriteHomeManagerConfig::new(user_config.clone())));
    steps.push(Box::new(ActivateHomeManagerConfig::new(
        user_config,
        mirror,
        mirror_key,
        activity,
    )));
    steps
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use mix_core::privilege::InvokingUser;
    use mix_core::{NoopActivity, NoopProgress};

    use super::*;

    fn user_config() -> UserConfig {
        UserConfig {
            user: InvokingUser {
                uid: 1000,
                gid: 1000,
                name: "mix-user".to_string(),
                home: PathBuf::from("/home/mix-user"),
            },
            flake: "flake-content".to_string(),
            home: "home-content".to_string(),
        }
    }

    fn noop_activity() -> Arc<dyn ActivityReporter> {
        Arc::new(NoopActivity)
    }

    fn step_names(steps: &[Box<dyn Step<Error = Error>>]) -> Vec<&'static str> {
        steps.iter().map(|step| step.name()).collect()
    }

    #[test]
    fn the_home_manager_steps_run_last_and_in_order() {
        let steps = steps_for(
            None,
            None,
            false,
            Arc::new(NoopProgress),
            noop_activity(),
            Some(user_config()),
        );
        let names = step_names(&steps);
        assert_eq!(
            &names[names.len() - 2..],
            &["write home-manager config", "activate home-manager config"]
        );
    }

    #[test]
    fn forcing_prepends_the_removal_step() {
        let steps = steps_for(
            None,
            None,
            true,
            Arc::new(NoopProgress),
            noop_activity(),
            None,
        );
        assert_eq!(step_names(&steps)[0], "remove the existing installation");
    }

    #[test]
    fn the_same_step_list_is_planned_with_or_without_a_user() {
        let with_user = steps_for(
            None,
            None,
            false,
            Arc::new(NoopProgress),
            noop_activity(),
            Some(user_config()),
        );
        let without_user = steps_for(
            None,
            None,
            false,
            Arc::new(NoopProgress),
            noop_activity(),
            None,
        );
        assert_eq!(step_names(&with_user), step_names(&without_user));
    }
}
