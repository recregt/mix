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
        gid: u32,
    },
    SystemdUnit {
        name: &'static str,
        dest: &'static str,
        must_be_active: bool,
    },
    PathExists {
        name: &'static str,
        path: &'static str,
    },
}

pub type Manifest = &'static [ManagedArtifact];
