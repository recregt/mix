#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Aarch64,
}

impl Arch {
    pub fn current() -> Option<Self> {
        Self::parse(std::env::consts::ARCH)
    }

    fn parse(arch: &str) -> Option<Self> {
        match arch {
            "x86_64" => Some(Self::X86_64),
            "aarch64" => Some(Self::Aarch64),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
        }
    }
}

impl std::fmt::Display for Arch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Linux,
    MacOs,
}

impl Os {
    pub fn current() -> Option<Self> {
        Self::parse(std::env::consts::OS)
    }

    fn parse(os: &str) -> Option<Self> {
        match os {
            "linux" => Some(Self::Linux),
            "macos" => Some(Self::MacOs),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::MacOs => "macos",
        }
    }
}

impl std::fmt::Display for Os {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arch_parse_recognizes_x86_64() {
        assert_eq!(Arch::parse("x86_64"), Some(Arch::X86_64));
    }

    #[test]
    fn arch_parse_recognizes_aarch64() {
        assert_eq!(Arch::parse("aarch64"), Some(Arch::Aarch64));
    }

    #[test]
    fn arch_parse_rejects_an_unknown_arch() {
        assert_eq!(Arch::parse("sparc64"), None);
    }

    #[test]
    fn arch_as_str_round_trips_through_parse() {
        assert_eq!(Arch::parse(Arch::X86_64.as_str()), Some(Arch::X86_64));
        assert_eq!(Arch::parse(Arch::Aarch64.as_str()), Some(Arch::Aarch64));
    }

    #[test]
    fn arch_current_matches_the_compiled_target() {
        assert_eq!(Arch::current(), Arch::parse(std::env::consts::ARCH));
    }

    #[test]
    fn os_parse_recognizes_linux() {
        assert_eq!(Os::parse("linux"), Some(Os::Linux));
    }

    #[test]
    fn os_parse_recognizes_macos() {
        assert_eq!(Os::parse("macos"), Some(Os::MacOs));
    }

    #[test]
    fn os_parse_rejects_an_unknown_os() {
        assert_eq!(Os::parse("plan9"), None);
    }

    #[test]
    fn os_as_str_round_trips_through_parse() {
        assert_eq!(Os::parse(Os::Linux.as_str()), Some(Os::Linux));
        assert_eq!(Os::parse(Os::MacOs.as_str()), Some(Os::MacOs));
    }

    #[test]
    fn os_current_matches_the_compiled_target() {
        assert_eq!(Os::current(), Os::parse(std::env::consts::OS));
    }
}
