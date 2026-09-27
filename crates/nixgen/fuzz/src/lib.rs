pub mod nix;

use arbitrary::Arbitrary;
use std::path::Path;

use mix_nixgen::{FlakeConfig, HomeModule, Rev, StateVersion, System};
use mix_pins::{HOME_MANAGER_REV, NIXPKGS_REV};

#[derive(Debug, Arbitrary)]
pub struct Input {
    pub username: String,
    pub home: String,
    pub packages: Vec<String>,
}

const STATE_VERSION: StateVersion = StateVersion::new_static("24.05");

pub fn render(input: Input) -> Option<String> {
    render_with_values(input).map(|(rendered, _)| rendered)
}

pub fn render_with_values(input: Input) -> Option<(String, Vec<(&'static str, String)>)> {
    let home = format!("/{}", input.home);
    let rendered = HomeModule::new(&input.username, Path::new(&home), STATE_VERSION)
        .ok()?
        .packages(&input.packages)
        .ok()?
        .render();
    Some((
        rendered,
        vec![
            ("home.username", input.username),
            ("home.homeDirectory", home),
        ],
    ))
}

#[derive(Debug, Arbitrary)]
pub struct FlakeInput {
    pub username: String,
}

const NIXPKGS: Rev = Rev::new_static(NIXPKGS_REV);
const HOME_MANAGER: Rev = Rev::new_static(HOME_MANAGER_REV);

pub fn render_flake(input: &FlakeInput) -> Option<String> {
    FlakeConfig::new(System::X86_64Linux, &input.username, NIXPKGS, HOME_MANAGER)
        .ok()
        .map(|cfg| cfg.render())
}

const KEY_PREFIX: &str = "homeConfigurations = {\n      ";
const KEY_SUFFIX: &str = " = home-manager.lib.homeManagerConfiguration {";

pub fn rendered_username_key(rendered: &str) -> Option<&str> {
    let start = rendered.find(KEY_PREFIX)? + KEY_PREFIX.len();
    let end = rendered.rfind(KEY_SUFFIX)?;
    rendered.get(start..end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_for(username: &str) -> String {
        let input = FlakeInput {
            username: username.to_string(),
        };
        let rendered = render_flake(&input).unwrap();
        rendered_username_key(&rendered).unwrap().to_string()
    }

    #[test]
    fn the_key_is_extracted_as_written() {
        assert_eq!(key_for("mix"), "mix");
        assert_eq!(key_for("john.doe"), "\"john.doe\"");
    }

    #[test]
    fn the_key_is_extracted_when_the_username_mimics_the_surrounding_syntax() {
        assert_eq!(
            key_for(" = home-manager.lib.homeManagerConfiguration {"),
            "\" = home-manager.lib.homeManagerConfiguration {\""
        );
        assert_eq!(
            key_for("x\n  homeConfigurations = {\n      y"),
            "\"x\n  homeConfigurations = {\n      y\""
        );
    }

    #[test]
    fn a_username_with_a_null_byte_renders_nothing() {
        assert!(
            render_flake(&FlakeInput {
                username: "mi\0x".to_string(),
            })
            .is_none()
        );
    }
}
