#[derive(Debug, Clone, Copy)]
pub enum ManagedArtifact {
    File {
        path: &'static str,
        expected: &'static str,
    },
    Directory {
        path: &'static str,
        mode: u32,
    },
    Group {
        name: &'static str,
    },
    SystemdUnit {
        name: &'static str,
        dest: &'static str,
    },
}

pub type Manifest = &'static [ManagedArtifact];
