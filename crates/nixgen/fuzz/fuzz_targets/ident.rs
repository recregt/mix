#![no_main]
use libfuzzer_sys::fuzz_target;
use mix_nixgen::{FileName, Ident, ast::Key, ast::RelPath};

fuzz_target!(|data: &str| {
    if let Ok(ident) = Ident::new(data) {
        let rendered = ident.as_str();
        assert_eq!(
            rendered, data,
            "Ident::new accepted input but mutated its value!"
        );
        assert!(
            !rendered.is_empty(),
            "Accepted empty string as valid Ident!"
        );
        assert!(
            !rendered.contains('\0'),
            "Accepted Ident containing null byte!"
        );
    }

    if let Ok(filename) = FileName::new(data) {
        let s = filename.as_str();
        assert!(
            !s.contains('/'),
            "Accepted FileName containing slash ('/'): {:?}",
            s
        );
        assert!(!s.contains('\0'), "Accepted FileName containing null byte!");
        assert!(
            s != "." && s != "..",
            "Accepted '.' or '..' as valid FileName!"
        );
    }

    let _ = Key::new(data);

    if let Ok(rel) = RelPath::new(data) {
        let path_str = rel.as_str();
        assert!(
            !path_str.starts_with('/'),
            "RelPath started with absolute root ('/')!"
        );
        assert!(
            !path_str.contains('\0'),
            "Accepted RelPath containing null byte!"
        );
    }
});
