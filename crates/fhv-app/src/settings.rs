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
            let _ = write_atomic(&p, &b);
        }
    }

    pub fn add_recent(&mut self, p: PathBuf) {
        self.recent.retain(|r| r != &p);
        self.recent.insert(0, p);
        self.recent.truncate(12);
    }
}

/// Writes a file via a temporary file and a rename, so a crash never
/// leaves it truncated.
pub fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Files under the cache directory with size and modification time.
fn cache_files(dir: &std::path::Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    let mut out = Vec::new();
    for sub in ["index", "transcoded"] {
        if let Ok(rd) = std::fs::read_dir(dir.join(sub)) {
            for e in rd.flatten() {
                if let Ok(m) = e.metadata()
                    && m.is_file()
                {
                    out.push((
                        e.path(),
                        m.len(),
                        m.modified().unwrap_or(std::time::UNIX_EPOCH),
                    ));
                }
            }
        }
    }
    out
}

/// Total size of the index cache and converted copies.
pub fn cache_size(dir: &std::path::Path) -> u64 {
    cache_files(dir).iter().map(|f| f.1).sum()
}

/// Deletes the oldest cache files until the cache is under `max_bytes`
/// (`0` = delete everything). Files of open tabs are rebuilt if needed.
pub fn evict_cache(dir: &std::path::Path, max_bytes: u64) -> u64 {
    let mut files = cache_files(dir);
    files.sort_by_key(|f| f.2);
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    let mut freed = 0;
    for (p, len, _) in files {
        if total <= max_bytes {
            break;
        }
        if std::fs::remove_file(&p).is_ok() {
            total -= len;
            freed += len;
        }
    }
    freed
}
