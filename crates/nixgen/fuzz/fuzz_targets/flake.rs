#![no_main]
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use mix_nixgen::{FlakeConfig, Rev, System, parse::parse};

const DUMMY_REV: Rev = Rev::new_static("0123456789abcdef0123456789abcdef01234567");

#[derive(Arbitrary, Debug)]
struct FlakeInput<'a> {
    system_index: u8,
    user: &'a str,
}

fuzz_target!(|input: FlakeInput| {
    let system = match input.system_index % 4 {
        0 => System::X86_64Linux,
        1 => System::Aarch64Linux,
        2 => System::X86_64Darwin,
        _ => System::Aarch64Darwin,
    };

    if let Ok(config) = FlakeConfig::new(system, input.user, DUMMY_REV, DUMMY_REV) {
        let nix_code = config.render();

        let parse_result = parse(&nix_code);
        assert!(
            parse_result.is_ok(),
            "TEMPLATE INJECTION / SYNTAX ERROR! FlakeConfig generated invalid Nix code from accepted inputs!\nUser: {:?}\nSystem: {:?}\nGenerated Nix:\n{}",
            input.user,
            system.as_str(),
            nix_code
        );
    }
});
