//! User settings (TOML) and the app's file locations.

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::geometry::Side;

const HEADER: &str = "\
# DroidLeap settings. The tray menu rewrites this file; comments are not kept.
#
# side         desktop edge that leads to the tablet: left, right, top or bottom
# threshold_px push through the edge before switching, in pixels; 0 switches on touch
# timeout_ms   time window in which the push must add up
# serial       USB serial number of the tablet; empty picks the only Android
#              device with USB debugging on
# enabled      false keeps all input on the desktop

";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub side: Side,
    pub threshold_px: f64,
    pub timeout_ms: u64,
    pub serial: String,
    pub enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            side: Side::Left,
            threshold_px: 0.0,
            timeout_ms: 1000,
            serial: String::new(),
            enabled: true,
        }
    }
}

impl Config {
    /// Reads the settings, or the defaults if the file doesn't exist yet.
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        let config: Config =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        config
            .validate()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(config)
    }

    /// Writes the settings, replacing the file atomically.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let body = toml::to_string(self).map_err(|e| format!("cannot encode settings: {e}"))?;
        let temp = path.with_extension("toml.tmp");
        fs::write(&temp, format!("{HEADER}{body}"))
            .and_then(|()| fs::rename(&temp, path))
            .map_err(|e| format!("cannot write {}: {e}", path.display()))
    }

    fn validate(&self) -> Result<(), String> {
        if !self.threshold_px.is_finite() || self.threshold_px < 0.0 {
            return Err(format!(
                "threshold_px must be 0 or more, got {}",
                self.threshold_px
            ));
        }
        if !(1..=10_000).contains(&self.timeout_ms) {
            return Err(format!(
                "timeout_ms must be between 1 and 10000, got {}",
                self.timeout_ms
            ));
        }
        Ok(())
    }

    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }

    pub fn serial(&self) -> Option<&str> {
        Some(self.serial.as_str()).filter(|s| !s.is_empty())
    }
}

/// Where the app keeps its files (XDG locations on Linux).
#[derive(Clone, Debug)]
pub struct Paths {
    pub config_file: PathBuf,
    pub log_file: PathBuf,
    pub lock_file: PathBuf,
    /// Lets the desktop restore its input capture permission without asking again.
    pub restore_token_file: PathBuf,
}

impl Paths {
    pub fn new() -> Result<Paths, String> {
        let dirs = ProjectDirs::from("io.github", "ssk101", "droidleap")
            .ok_or("cannot determine the home directory")?;
        let config_dir = dirs.config_dir();
        let state_dir = dirs.state_dir().unwrap_or(dirs.data_local_dir());
        for dir in [config_dir, state_dir] {
            fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        Ok(Paths {
            config_file: config_dir.join("config.toml"),
            log_file: state_dir.join("droidleap.log"),
            lock_file: state_dir.join("droidleap.lock"),
            restore_token_file: state_dir.join("portal-restore-token"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("droidleap-test-{}-{name}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir.join("config.toml")
    }

    #[test]
    fn missing_file_gives_defaults() {
        let path = temp_file("missing");
        let _ = fs::remove_file(&path);
        assert_eq!(Config::load(&path).unwrap(), Config::default());
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let path = temp_file("partial");
        fs::write(&path, "side = \"bottom\"\nserial = \"R5CT12345XY\"\n").unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.side, Side::Bottom);
        assert_eq!(config.serial(), Some("R5CT12345XY"));
        assert_eq!(config.timeout_ms, 1000);
        assert!(config.enabled);
    }

    #[test]
    fn save_then_load_round_trips() {
        let path = temp_file("roundtrip");
        let config = Config {
            side: Side::Top,
            threshold_px: 250.0,
            enabled: false,
            ..Config::default()
        };
        config.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), config);
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .starts_with("# DroidLeap settings")
        );
    }

    #[test]
    fn rejects_bad_values_and_typos() {
        let path = temp_file("bad");
        for bad in [
            "threshold_px = -1.0",
            "timeout_ms = 0",
            "side = \"middle\"",
            "sied = \"left\"",
        ] {
            fs::write(&path, bad).unwrap();
            assert!(Config::load(&path).is_err(), "{bad}");
        }
    }

    #[test]
    fn empty_serial_means_any_tablet() {
        assert_eq!(Config::default().serial(), None);
    }
}
