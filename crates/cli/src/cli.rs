use clap::{ArgAction, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "mix", version, about = "Reproducible systems, made effortless")]
pub struct Cli {
    /// Verbosity: -v steps, -vv commands, -vvv output
    #[arg(short, long, action = ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Never draw progress in place, even on a terminal
    #[arg(
        long,
        global = true,
        env = "MIX_NO_PROGRESS",
        action = ArgAction::SetTrue,
        // A script exporting MIX_NO_PROGRESS=1 means it, and an empty one means nothing, so the
        // usual shell spellings are all accepted rather than just "true".
        value_parser = clap::builder::FalseyValueParser::new(),
    )]
    pub no_progress: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Initialize runtime and system dependencies
    Bootstrap {
        /// Alternate URL to fetch the pinned Nix archive from
        #[arg(long, env = "MIX_NIX_MIRROR")]
        mirror: Option<String>,

        /// Public key the mirror's binary cache is signed with
        #[arg(long, env = "MIX_NIX_MIRROR_KEY")]
        mirror_key: Option<String>,

        /// Wipe any existing managed installation before bootstrapping
        #[arg(short, long)]
        force: bool,
    },

    /// Add packages to your home-manager profile
    Install {
        /// Packages to add
        #[arg(required = true)]
        packages: Vec<String>,

        /// Alternate URL to fetch the pinned Nix archive from
        #[arg(long, env = "MIX_NIX_MIRROR")]
        mirror: Option<String>,

        /// Public key the mirror's binary cache is signed with
        #[arg(long, env = "MIX_NIX_MIRROR_KEY")]
        mirror_key: Option<String>,

        /// Report the result as JSON on stdout, for scripts
        #[arg(long)]
        json: bool,

        /// Compile packages the binary cache cannot provide, instead of refusing
        #[arg(long)]
        build: bool,
    },

    /// Remove packages from your home-manager profile
    Remove {
        /// Packages to remove
        #[arg(required = true)]
        packages: Vec<String>,

        /// Alternate URL to fetch the pinned Nix archive from
        #[arg(long, env = "MIX_NIX_MIRROR")]
        mirror: Option<String>,

        /// Public key the mirror's binary cache is signed with
        #[arg(long, env = "MIX_NIX_MIRROR_KEY")]
        mirror_key: Option<String>,

        /// Report the result as JSON on stdout, for scripts
        #[arg(long)]
        json: bool,
    },

    /// Inspect system health
    Doctor,

    /// Repair configuration drift
    Repair,
}

impl Cli {
    /// Whether output may be drawn in place.
    ///
    /// `--no-progress` and `--json` are explicit requests for plain output; `CI` is honoured
    /// because build systems set it and nobody is watching a CI log live.
    pub fn draws_progress(&self) -> bool {
        !self.no_progress
            && !matches!(
                self.command,
                Command::Install { json: true, .. } | Command::Remove { json: true, .. }
            )
            && !ci_asks_for_plain_output(std::env::var("CI").ok().as_deref())
    }
}

fn ci_asks_for_plain_output(ci: Option<&str>) -> bool {
    match ci.map(str::trim) {
        None | Some("") | Some("0") => false,
        Some(value) => !value.eq_ignore_ascii_case("false"),
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    const MAX_HELP_LEN: usize = 80;

    fn check_help(command: &clap::Command, path: &str) {
        if let Some(about) = command.get_about() {
            let text = about.to_string();
            assert!(
                text.chars().count() <= MAX_HELP_LEN,
                "{path}: help text is {} chars (max {MAX_HELP_LEN}): {text:?}",
                text.chars().count()
            );
        }

        for arg in command.get_arguments() {
            if let Some(help) = arg.get_help() {
                let text = help.to_string();
                assert!(
                    text.chars().count() <= MAX_HELP_LEN,
                    "{path} --{}: help text is {} chars (max {MAX_HELP_LEN}): {text:?}",
                    arg.get_id(),
                    text.chars().count()
                );
            }
        }

        for subcommand in command.get_subcommands() {
            check_help(subcommand, &format!("{path} {}", subcommand.get_name()));
        }
    }

    #[test]
    fn help_text_stays_within_the_length_budget() {
        check_help(&Cli::command(), "mix");
    }

    #[test]
    fn the_argument_surface_is_valid() {
        Cli::command().debug_assert();
    }

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("the arguments should parse")
    }

    #[test]
    fn install_draws_progress_by_default() {
        assert!(!parse(&["mix", "install", "ripgrep"]).no_progress);
    }

    #[test]
    fn no_progress_turns_drawing_off() {
        assert!(parse(&["mix", "--no-progress", "install", "ripgrep"]).no_progress);
    }

    #[test]
    fn no_progress_is_accepted_after_the_subcommand_too() {
        assert!(parse(&["mix", "install", "ripgrep", "--no-progress"]).no_progress);
    }

    #[test]
    fn json_is_off_unless_it_is_asked_for() {
        let cli = parse(&["mix", "install", "ripgrep"]);
        assert!(matches!(cli.command, Command::Install { json: false, .. }));
    }

    #[test]
    fn json_can_be_asked_for() {
        let cli = parse(&["mix", "install", "--json", "ripgrep"]);
        assert!(matches!(cli.command, Command::Install { json: true, .. }));
    }

    #[test]
    fn json_output_implies_plain_output() {
        assert!(!parse(&["mix", "install", "--json", "ripgrep"]).draws_progress());
    }

    #[test]
    fn remove_takes_the_packages_to_remove() {
        let cli = parse(&["mix", "remove", "ripgrep", "fd"]);

        assert!(matches!(
            cli.command,
            Command::Remove { ref packages, json: false, .. } if packages == &["ripgrep", "fd"]
        ));
    }

    #[test]
    fn remove_needs_at_least_one_package() {
        assert!(Cli::try_parse_from(["mix", "remove"]).is_err());
    }

    #[test]
    fn remove_json_can_be_asked_for() {
        let cli = parse(&["mix", "remove", "--json", "ripgrep"]);

        assert!(matches!(cli.command, Command::Remove { json: true, .. }));
    }

    #[test]
    fn remove_json_output_implies_plain_output() {
        assert!(!parse(&["mix", "remove", "--json", "ripgrep"]).draws_progress());
    }

    #[test]
    fn remove_has_no_build_flag() {
        assert!(Cli::try_parse_from(["mix", "remove", "--build", "ripgrep"]).is_err());
    }

    #[test]
    fn an_unset_or_disabled_ci_variable_leaves_progress_alone() {
        for value in [
            None,
            Some(""),
            Some("  "),
            Some("0"),
            Some("false"),
            Some("FALSE"),
        ] {
            assert!(
                !ci_asks_for_plain_output(value),
                "CI={value:?} should not force plain output"
            );
        }
    }

    #[test]
    fn a_set_ci_variable_forces_plain_output() {
        for value in [Some("1"), Some("true"), Some("yes"), Some("github")] {
            assert!(
                ci_asks_for_plain_output(value),
                "CI={value:?} should force plain output"
            );
        }
    }
}
