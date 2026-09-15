#![no_main]

use libfuzzer_sys::fuzz_target;
use mix_nixgen_fuzz::{Input, render};

fuzz_target!(|input: Input| {
    let _ = render(input);
});
