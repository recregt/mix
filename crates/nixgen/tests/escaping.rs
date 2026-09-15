mod support;

use mix_nixgen::HomeManagerConfig;
use proptest::collection::vec;
use proptest::prelude::*;

fn round_trip(raw: &str) -> String {
    let mut cfg = HomeManagerConfig::new();
    cfg.set_str("mix.fuzz.value", raw).unwrap();

    let output = support::eval_raw_attr(&cfg.render(), "mix.fuzz.value");
    assert!(
        output.status.success(),
        "eval failed for {raw:?}:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
#[ignore = "requires nix on PATH"]
fn escaped_strings_round_trip_through_the_real_nix_evaluator() {
    let tricky = [
        "plain",
        "with \"quotes\"",
        "with\\backslash",
        "${interpolation}",
        "a\nnewline\tand\ttab",
        "unicode: héllo wörld 你好",
    ];

    for raw in tricky {
        assert_eq!(round_trip(raw), raw, "round-trip mismatch for {raw:?}");
    }
}

fn arbitrary_nix_content() -> impl Strategy<Value = String> {
    vec(
        any::<char>().prop_filter("no NUL: unrepresentable in a process argv", |c| *c != '\0'),
        0..40,
    )
    .prop_map(|chars| chars.into_iter().collect())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, .. ProptestConfig::default() })]

    #[test]
    #[ignore = "requires nix on PATH"]
    fn fuzzed_strings_round_trip_through_the_real_nix_evaluator(raw in arbitrary_nix_content()) {
        prop_assert_eq!(round_trip(&raw), raw);
    }
}
