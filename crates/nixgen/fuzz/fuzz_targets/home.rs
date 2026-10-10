#![no_main]
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use mix_nixgen::{HomeModule, StateVersion, parse::parse};
use std::path::PathBuf;

const STATE_VER: StateVersion = StateVersion::new_static("24.05");

#[derive(Arbitrary, Debug)]
struct HomeInput<'a> {
    user: &'a str,
    dir: &'a str,
    packages: Vec<&'a str>,
}

fuzz_target!(|input: HomeInput| {
    let dir_path = PathBuf::from(input.dir);

    let Ok(home) = HomeModule::new(input.user, &dir_path, STATE_VER) else {
        return;
    };
    if let Ok(home) = home.packages(input.packages) {
        let rendered = home.render();
        assert!(
            parse(&rendered).is_ok(),
            "HomeModule::render() produced invalid Nix code!\nUser: {:?}\nDir: {:?}\nRendered Output:\n{}",
            input.user,
            input.dir,
            rendered
        );
    }
});
