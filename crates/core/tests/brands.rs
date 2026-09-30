#[test]
fn a_report_is_read_only_through_the_plan_that_closed() {
    let cases = trybuild::TestCases::new();
    cases.pass("tests/ui/own_report.rs");
    cases.compile_fail("tests/ui/crossed_runners.rs");
    cases.compile_fail("tests/ui/forged_token.rs");
    cases.compile_fail("tests/ui/two_sessions.rs");
}
