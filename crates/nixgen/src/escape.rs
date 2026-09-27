#[derive(Debug, thiserror::Error)]
#[error("string value contains a null byte, which Nix cannot represent")]
pub struct NulByte;
