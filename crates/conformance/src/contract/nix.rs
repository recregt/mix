use mix_core::declared::identity::InvokingUser;
use mix_core::declared::paths::GENERATION_LOCK_FILE;
use mix_core::ops::update::Versions;
use mix_exec::Scope;
use mix_home::active_input;
use mix_shell::request::context::Host;
use mix_shell::request::installed::Installed;
use mix_shell::request::upstream::{Nix, Upstream};

pub fn user(name: &str) -> Result<InvokingUser, String> {
    let found = nix::unistd::User::from_name(name)
        .map_err(|error| format!("looking up {name}: {error}"))?
        .ok_or_else(|| format!("no user named {name}"))?;
    Ok(InvokingUser {
        uid: found.uid.as_raw(),
        gid: found.gid.as_raw(),
        name: found.name,
        home: found.dir,
    })
}

pub fn versions_agree_with_the_listing(
    runtime: &tokio::runtime::Runtime,
    user: &InvokingUser,
    mirror: Option<&str>,
) -> Result<u32, String> {
    let scope = Scope::root();
    let lock = active_input(user, GENERATION_LOCK_FILE)
        .ok_or("the active generation's lock could not be read from the Nix store")?;
    let pins = mix_core::declared::pins::read(&String::from_utf8_lossy(&lock))
        .ok_or("the active generation's lock holds no pins")?;
    let listed = runtime
        .block_on(Installed::nix().listing(&Host::Machine, user, mirror, &scope))
        .map_err(|error| format!("listing the profile: {error}"))?
        .ok_or("the user has no active generation")?;
    let names: Vec<String> = listed
        .packages
        .iter()
        .map(|package| package.name.clone())
        .collect();
    if names.is_empty() {
        return Err("the profile lists no packages to compare".to_string());
    }
    let expected: Versions = listed
        .packages
        .iter()
        .map(|package| (package.name.clone(), package.version.clone()))
        .collect();
    let evaluated = runtime
        .block_on(Nix.versions(user, &names, &pins, &scope))
        .map_err(|error| format!("evaluating the packages at the profile's pins: {error}"))?;
    if evaluated != expected {
        return Err(format!(
            "the versions at the profile's own pins were {evaluated:?}, the listing says {expected:?}"
        ));
    }
    u32::try_from(names.len()).map_err(|error| error.to_string())
}
