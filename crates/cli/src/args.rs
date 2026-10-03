use std::path::PathBuf;

use clap::builder::styling::{AnsiColor, Effects, Styles};
use clap::{ArgAction, Parser, Subcommand, ValueEnum};

const STYLES: Styles = Styles::styled()
    .header(AnsiColor::BrightGreen.on_default().effects(Effects::BOLD))
    .usage(AnsiColor::BrightGreen.on_default().effects(Effects::BOLD))
    .literal(AnsiColor::BrightCyan.on_default().effects(Effects::BOLD))
    .placeholder(AnsiColor::Cyan.on_default())
    .error(AnsiColor::BrightRed.on_default().effects(Effects::BOLD))
    .valid(AnsiColor::BrightCyan.on_default().effects(Effects::BOLD))
    .invalid(AnsiColor::Yellow.on_default().effects(Effects::BOLD));

#[derive(Parser)]
#[command(
    name = "mix",
    version,
    about = "Reproducible systems, made effortless",
    styles = STYLES,
    after_help = "See 'mix help <command>' for more information on a specific command."
)]
pub struct Args {
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

    /// Choose when to color the output
    #[arg(long, global = true, value_enum, value_name = "WHEN", default_value_t = Color::Auto)]
    pub color: Color,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Color {
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Output {
    Human,
    Json,
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

    /// Remove old generations of your profile
    Clean {
        /// Also remove store paths nothing uses any more
        #[arg(short, long)]
        all: bool,
    },

    /// Check the health of the system
    Doctor,

    /// Repair configuration drift
    Repair,

    /// Describe a failure code, such as `network`
    Explain {
        /// The code, as a failure prints it
        #[arg(required_unless_present = "list", value_parser = code)]
        code: Option<mix_events::v1::Code>,

        /// List every failure code
        #[arg(long, conflicts_with = "code")]
        list: bool,
    },
}

fn code(name: &str) -> Result<mix_events::v1::Code, String> {
    mix_explain::codes::parse(name).ok_or_else(|| {
        "it isn't a code `mix` uses; `mix explain --list` shows them all".to_string()
    })
}

pub fn color_requested<I, S>(args: I) -> Color
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut args = args.into_iter();
    let mut found = Color::Auto;
    while let Some(arg) = args.next() {
        let arg = arg.as_ref().to_string_lossy();
        let value = match arg.as_ref() {
            "--" => break,
            "--color" => args
                .next()
                .map(|value| value.as_ref().to_string_lossy().into_owned()),
            other => other.strip_prefix("--color=").map(str::to_string),
        };
        if let Some(value) = value
            && let Ok(color) = Color::from_str(&value, false)
        {
            found = color;
        }
    }
    found
}

impl Color {
    pub fn clap(self) -> clap::ColorChoice {
        match self {
            Color::Auto => clap::ColorChoice::Auto,
            Color::Always => clap::ColorChoice::Always,
            Color::Never => clap::ColorChoice::Never,
        }
    }
}

pub fn exit_status() -> clap::builder::StyledStr {
    use mix_events::exit;

    let header = STYLES.get_header();
    let literal = STYLES.get_literal();
    let rows = [
        (exit::SUCCEEDED, "succeeded"),
        (exit::FAILED, "failed"),
        (exit::USAGE, "the arguments were not valid"),
        (
            exit::PROBLEMS_REMAIN,
            "problems remain: `mix doctor` found some, or `mix repair` left some",
        ),
        (exit::INTERRUPTED, "interrupted"),
    ];
    let mut text = format!("{header}Exit status:{header:#}\n");
    for (code, meaning) in rows {
        text.push_str(&format!("  {literal}{code:<3}{literal:#}  {meaning}\n"));
    }
    text.push_str("\nSee 'mix help <command>' for more information on a specific command.");
    text.into()
}

impl Args {
    pub fn parse_with_color() -> Self {
        use clap::{CommandFactory, FromArgMatches};

        let color = color_requested(std::env::args_os().skip(1));
        let matches = Self::command()
            .color(color.clap())
            .after_long_help(exit_status())
            .get_matches();
        Self::from_arg_matches(&matches).unwrap_or_else(|error| error.exit())
    }

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
        check_help(&Args::command(), "mix");
    }

    #[test]
    fn a_color_request_is_found_before_clap_reads_the_arguments() {
        assert_eq!(color_requested(["install", "hello"]), Color::Auto);
        assert_eq!(
            color_requested(["--color", "never", "doctor"]),
            Color::Never
        );
        assert_eq!(color_requested(["doctor", "--color=always"]), Color::Always);
        assert_eq!(
            color_requested(["install", "--", "--color=never"]),
            Color::Auto
        );
        assert_eq!(color_requested(["--color", "sometimes"]), Color::Auto);
    }

    #[test]
    fn the_argument_surface_is_valid() {
        Args::command().debug_assert();
    }

    fn parse(args: &[&str]) -> Args {
        Args::try_parse_from(args).expect("the arguments should parse")
    }

    #[test]
    fn a_code_is_read_as_a_failure_prints_it_and_anything_else_is_a_usage_error() {
        assert!(matches!(
            parse(&["mix", "explain", "not-bootstrapped"]).command,
            Command::Explain {
                code: Some(mix_events::v1::Code::NotBootstrapped),
                list: false
            }
        ));
        let refused = Args::try_parse_from(["mix", "explain", "locked"])
            .err()
            .expect("a code mix no longer uses is refused");
        assert_eq!(refused.exit_code(), mix_events::exit::USAGE as i32);
    }

    #[test]
    fn json_output_implies_plain_output() {
        assert!(!parse(&["mix", "--output", "json", "install", "ripgrep"]).draws_progress());
    }

    #[test]
    fn every_flag_that_shapes_output_works_before_or_after_the_subcommand() {
        let command = Args::command();
        for id in [
            "output",
            "events_file",
            "no_progress",
            "verbose",
            "quiet",
            "color",
        ] {
            let arg = command
                .get_arguments()
                .find(|arg| arg.get_id() == id)
                .unwrap_or_else(|| panic!("no argument {id}"));
            assert!(arg.is_global_set(), "--{id} must be global");
        }
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
        visit(&<Args as clap::CommandFactory>::command(), &mut found);
        assert_eq!(found, Vec::<String>::new());
    }
}
