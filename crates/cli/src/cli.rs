use std::path::PathBuf;

use clap::{ArgAction, Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(name = "mix", version, about = "Reproducible systems, made effortless")]
pub struct Cli {
    /// Use verbose output (-vv also shows each program's output)
    #[arg(short, long, action = ArgAction::Count, global = true, conflicts_with = "quiet")]
    pub verbose: u8,

    /// Print only errors
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Never draw progress in place, even on a terminal
    #[arg(
        long,
        global = true,
        env = "MIX_NO_PROGRESS",
        action = ArgAction::SetTrue,
        value_parser = clap::builder::FalseyValueParser::new(),
    )]
    pub no_progress: bool,

    /// Write output for people, or as one JSON event per line
    #[arg(long, global = true, value_enum, default_value_t = Output::Human)]
    pub output: Output,

    /// Also record every event to this file
    #[arg(long, global = true, value_name = "PATH")]
    pub events_file: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Output {
    Human,
    Json,
}

#[derive(Subcommand)]
pub enum EventsCommand {
    /// Check that a recorded events file is complete and well formed
    Check {
        /// The file `--events-file` wrote
        file: PathBuf,
    },

    /// Show a recorded events file the way the run looked, at any verbosity
    Show {
        /// The file `--events-file` wrote
        file: PathBuf,

        /// Show only this node and what ran inside it, such as `install/activate`
        #[arg(long)]
        node: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum Command {
    /// Set up `mix` and its runtime on this machine
    Bootstrap {
        /// Mirror for Nix and packages, used by every user [env: MIX_NIX_MIRROR]
        #[arg(long)]
        mirror: Option<String>,

        /// Key the mirror signs with, trusted machine-wide [env: MIX_NIX_MIRROR_KEY]
        #[arg(long)]
        mirror_key: Option<String>,

        /// Wipe any existing managed installation before bootstrapping
        #[arg(short, long)]
        force: bool,
    },

    /// Add packages to your profile
    Install {
        /// Packages to add
        #[arg(required = true)]
        packages: Vec<String>,
    },

    /// Remove packages from your profile
    Remove {
        /// Packages to remove
        #[arg(required = true)]
        packages: Vec<String>,
    },

    /// Check the health of the system
    Doctor,

    /// Repair configuration drift
    Repair,

    /// Work with recorded events
    Events {
        #[command(subcommand)]
        command: EventsCommand,
    },

    /// Describe a failure code, such as `locked`
    Explain {
        /// The code, as a failure prints it
        #[arg(required_unless_present = "list")]
        code: Option<String>,

        /// List every failure code
        #[arg(long, conflicts_with = "code")]
        list: bool,
    },

    #[command(
        hide = true,
        about = "Carry out privileged operations for another mix process"
    )]
    Worker,

    #[command(
        name = mix_shell::effect::home::COMMAND,
        hide = true,
        about = "Change files in your own directories for a privileged mix process"
    )]
    HomeFiles { request: String },
}

