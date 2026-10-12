#![allow(clippy::disallowed_methods)]

use std::sync::Arc;

use futures_util::FutureExt;
use mix_core::declared::identity::InvokingUser;
use mix_core::declared::paths::{HOME_MANAGER_PROFILE_NAME, nix_profiles_dir};
use mix_events::v1::{InstalledPackage, PackageOutput};
use mix_exec::Scope;
use mix_shell::request::installed::{Ask, Gathering, Installed, Source};

fn main() {
    divan::main();
}

struct Recorded(Vec<InstalledPackage>);

impl Source for Recorded {
    fn gather<'a>(&'a self, _ask: Ask<'a>) -> Gathering<'a> {
        let packages = self.0.clone();
        Box::pin(async move { Ok(packages) })
    }
}

fn package(n: usize) -> InstalledPackage {
    InstalledPackage {
        name: format!("package-{n:03}"),
        pname: Some(format!("package-{n:03}")),
        version: Some("1.2.3".to_string()),
        description: Some("a program that does one thing".to_string()),
        homepage: Some("https://example.org".to_string()),
        licenses: vec!["MIT".to_string()],
        outputs: ["bin", "man"]
            .map(|output| PackageOutput {
                output: output.to_string(),
                store_path: format!(
                    "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-package-{n:03}-{output}"
                ),
                nar_size: 4096,
                nar_hash: "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_string(),
                signers: vec!["cache.nixos.org-1".to_string()],
                ..PackageOutput::default()
            })
            .to_vec(),
        ..InstalledPackage::default()
    }
}

#[divan::bench(args = [1, 16, 128])]
fn answer_a_list_from_memory(bencher: divan::Bencher, packages: usize) {
    let home = tempfile::tempdir().unwrap();
    let generation = home.path().join("generation");
    std::fs::create_dir_all(&generation).unwrap();
    let profiles = nix_profiles_dir(home.path());
    std::fs::create_dir_all(&profiles).unwrap();
    std::os::unix::fs::symlink(&generation, profiles.join(HOME_MANAGER_PROFILE_NAME)).unwrap();
    let user = InvokingUser {
        uid: 1000,
        gid: 1000,
        name: "mix-user".to_string(),
        home: home.path().to_path_buf(),
    };
    let installed =
        Installed::with_source(Arc::new(Recorded((0..packages).map(package).collect())))
            .in_store(std::fs::canonicalize(home.path()).unwrap());
    let scope = Scope::root();
    installed
        .listing(&user, None, &scope)
        .now_or_never()
        .expect("a recorded source answers at once")
        .unwrap();

    bencher.bench_local(|| {
        installed
            .listing(divan::black_box(&user), None, &scope)
            .now_or_never()
            .expect("a recorded generation answers at once")
            .unwrap()
            .map(|listed| listed.packages.to_vec())
    });
}
