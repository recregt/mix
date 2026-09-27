pub use tokio_util::sync::CancellationToken;

#[allow(clippy::disallowed_methods)]
pub fn root() -> CancellationToken {
    CancellationToken::new()
}

#[allow(clippy::disallowed_methods)]
pub fn shield() -> CancellationToken {
    CancellationToken::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shield_is_not_reached_by_a_cancelled_root() {
        let root = root();
        let child = root.child_token();
        let shield = shield();

        root.cancel();

        assert!(child.is_cancelled());
        assert!(!shield.is_cancelled());
    }
}
