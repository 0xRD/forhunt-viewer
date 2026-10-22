//! User settings (theme, display time zone, recent files, forhunt evidence
//! path), stored as JSON in the OS config directory.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub dark: bool,
    /// IANA time zone for display ("UTC" = no conversion).
    pub tz: String,
    pub recent: Vec<PathBuf>,
    /// Evidence path used in copied `forhunt show` commands.
    pub evidence: String,
    pub detail_pane: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            dark: true,
            tz: "UTC".into(),
            recent: Vec::new(),
            evidence: String::new(),
            detail_pane: true,
        }
    }
}

pub struct Dirs {
    pub config: PathBuf,
    pub cache: PathBuf,
}

/// OS config and cache directories (never next to the evidence), following
/// each platform's convention (XDG on Linux, ~/Library on macOS,
/// %APPDATA% / %LOCALAPPDATA% on Windows). Written by hand: the usual crate
/// pulls in an MPL-2.0 dependency (DECISIONS.md D-019).
pub fn dirs() -> Dirs {
    const APP: &str = "forhunt-viewer";
    let env = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let home = env("HOME").or_else(|| env("USERPROFILE"));
    let fallback = || std::env::temp_dir().join(APP);
    if cfg!(windows) {
        let roaming = env("APPDATA").unwrap_or_else(fallback);
        let local = env("LOCALAPPDATA").unwrap_or_else(fallback);
        Dirs {
            config: roaming.join(APP),
            cache: local.join(APP).join("cache"),
        }
    } else if cfg!(target_os = "macos") {
        let lib = home.map(|h| h.join("Library")).unwrap_or_else(fallback);
        Dirs {
            config: lib.join("Application Support").join(APP),
            cache: lib.join("Caches").join(APP),
        }
    } else {
        let config = env("XDG_CONFIG_HOME").or_else(|| home.as_ref().map(|h| h.join(".config")));
        let cache = env("XDG_CACHE_HOME").or_else(|| home.as_ref().map(|h| h.join(".cache")));
        Dirs {
            config: config
                .map(|c| c.join(APP))
                .unwrap_or_else(|| fallback().join("config")),
            cache: cache
                .map(|c| c.join(APP))
                .unwrap_or_else(|| fallback().join("cache")),
        }
    }
}

impl Settings {
    fn path() -> PathBuf {
        dirs().config.join("settings.json")
    }

    pub fn load() -> Settings {
        std::fs::read(Self::path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if cfg!(test) {
            return;
        }
        let p = Self::path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(b) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(p, b);
        }
    }

    pub fn add_recent(&mut self, p: PathBuf) {
        self.recent.retain(|r| r != &p);
        self.recent.insert(0, p);
        self.recent.truncate(12);
    }
}
