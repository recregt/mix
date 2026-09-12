use mix_core::Step;

use crate::error::Error;
use crate::steps::{
    ConfigureNixConf, ConfigureSystemdService, CreateNixDir, CreateNixTree, CreateUsersAndGroups,
    FetchAndUnpack,
};

pub fn bootstrap_steps(mirror: Option<&str>) -> Vec<Box<dyn Step<Error = Error>>> {
    vec![
        Box::new(CreateNixDir::default()),
        Box::new(CreateNixTree),
        Box::new(CreateUsersAndGroups::default()),
        Box::new(FetchAndUnpack::new(mirror)),
        Box::new(ConfigureNixConf::default()),
        Box::new(ConfigureSystemdService::default()),
    ]
}
