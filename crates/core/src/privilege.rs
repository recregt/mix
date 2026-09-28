#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvokingUser {
    pub uid: u32,
    pub gid: u32,
    pub name: String,
    pub home: std::path::PathBuf,
}
