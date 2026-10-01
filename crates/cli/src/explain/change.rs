use mix_shell::profile::change::Error;
use mix_shell::profile::state::Source;

use super::{Context, Diagnostic};

pub(crate) fn describe(error: &Error, command: &str, action: &dyn std::fmt::Display) -> Diagnostic {
    super::render::render_error(error, &Context { command, action })
}

pub fn restored(source: Source) -> Option<Diagnostic> {
    match source {
        Source::File | Source::Generation => None,
        Source::Fresh => Some(Diagnostic::hinting(
            "your package list was damaged and couldn't be recovered, so it was reset",
            "reinstall your packages with `mix install`",
        )),
    }
}

#[cfg(test)]
mod tests {
    use mix_shell::profile::state::Invalid;

    use super::*;

    fn message(error: &Error) -> String {
        describe(error, "mix install", &"install ripgrep").message()
    }

    #[test]
    fn running_as_root_names_the_command_and_says_how_to_run_it_instead() {
        for command in ["mix install", "mix remove"] {
            let message = describe(&Error::NotRoot, command, &"install ripgrep").message();

            assert!(message.contains(&format!("`{command}` can't be run as root")));
            assert!(message.contains("without sudo"));
        }
    }

    #[test]
    fn an_older_mix_is_told_how_to_update() {
        let message = message(&Error::NewerState(2));

        assert!(message.contains("older than the one that set up your packages"));
        assert!(message.contains("original install method"));
        assert!(message.contains("https://github.com/recregt/mix"));
    }

    #[test]
    fn a_bad_package_name_reads_the_same_whichever_check_caught_it() {
        let from_state = message(&Error::InvalidState(Invalid::Package(
            "rip grep".to_string(),
        )));
        let from_render = message(&Error::InvalidPackage(
            mix_nixgen::HomeModule::new(
                "mix",
                std::path::Path::new("/home/mix"),
                mix_nixgen::StateVersion::new_static("24.05"),
            )
            .unwrap()
            .packages(["rip grep"])
            .unwrap_err(),
        ));

        assert_eq!(from_state, from_render);
        assert_eq!(
            from_state,
            "\"rip grep\" isn't a valid package name\npackage names look like `ripgrep` or `python3`"
        );
        assert!(!from_state.to_lowercase().contains("nix"));
    }

    #[test]
    fn a_list_mix_built_wrong_is_reported_as_a_bug() {
        assert!(message(&Error::InvalidState(Invalid::Missing("git"))).contains("report this bug"));
    }

    #[test]
    fn only_packages_that_are_gone_are_worth_telling() {
        assert!(restored(Source::File).is_none());
        assert!(restored(Source::Generation).is_none());
        let note = restored(Source::Fresh).unwrap().message();
        assert!(note.contains("couldn't be recovered"));
        assert!(note.ends_with("reinstall your packages with `mix install`"));
    }

    #[test]
    fn an_unbootstrapped_user_is_sent_to_bootstrap() {
        let message = message(&Error::NotBootstrapped);

        assert!(message.contains("isn't set up for you yet"));
        assert!(message.contains("mix bootstrap"));
    }

    #[test]
    fn a_shared_failure_names_the_command_the_reader_ran() {
        for command in ["mix install", "mix remove"] {
            let message = describe(
                &Error::Core(mix_core::Error::Locked {
                    path: "/var/lib/mix/lock".into(),
                }),
                command,
                &"install ripgrep",
            )
            .message();

            assert!(message.contains(&format!("run `{command}` again")));
        }
    }
}
