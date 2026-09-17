use clap::{ArgAction, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "mix", version, about = "Reproducible systems, made effortless")]
pub struct Cli {
    /// Verbosity: -v steps, -vv commands, -vvv output
    #[arg(short, long, action = ArgAction::Count, global = true)]
    pub verbose: u8,

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
    },

    /// Inspect system health
    Doctor,

    /// Repair configuration drift
    Repair,
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::Cli;

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
}
