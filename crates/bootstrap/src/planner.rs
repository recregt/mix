use mix_core::Step;

use crate::steps::{
    ConfigureNixConf, ConfigureSystemdService, CreateNixDir, CreateNixTree, CreateUsersAndGroups,
    FetchAndUnpack,
};

pub fn install_steps() -> Vec<Box<dyn Step>> {
    vec![
        Box::new(CreateNixDir),
        Box::new(CreateNixTree),
        Box::new(CreateUsersAndGroups),
        Box::new(FetchAndUnpack),
        Box::new(ConfigureNixConf),
        Box::new(ConfigureSystemdService),
    ]
}
