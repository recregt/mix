//! A child program for tests that needs nothing from the host: the test binary starts itself.
//!
//! A test binary that calls [`install!`] checks its command line before `main`. Started with
//! [`FLAG`], it does the steps that follow it and exits, so the test harness never runs and
//! writes nothing of its own. Every step that waits says so on stdout first, so a test waits for
//! what the child reports instead of for time to pass.
//!
//! The steps, done in order:
//!
//! - `print:TEXT` and `eprint:TEXT` write a line to stdout or stderr.
//! - `echo:PREFIX` reads a line from stdin and writes it back after `PREFIX`.
//! - `pid` writes the child's process id.
//! - `env` writes the child's environment, a `NAME=value` line each.
//! - `trap-term` turns a SIGTERM into a `term` line on stdout instead of an exit.
//! - `spawn` starts a grandchild that sleeps, in the same process group, and writes its id.
//! - `wait` waits for every grandchild.
//! - `sleep` waits for signals until one ends the child.
//! - `count-stderr:N` writes the numbers 1 to N to stderr, a line each.
//! - `zeros:N` writes N zero bytes to stdout.
//! - `exit:N` exits with status N; without it the child exits with 0 after its last step.
//!
//! Note: The command line is read from the arguments glibc hands to initialisers, so the child
//! works on glibc only.

#![expect(
    clippy::disallowed_methods,
    reason = "the child is a program of its own, and stdout and stderr are what it says"
)]

use std::ffi::{CStr, c_char, c_int};
use std::io::{BufRead, Write};
use std::os::fd::BorrowedFd;
use std::path::PathBuf;
use std::process::Child;

use nix::sys::signal::{SigHandler, Signal, signal};

/// The first argument that makes a test binary the child.
pub const FLAG: &str = "--mix-test-child";

/// Makes the test binary it is called in able to start as the child.
#[macro_export]
macro_rules! install {
    () => {
        #[used]
        #[unsafe(link_section = ".init_array")]
        static MIX_TEST_CHILD: unsafe extern "C" fn(
            ::std::ffi::c_int,
            *const *const ::std::ffi::c_char,
            *const *const ::std::ffi::c_char,
        ) = $crate::serve;
    };
}

/// The program to start: the running test binary.
pub fn program() -> PathBuf {
    std::env::current_exe().expect("a test binary knows where it is")
}

/// The arguments that make [`program`] the child doing `steps`.
pub fn args<'s>(steps: impl IntoIterator<Item = &'s str>) -> Vec<String> {
    std::iter::once(FLAG)
        .chain(steps)
        .map(str::to_string)
        .collect()
}

/// Runs the steps and exits when the process was started as the child; returns otherwise.
///
/// # Safety
///
/// `argv` holds `argc` valid NUL-terminated strings, as glibc hands every initialiser.
pub unsafe extern "C" fn serve(argc: c_int, argv: *const *const c_char, _: *const *const c_char) {
    let count = usize::try_from(argc).unwrap_or(0);
    if count < 2 || argv.is_null() {
        return;
    }
    let args: Vec<String> = (0..count)
        .map(|index| {
            // SAFETY: the caller hands `argc` valid NUL-terminated strings in `argv`.
            unsafe { CStr::from_ptr(*argv.add(index)) }
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    if args[1] != FLAG {
        return;
    }
    std::process::exit(run(&args[2..]));
}

extern "C" fn on_term(_: c_int) {
    // SAFETY: stdout stays open for the life of the child.
    let stdout = unsafe { BorrowedFd::borrow_raw(1) };
    let _ = nix::unistd::write(stdout, b"term\n");
}

fn line(mut out: impl Write, text: &str) {
    writeln!(out, "{text}").expect("the test reads what the child writes");
    out.flush().expect("the test reads what the child writes");
}

fn run(steps: &[String]) -> i32 {
    let mut grandchildren: Vec<Child> = Vec::new();
    for step in steps {
        let (name, value) = step.split_once(':').unwrap_or((step, ""));
        match name {
            "print" => line(std::io::stdout(), value),
            "eprint" => line(std::io::stderr(), value),
            "echo" => {
                let mut read = String::new();
                std::io::stdin()
                    .lock()
                    .read_line(&mut read)
                    .expect("the test writes a line");
                line(std::io::stdout(), &format!("{value}{}", read.trim_end()));
            }
            "pid" => line(std::io::stdout(), &std::process::id().to_string()),
            "env" => {
                for (name, value) in std::env::vars_os() {
                    line(
                        std::io::stdout(),
                        &format!("{}={}", name.to_string_lossy(), value.to_string_lossy()),
                    );
                }
            }
            "trap-term" => {
                // SAFETY: the handler only writes to a descriptor, which is safe in a handler.
                unsafe { signal(Signal::SIGTERM, SigHandler::Handler(on_term)) }
                    .expect("SIGTERM can be handled");
            }
            "spawn" => {
                let grandchild = std::process::Command::new(program())
                    .args(args(["sleep"]))
                    .spawn()
                    .expect("the child can start itself");
                line(std::io::stdout(), &grandchild.id().to_string());
                grandchildren.push(grandchild);
            }
            "wait" => {
                for mut grandchild in grandchildren.drain(..) {
                    let _ = grandchild.wait();
                }
            }
            "sleep" => loop {
                nix::unistd::pause();
            },
            "count-stderr" => {
                let mut stderr = std::io::stderr().lock();
                for number in 1..=value.parse::<u64>().expect("a count") {
                    writeln!(stderr, "{number}").expect("the test reads what the child writes");
                }
            }
            "zeros" => {
                let zeros = vec![0; value.parse().expect("a byte count")];
                std::io::stdout()
                    .write_all(&zeros)
                    .expect("the test reads what the child writes");
            }
            "exit" => return value.parse().expect("an exit status"),
            other => panic!("the child has no step {other:?}"),
        }
    }
    0
}

/// Returns once process `pid` has ended, at once when it is already gone.
pub fn ended(pid: i32) {
    use std::os::fd::{FromRawFd, OwnedFd};

    use nix::poll::{PollFd, PollFlags, PollTimeout, poll};

    // SAFETY: pidfd_open takes a process id and flags and returns a new descriptor or -1.
    let fd = unsafe { nix::libc::syscall(nix::libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        return;
    }
    // SAFETY: the descriptor was just returned to us and nothing else owns it.
    let pidfd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
    let mut ready = [PollFd::new(
        std::os::fd::AsFd::as_fd(&pidfd),
        PollFlags::POLLIN,
    )];
    while poll(&mut ready, PollTimeout::NONE).is_err() {}
}
