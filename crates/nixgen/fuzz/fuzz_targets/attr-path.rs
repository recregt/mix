#![no_main]
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use mix_nixgen::AttrPath;

#[derive(Arbitrary, Debug)]
struct AttrInput<'a> {
    segments: Vec<&'a str>,
}

fuzz_target!(|input: AttrInput| {
    let res = AttrPath::new(input.segments.clone());

    if let Ok(path) = res {
        for seg in &input.segments {
            assert!(
                !seg.contains('"'),
                "SECURITY / LOGIC INVARIANT VIOLATION: AttrPath accepted segment with quotes! Segment: {:?}",
                seg
            );
        }

        let flake = mix_nixgen::FlakeRef::path("/").unwrap();
        let rendered = mix_nixgen::Installable::new(flake, path).render();

        let decode_res = mix_nixgen_fuzz::installable::decode(&rendered);
        assert!(
            decode_res.is_ok(),
            "Installable::render() output failed to decode!\nRendered: {:?}\nOriginal segments: {:?}",
            rendered,
            input.segments
        );
    }
});
