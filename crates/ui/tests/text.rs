#[test]
fn text_that_breaks_the_style_does_not_build() {
    let cases = trybuild::TestCases::new();
    cases.pass("tests/text/follows_the_style.rs");
    cases.compile_fail("tests/text/capital_phrase.rs");
    cases.compile_fail("tests/text/full_stop.rs");
    cases.compile_fail("tests/text/two_sentences.rs");
    cases.compile_fail("tests/text/help_without_instruction.rs");
    cases.compile_fail("tests/text/note_with_instruction.rs");
    cases.compile_fail("tests/text/help_parts_without_instruction.rs");
    cases.compile_fail("tests/text/lowercase_sentence.rs");
    cases.compile_fail("tests/text/phrase_parts_capital.rs");
}
