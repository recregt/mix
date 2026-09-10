use mix_core::Step;

use crate::steps::{
    ConfigureNixConf, ConfigureSystemdService, CreateNixDir, CreateUsersAndGroups, FetchAndUnpack,
};

pub fn install_steps() -> Vec<Box<dyn Step>> {
    vec![
        Box::new(CreateNixDir),
        Box::new(FetchAndUnpack),
        Box::new(CreateUsersAndGroups),
        Box::new(ConfigureNixConf),
        Box::new(ConfigureSystemdService),
    ]
}
