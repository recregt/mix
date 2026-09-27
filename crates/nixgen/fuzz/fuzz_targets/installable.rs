#![no_main]

use libfuzzer_sys::fuzz_target;
use mix_nixgen_fuzz::installable::{InstallableInput, decode};

fuzz_target!(|input: InstallableInput| {
    let Some((installable, parts)) = input.build() else {
        return;
    };
    let rendered = installable.render();
    let decoded = decode(&rendered).unwrap_or_else(|error| panic!("{error}\n{rendered}"));
    assert_eq!(decoded, parts, "{rendered}");
});
