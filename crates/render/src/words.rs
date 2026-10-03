use std::fmt::{self, Display};

use mix_events::Fault;
use mix_events::v1::command::Request;
use mix_explain::Diagnostic;
use mix_ui::{Help, Note, help, note};

pub fn name(request: &Request) -> &'static str {
    match request {
        Request::Bootstrap(_) => "mix bootstrap",
        Request::Install(_) => "mix install",
        Request::Remove(_) => "mix remove",
        Request::Clean(_) => "mix clean",
        Request::Repair(_) => "mix repair",
        Request::Doctor(_) => "mix doctor",
        Request::Explain(_) => "mix explain",
    }
}

pub fn action(request: &Request) -> Box<dyn Display + '_> {
    match request {
        Request::Bootstrap(_) => Box::new("finish setting up `mix`"),
        Request::Install(install) => Box::new(Packages("install", &install.packages)),
        Request::Remove(remove) => Box::new(Packages("remove", &remove.packages)),
        Request::Clean(_) => Box::new("clean up your profile"),
        Request::Repair(_) => Box::new("finish the repair"),
        Request::Doctor(_) => Box::new("finish the health check"),
        Request::Explain(_) => Box::new("explain the code"),
    }
}

pub fn stopping(request: &Request) -> Option<Note> {
    match request {
        Request::Bootstrap(_) => Some(note!("cancelling and cleaning up")),
        Request::Install(_) | Request::Remove(_) => {
            Some(note!("cancelling and putting the package list back"))
        }
        Request::Clean(_) => Some(note!("stopping after the current removal")),
        Request::Repair(_) => Some(note!("stopping after the current repair")),
        Request::Doctor(_) | Request::Explain(_) => None,
    }
}

pub fn outcome(request: &Request, fault: &Fault) -> Diagnostic {
    mix_explain::outcome(name(request), &*action(request), fault)
}

pub fn second_ctrl_c() -> Help {
    help!("press Ctrl-C again to leave it running in the background")
}

pub fn escalating() -> Note {
    note!("root is required, re-running with sudo")
}

pub fn detached() -> Note {
    note!("`mix` is finishing the cleanup in the background")
}

struct Packages<'a>(&'static str, &'a [String]);

impl Display for Packages<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Packages(verb, packages) = self;
        f.write_str(verb)?;
        match packages {
            [] => f.write_str(" the packages"),
            [first, rest @ ..] if rest.len() < 3 => {
                f.write_str(" ")?;
                f.write_str(first)?;
                rest.iter().try_for_each(|package| {
                    f.write_str(", ")?;
                    f.write_str(package)
                })
            }
            packages => write!(f, " {} packages", packages.len()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_list_of_packages_is_counted() {
        let packages: Vec<String> = ["a", "b", "c", "d"].map(String::from).to_vec();

        assert_eq!(Packages("install", &packages[..1]).to_string(), "install a");
        assert_eq!(
            Packages("install", &packages[..3]).to_string(),
            "install a, b, c"
        );
        assert_eq!(
            Packages("install", &packages).to_string(),
            "install 4 packages"
        );
    }

    #[test]
    fn every_request_says_what_could_not_be_done_when_it_fails() {
        use mix_events::v1::{
            CleanRequest, Code, DoctorRequest, ExplainRequest, InstallRequest, RemoveRequest,
            RepairRequest,
        };

        let fault = Fault::failed(Code::Io, "read-only file system", None);
        for (request, action) in [
            (
                Request::Bootstrap(Box::default()),
                "finish setting up `mix`",
            ),
            (
                Request::Install(InstallRequest {
                    packages: vec!["x".to_string()],
                }),
                "install x",
            ),
            (
                Request::Remove(RemoveRequest {
                    packages: vec!["git".to_string()],
                }),
                "remove git",
            ),
            (
                Request::Clean(CleanRequest { all: false }),
                "clean up your profile",
            ),
            (Request::Repair(RepairRequest {}), "finish the repair"),
            (Request::Doctor(DoctorRequest {}), "finish the health check"),
            (
                Request::Explain(ExplainRequest { code: None }),
                "explain the code",
            ),
        ] {
            assert_eq!(
                outcome(&request, &fault).message(),
                format!("couldn't {action}\nrun it again with `-v` to see what went wrong")
            );
        }
    }
}
