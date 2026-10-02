pub fn host_config() -> mix_shell::HostConfig {
    mix_shell::HostConfig {
        git_binary: std::env::var_os("MIX_GIT_PATH").map(std::path::PathBuf::from),
    }
}
