use arbitrary::Arbitrary;
use mix_nixgen::HomeManagerConfig;

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
