use std::sync::Arc;

use mix_core::{DownloadProgress, Step};

use crate::bootstrap::error::Error;
use crate::bootstrap::steps::{
    ConfigureNixConf, ConfigureSystemdService, CreateNixDir, CreateNixTree, CreateUsersAndGroups,
    FetchAndUnpack, RemoveExistingInstallation, WriteHomeManagerConfig,
};

pub fn bootstrap_steps(
    mirror: Option<&str>,
    force: bool,
    progress: Arc<dyn DownloadProgress>,
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
    steps.push(Box::new(WriteHomeManagerConfig::default()));
    steps
}
