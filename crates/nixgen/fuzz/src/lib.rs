pub mod nix;

use arbitrary::Arbitrary;
use mix_nixgen::{FlakeConfig, HomeManagerConfig};

#[derive(Debug, Arbitrary)]
pub enum Op {
    Packages(Vec<String>),
    SetBool(String, bool),
    SetStr(String, String),
}

#[derive(Debug, Arbitrary)]
pub struct Input {
    pub ops: Vec<Op>,
}

#[derive(Debug, Clone)]
pub enum Touch {
    SetStr { path: String, value: String },
    Other { path: String },
}

impl Touch {
    pub fn path(&self) -> &str {
        match self {
            Touch::SetStr { path, .. } => path,
            Touch::Other { path } => path,
        }
    }
}

pub fn render(input: Input) -> String {
    render_with_log(input).0
}

pub fn render_with_log(input: Input) -> (String, Vec<Touch>) {
    let mut cfg = HomeManagerConfig::new();
    let mut log = Vec::new();
    for op in input.ops {
        match op {
            Op::Packages(names) => {
                if cfg.packages(names).is_ok() {
                    log.push(Touch::Other {
                        path: "home.packages".to_string(),
                    });
                }
            }
            Op::SetBool(path, value) => {
                if cfg.set_bool(&path, value).is_ok() {
                    log.push(Touch::Other { path });
                }
            }
            Op::SetStr(path, value) => {
                if cfg.set_str(&path, &value).is_ok() {
                    log.push(Touch::SetStr { path, value });
                }
            }
        }
    }
    (cfg.render(), log)
}

pub fn surviving_str_values(log: &[Touch]) -> Vec<(String, String)> {
    let mut alive: Vec<(Vec<String>, String, String)> = Vec::new();

    for touch in log {
        let segments: Vec<String> = touch.path().split('.').map(String::from).collect();
        alive.retain(|(seg, _, _)| !related(seg, &segments));
        if let Touch::SetStr { path, value } = touch {
            alive.push((segments, path.clone(), value.clone()));
        }
    }

    alive
        .into_iter()
        .map(|(_, path, value)| (path, value))
        .collect()
}

fn related(a: &[String], b: &[String]) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

#[derive(Debug, Arbitrary)]
pub struct FlakeInput {
    pub system: String,
    pub username: String,
}

pub fn render_flake(input: &FlakeInput) -> Option<String> {
    FlakeConfig::new(&input.system, &input.username)
        .ok()
        .map(|cfg| cfg.render())
}

const KEY_PREFIX: &str = "homeConfigurations.";
const KEY_SUFFIX: &str = " = home-manager.lib.homeManagerConfiguration {";

pub fn rendered_username_key(rendered: &str) -> Option<&str> {
    rendered.lines().find_map(|line| {
        line.trim_start()
            .strip_prefix(KEY_PREFIX)?
            .strip_suffix(KEY_SUFFIX)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_for(username: &str) -> String {
        let input = FlakeInput {
            system: "x86_64-linux".to_string(),
            username: username.to_string(),
        };
        let rendered = render_flake(&input).unwrap();
        rendered_username_key(&rendered).unwrap().to_string()
    }

    #[test]
    fn the_key_is_extracted_as_a_quoted_nix_literal() {
        assert_eq!(key_for("mix"), "\"mix\"");
    }

    #[test]
    fn the_key_is_extracted_when_the_username_mimics_the_surrounding_syntax() {
        assert_eq!(
            key_for(" = home-manager.lib.homeManagerConfiguration {"),
            "\" = home-manager.lib.homeManagerConfiguration {\""
        );
        assert_eq!(
            key_for("x\n  homeConfigurations.y"),
            "\"x\\n  homeConfigurations.y\""
        );
    }

    #[test]
    fn a_username_with_a_null_byte_renders_nothing() {
        assert!(
            render_flake(&FlakeInput {
                system: "x86_64-linux".to_string(),
                username: "mi\0x".to_string(),
            })
            .is_none()
        );
    }
}
