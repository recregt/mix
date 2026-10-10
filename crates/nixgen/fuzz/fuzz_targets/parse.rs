#![no_main]
use libfuzzer_sys::fuzz_target;
use mix_nixgen::parse::parse;

fuzz_target!(|data: &str| {
    let parsed = parse(data);

    if let Ok(ast) = parsed {
        let rendered = ast.print();

        let reparsed = parse(&rendered).unwrap_or_else(|err| {
            panic!(
                "ROUND-TRIP FAILED!\nOriginal parsed successfully, but rendered Nix is invalid!\nRendered: {:?}\nError: {:?}",
                rendered, err
            );
        });

        assert_eq!(
            mix_nixgen::parse::normalize(ast),
            mix_nixgen::parse::normalize(reparsed),
            "AST mismatch: Reparsed AST does not match the original AST!"
        );
    }
});
