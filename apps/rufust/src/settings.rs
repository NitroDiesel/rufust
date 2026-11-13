//! Preferences kept across launches as `key=value` lines in
//! `$XDG_CONFIG_HOME/rufust/settings.conf` on Linux and
//! `%APPDATA%\Rufust\settings.conf` on Windows, or a `settings.conf` that already
//! sits beside `rufust.exe` (portable use).

#[cfg(windows)]
use std::path::Path;
use std::path::PathBuf;

use crate::units::{SizeUnit, SpeedUnit};
use crate::wue::WueOption;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Settings {
    pub size_unit: SizeUnit,
    pub speed_unit: SpeedUnit,
    /// Windows User Experience choices, like upstream's `WUEOptions`.
    pub wue_options: Vec<WueOption>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            size_unit: SizeUnit::Auto,
            speed_unit: SpeedUnit::MBps,
            wue_options: crate::wue::default_selection(),
        }
    }
}

impl Settings {
    /// Falls back to the file Rufus Linux (this project's name up to 1.0.1)
    /// saved, until Rufust saves its own.
    pub fn load() -> Self {
        let read = |dir: &str| std::fs::read_to_string(path_in(dir)?).ok();
        read(CONFIG_DIR)
            .or_else(|| read(LEGACY_CONFIG_DIR))
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    /// Best effort: an unwritable config directory only loses the preference.
    pub fn save(&self) {
        // Unit tests must not touch the developer's own settings.
        if cfg!(test) {
            return;
        }
        let Some(path) = path_in(CONFIG_DIR) else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, self.serialize());
    }

    fn parse(text: &str) -> Self {
        let mut settings = Self::default();
        for (key, value) in text.lines().filter_map(|line| line.split_once('=')) {
            match key.trim() {
                "size_unit" => {
                    if let Some(unit) = SizeUnit::from_label(value.trim()) {
                        settings.size_unit = unit;
                    }
                }
                "speed_unit" => {
                    if let Some(unit) = SpeedUnit::from_label(value.trim()) {
                        settings.speed_unit = unit;
                    }
                }
                "wue_options" => {
                    settings.wue_options = value
                        .split(',')
                        .filter_map(|key| WueOption::from_key(key.trim()))
                        .collect();
                }
                _ => {}
            }
        }
        settings
    }

    fn serialize(&self) -> String {
        let wue: Vec<_> = crate::wue::remembered(&self.wue_options)
            .into_iter()
            .map(WueOption::key)
            .collect();
        format!(
            "size_unit={}\nspeed_unit={}\nwue_options={}\n",
            self.size_unit.label(),
            self.speed_unit.label(),
            wue.join(",")
        )
    }
}

const CONFIG_DIR: &str = "rufust";
const LEGACY_CONFIG_DIR: &str = "rufus-linux";

#[cfg(windows)]
fn path_in(dir: &str) -> Option<PathBuf> {
    // Rufus Linux never ran on Windows, so there is nothing to migrate.
    if dir == LEGACY_CONFIG_DIR {
        return None;
    }
    windows_path(
        std::env::current_exe().ok().as_deref(),
        std::env::var_os("APPDATA").map(PathBuf::from),
    )
}

/// A `settings.conf` beside the executable makes that copy portable: it is
/// read and written there, so preferences travel with a USB stick. Without
/// one, settings live under `%APPDATA%`.
#[cfg(windows)]
fn windows_path(exe: Option<&Path>, appdata: Option<PathBuf>) -> Option<PathBuf> {
    let beside = exe
        .and_then(Path::parent)
        .map(|folder| folder.join("settings.conf"));
    if let Some(beside) = beside.filter(|path| path.is_file()) {
        return Some(beside);
    }
    let base = appdata.filter(|path| path.is_absolute())?;
    Some(base.join("Rufust").join("settings.conf"))
}

#[cfg(not(windows))]
fn path_in(dir: &str) -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join(dir).join("settings.conf"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_ignores_unknown_or_invalid_lines() {
        let settings = Settings {
            size_unit: SizeUnit::Gb,
            speed_unit: SpeedUnit::Mbps,
            wue_options: vec![WueOption::LocalAccount, WueOption::QualityOfLife],
        };
        assert_eq!(Settings::parse(&settings.serialize()), settings);
        assert_eq!(
            Settings::parse("speed_unit=furlongs\nfuture=1\nsize_unit = MB\n"),
            Settings {
                size_unit: SizeUnit::Mb,
                ..Settings::default()
            }
        );
        // An empty list is a choice; silent install is never restored.
        assert!(Settings::parse("wue_options=\n").wue_options.is_empty());
        assert_eq!(
            Settings::parse("wue_options=silent,regional,bogus\n").wue_options,
            [WueOption::Regional]
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_settings_file_beside_the_executable_is_portable() {
        let folder = std::env::temp_dir().join(format!("rufust-portable-{}", std::process::id()));
        std::fs::create_dir_all(&folder).expect("folder");
        let exe = folder.join("rufust.exe");
        let appdata = PathBuf::from(r"C:\Users\someone\AppData\Roaming");
        assert_eq!(
            windows_path(Some(&exe), Some(appdata.clone())),
            Some(appdata.join("Rufust").join("settings.conf"))
        );
        let beside = folder.join("settings.conf");
        std::fs::write(&beside, "").expect("marker");
        assert_eq!(
            windows_path(Some(&exe), Some(appdata)),
            Some(beside.clone())
        );
        assert_eq!(windows_path(Some(&exe), None), Some(beside));
        std::fs::remove_dir_all(&folder).expect("cleanup");
    }
}
