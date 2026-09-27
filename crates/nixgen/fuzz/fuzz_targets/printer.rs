#![no_main]

use libfuzzer_sys::fuzz_target;
use mix_nixgen::parse::{normalize, parse};
use mix_nixgen_fuzz::generate::ArbExpr;

fuzz_target!(|input: ArbExpr| {
    let expr = input.build();
    let printed = expr.print();
    let parsed = parse(&printed).unwrap_or_else(|error| panic!("{error}\n{printed}"));
    assert_eq!(normalize(parsed), normalize(expr), "{printed}");
});
