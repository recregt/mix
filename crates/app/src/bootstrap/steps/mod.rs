mod activate_home_manager;
mod configure_nix_conf;
mod configure_systemd_service;
mod create_nix_dir;
mod fetch_and_unpack;
mod remove_existing_installation;
mod write_home_manager_config;

pub mod create_nix_tree;
pub mod create_users_and_groups;

pub use activate_home_manager::ActivateHomeManagerConfig;
pub(crate) use activate_home_manager::{BuildPolicy, activate};
pub use configure_nix_conf::ConfigureNixConf;
pub use configure_systemd_service::ConfigureSystemdService;
pub use create_nix_dir::CreateNixDir;
pub use create_nix_tree::CreateNixTree;
pub use create_users_and_groups::CreateUsersAndGroups;
pub use fetch_and_unpack::FetchAndUnpack;
pub use remove_existing_installation::RemoveExistingInstallation;
pub use write_home_manager_config::WriteHomeManagerConfig;
