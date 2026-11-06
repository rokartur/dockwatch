use serde_json::{Value, json};
use std::path::PathBuf;

/// What dockwatch remembers between launches, in `~/.config/dockwatch/settings.json`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    pub runtime: Option<String>,
    /// The "only running" and "only unused" toggles, per panel.
    pub narrow: Vec<bool>,
}

// Tests build `App`s that load and save settings; keep them off the real home.
#[cfg(test)]
fn path() -> PathBuf {
    std::env::temp_dir().join(format!("dockwatch-test-{}/settings.json", std::process::id()))
}

#[cfg(not(test))]
fn path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    let config = std::env::var("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(home).join(".config"));
    config.join("dockwatch/settings.json")
}

/// Missing or unreadable settings give the defaults.
pub fn load() -> Settings {
    let Ok(bytes) = std::fs::read(path()) else { return Settings::default() };
    parse(&bytes)
}

fn parse(bytes: &[u8]) -> Settings {
    let value: Value = serde_json::from_slice(bytes).unwrap_or_default();
    Settings {
        runtime: value["runtime"].as_str().map(String::from),
        narrow: value["narrow"].as_array().map(|flags| flags.iter().map(|flag| flag.as_bool().unwrap_or(false)).collect()).unwrap_or_default(),
    }
}

/// Best effort: a read-only home only means the next launch starts from the defaults.
pub fn save(settings: &Settings) {
    let path = path();
    let text = json!({"runtime": settings.runtime, "narrow": settings.narrow}).to_string();
    if let Some(folder) = path.parent() {
        let _ = std::fs::create_dir_all(folder);
    }
    let _ = std::fs::write(path, text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_it_writes_and_defaults_on_garbage() {
        let settings = parse(br#"{"runtime": "/h/.orbstack/run/docker.sock", "narrow": [false, true]}"#);
        assert_eq!(settings, Settings { runtime: Some("/h/.orbstack/run/docker.sock".into()), narrow: vec![false, true] });
        assert_eq!(parse(b"not json"), Settings::default());
    }
}