impl Cli {
    /// Whether output may be drawn in place.
    ///
    /// `--no-progress` and `--output json` are explicit requests for plain output; `CI` is
    /// honoured because build systems set it and nobody is watching a CI log live.
    pub fn draws_progress(&self) -> bool {
        !self.no_progress
            && self.output == Output::Human
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

    fn cargo_style(text: &str) -> bool {
        text.is_ascii()
            && text.chars().count() <= MAX_HELP_LEN
            && text
                .chars()
                .next()
                .is_some_and(|first| !first.is_ascii_lowercase())
            && !text.ends_with('.')
            && !text.contains("; ")
            && !text.contains(". ")
            && !text.contains('\n')
    }

    fn check_help(command: &clap::Command, path: &str) {
        let abouts = [command.get_about(), command.get_long_about()];
        for about in abouts.into_iter().flatten() {
            let text = about.to_string();
            assert!(cargo_style(&text), "{path}: {text:?}");
        }

        for arg in command.get_arguments() {
            let helps = [arg.get_help(), arg.get_long_help()];
            for help in helps.into_iter().flatten() {
                let text = help.to_string();
                assert!(cargo_style(&text), "{path} {}: {text:?}", arg.get_id());
            }
            for value in arg.get_possible_values() {
                if let Some(help) = value.get_help() {
                    let text = help.to_string();
                    assert!(cargo_style(&text), "{path} {}: {text:?}", value.get_name());
                }
            }
        }

        for subcommand in command.get_subcommands() {
            check_help(subcommand, &format!("{path} {}", subcommand.get_name()));
        }
    }

    #[test]
    fn every_help_text_reads_like_cargo_s() {
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
    fn output_is_for_people_unless_json_is_asked_for() {
        assert_eq!(parse(&["mix", "install", "ripgrep"]).output, Output::Human);
        assert_eq!(
            parse(&["mix", "--output", "json", "install", "ripgrep"]).output,
            Output::Json
        );
        assert_eq!(
            parse(&["mix", "install", "--output", "json", "ripgrep"]).output,
            Output::Json
        );
    }

    #[test]
    fn json_output_implies_plain_output() {
        assert!(!parse(&["mix", "--output", "json", "install", "ripgrep"]).draws_progress());
    }

    #[test]
    fn events_can_be_recorded_by_any_command() {
        let cli = parse(&["mix", "doctor", "--events-file", "/tmp/events.ndjson"]);
        assert_eq!(cli.events_file, Some(PathBuf::from("/tmp/events.ndjson")));
    }

    #[test]
    fn a_recorded_file_is_checked_by_its_own_command() {
        assert!(matches!(
            parse(&["mix", "events", "check", "/tmp/events.ndjson"]).command,
            Command::Events {
                command: EventsCommand::Check { ref file }
            } if file == &PathBuf::from("/tmp/events.ndjson")
        ));
    }

    #[test]
    fn a_recorded_run_is_shown_at_any_verbosity_and_for_one_node() {
        let cli = parse(&[
            "mix",
            "-vv",
            "events",
            "show",
            "/tmp/events.ndjson",
            "--node",
            "install/plan/activate",
        ]);

        assert_eq!(cli.verbose, 2);
        assert!(matches!(
            cli.command,
            Command::Events {
                command: EventsCommand::Show { ref file, node: Some(ref node) }
            } if file == &PathBuf::from("/tmp/events.ndjson") && node == "install/plan/activate"
        ));
    }

    #[test]
    fn the_old_json_flag_is_gone() {
        assert!(Cli::try_parse_from(["mix", "install", "--json", "ripgrep"]).is_err());
        assert!(Cli::try_parse_from(["mix", "remove", "--json", "ripgrep"]).is_err());
    }

    #[test]
    fn remove_takes_the_packages_to_remove() {
        let cli = parse(&["mix", "remove", "ripgrep", "fd"]);

        assert!(matches!(
            cli.command,
            Command::Remove { ref packages } if packages == &["ripgrep", "fd"]
        ));
    }

    #[test]
    fn remove_needs_at_least_one_package() {
        assert!(Cli::try_parse_from(["mix", "remove"]).is_err());
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

    #[test]
    fn no_help_text_names_nix_mechanics() {
        fn visit(command: &clap::Command, found: &mut Vec<String>) {
            let texts = command
                .get_about()
                .into_iter()
                .chain(command.get_arguments().filter_map(|arg| arg.get_help()))
                .map(ToString::to_string);
            for text in texts {
                if !mix_core::vocabulary::nix_mechanics_in(&text).is_empty() {
                    found.push(format!("{}: {text}", command.get_name()));
                }
            }
            for sub in command.get_subcommands() {
                visit(sub, found);
            }
        }

        let mut found = Vec::new();
        visit(&<Cli as clap::CommandFactory>::command(), &mut found);
        assert_eq!(found, Vec::<String>::new());
    }
}
