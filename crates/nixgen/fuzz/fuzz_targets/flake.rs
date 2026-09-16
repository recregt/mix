#![no_main]

use libfuzzer_sys::fuzz_target;
use mix_nixgen_fuzz::{FlakeInput, render_flake};

fuzz_target!(|input: FlakeInput| {
    let _ = render_flake(&input);
});
