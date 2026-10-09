use std::io::Write as _;
use std::process::ExitCode;

use mix_conformance::contract::accounts::Accounts;
use mix_conformance::contract::files::FileTree;
use mix_conformance::contract::git::{GIT, Git};
use mix_conformance::contract::units::Units;
use proptest::test_runner::{
    Config, FailurePersistence, FileFailurePersistence, TestError, TestRunner,
};
use proptest_state_machine::{ReferenceStateMachine, StateMachineTest};

const SUITES: [&str; 4] = ["files", "accounts", "units", "git"];

fn config(seeds: Option<&str>, suite: &str) -> Config {
    let path: Option<&'static str> =
        seeds.map(|dir| &*Box::leak(format!("{dir}/{suite}.proptest-regressions").into()));
    Config {
        failure_persistence: path.map(|path| {
            Box::new(FileFailurePersistence::Direct(path)) as Box<dyn FailurePersistence>
        }),
        source_file: path,
        ..Config::default()
    }
}

fn suite<T>(seeds: Option<&str>, name: &str) -> Result<u32, String>
where
    T: StateMachineTest,
    T::Reference: ReferenceStateMachine,
{
    let config = config(seeds, name);
    let mut runner = TestRunner::new(config.clone());
    let strategy = <T::Reference as ReferenceStateMachine>::sequential_strategy(1..16);
    match runner.run(&strategy, |(state, transitions, seen)| {
        T::test_sequential(config.clone(), state, transitions, seen);
        Ok(())
    }) {
        Ok(()) => Ok(config.cases),
        Err(TestError::Fail(reason, (_, transitions, _))) => Err(format!(
            "{reason}\nminimal failing transitions: {transitions:#?}"
        )),
        Err(TestError::Abort(reason)) => Err(format!("aborted: {reason}")),
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the contract reports to the E2E harness on stdout"
)]
fn report(line: &str) {
    let _ = writeln!(std::io::stdout(), "{line}");
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut chosen = Vec::new();
    let mut seeds = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--seeds" => seeds = args.next(),
            "--git" => {
                if let Some(git) = args.next() {
                    let _ = GIT.set(git.into());
                }
            }
            name if SUITES.contains(&name) => chosen.push(arg),
            other => {
                report(&format!(
                    "unknown argument {other:?}; expected {SUITES:?}, --seeds DIR or --git PATH"
                ));
                return ExitCode::from(2);
            }
        }
    }
    if chosen.is_empty() {
        chosen = SUITES.iter().map(|name| name.to_string()).collect();
    }
    if chosen.iter().any(|name| name == "git") && GIT.get().is_none() {
        report("the git suite checks the pinned git, so it needs --git PATH");
        return ExitCode::from(2);
    }
    if !nix::unistd::Uid::effective().is_root() {
        report("the contract changes accounts, units and owners, so it runs as root");
        return ExitCode::from(2);
    }
    let mut failed = false;
    for name in &chosen {
        let result = match name.as_str() {
            "files" => suite::<FileTree>(seeds.as_deref(), name),
            "accounts" => suite::<Accounts>(seeds.as_deref(), name),
            "units" => suite::<Units>(seeds.as_deref(), name),
            "git" => suite::<Git>(seeds.as_deref(), name),
            _ => unreachable!("only known suites are chosen"),
        };
        match result {
            Ok(cases) => report(&format!("{name}: {cases} cases passed")),
            Err(reason) => {
                failed = true;
                report(&format!("{name}: failed\n{reason}"));
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
