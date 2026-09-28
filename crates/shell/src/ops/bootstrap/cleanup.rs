//! Undoing work that must not fail the command that is already failing.
//!
//! A rollback, or a forced removal of what was there before: a step that cannot undo something
//! says so and carries on, because the failure the reader needs to see is the one that started
//! the rollback.

pub(crate) fn warn_on_failure<T, E: std::fmt::Display>(
    action: &'static str,
    result: std::result::Result<T, E>,
) {
    if let Err(error) = result {
        tracing::warn!("{action} failed: {error}, continuing");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_to_undo_is_not_propagated() {
        warn_on_failure(
            "remove managed directory",
            Err::<(), &str>("permission denied"),
        );
    }
}
