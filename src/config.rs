use std::fs;
use std::path::PathBuf;

use directories::ProjectDirs;
use serde::Deserialize;

use crate::Result;
use crate::device::{Firmware, normalize};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: Option<String>,
    #[serde(default = "default_dir")]
    pub dir: String,
    /// Default for `--no-spaces` on `add` and `sync`.
    #[serde(default)]
    pub no_spaces: bool,
    /// Local folder that `x4-cli sync` uploads from; `~/` is expanded.
    pub library: Option<String>,
}

fn default_dir() -> String {
    "/Books".into()
}

fn dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("", "", "x4-cli")
}

pub fn path() -> Option<PathBuf> {
    dirs().map(|d| d.config_dir().join("config.toml"))
}

pub fn load() -> Result<Config> {
    let text = match path().map(fs::read_to_string) {
        Some(Ok(t)) => t,
        Some(Err(e)) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => String::new(),
    };
    let mut cfg: Config = toml::from_str(&text)
        .map_err(|e| format!("{}: {e}", path().unwrap_or_default().display()))?;
    cfg.dir = normalize(&cfg.dir);
    Ok(cfg)
}

/// Sets one key in the config file, keeping the others.
pub fn save(key: &str, value: &str) -> Result<PathBuf> {
    let p = path().ok_or("no config directory on this system")?;
    let mut table: toml::Table = match fs::read_to_string(&p) {
        Ok(t) => t.parse()?,
        Err(_) => toml::Table::new(),
    };
    table.insert(key.into(), value.into());
    fs::create_dir_all(p.parent().unwrap())?;
    fs::write(&p, toml::to_string(&table)?)?;
    Ok(p)
}

fn cache_path() -> Option<PathBuf> {
    dirs().map(|d| d.cache_dir().join("firmware.toml"))
}

fn read_cache() -> toml::Table {
    cache_path()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|t| t.parse().ok())
        .unwrap_or_default()
}

pub fn cached_firmware(host: &str) -> Option<Firmware> {
    read_cache().get(host)?.as_str().and_then(Firmware::parse)
}

/// Best effort: a failed write only costs a re-detect next time.
pub fn cache_firmware(host: &str, fw: Firmware) {
    let Some(p) = cache_path() else { return };
    let mut table = read_cache();
    table.insert(host.into(), fw.as_str().into());
    let _ = fs::create_dir_all(p.parent().unwrap());
    if let Ok(text) = toml::to_string(&table) {
        let _ = fs::write(p, text);
    }
}
