//! User settings (theme, fonts, display time zone, recent files, forhunt evidence
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
    /// Recently applied queries, newest first.
    pub query_history: Vec<String>,
    /// Query conditions without `cs` / `ci` match case.
    pub query_case_sensitive: bool,
    /// Last choice of the global search's "only matches" box (D-040).
    pub search_only_matches: bool,
    /// Row-number column mode per file path (D-042); absent = Auto.
    pub row_num: std::collections::BTreeMap<String, RowNumMode>,
    /// Date formats set on columns, per file path: (column, spec) as in
    /// sessions (D-051).
    pub date_formats: std::collections::BTreeMap<String, Vec<(String, String)>>,
    /// Theme name or "System" (D-056); None: from `dark` (older settings).
    pub theme: Option<String>,
    /// Grid font, its size in points, monospace-only font list (D-057).
    pub grid_font: GridFont,
    pub grid_font_size: f32,
    pub mono_only: bool,
    /// Scale of the whole UI (egui zoom factor).
    pub ui_scale: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum GridFont {
    Bundled(String),
    /// An installed family.
    System(String),
    File(PathBuf),
}

impl Default for GridFont {
    fn default() -> Self {
        GridFont::Bundled("JetBrains Mono".into())
    }
}

pub const FONT_SIZE: f32 = 12.5;
pub const FONT_MIN: f32 = 8.0;
pub const FONT_MAX: f32 = 32.0;

/// What to do with the viewer's own `#` column when the file has a
/// row-number column of its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RowNumMode {
    /// Hide `#` when the file's column is detected.
    #[default]
    Auto,
    /// Always show `#`, numbering file rows.
    Always,
    /// Never show `#`.
    Never,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            dark: true,
            tz: "UTC".into(),
            recent: Vec::new(),
            evidence: String::new(),
            detail_pane: true,
            query_history: Vec::new(),
            query_case_sensitive: false,
            search_only_matches: true,
            row_num: Default::default(),
            date_formats: Default::default(),
            theme: None,
            grid_font: GridFont::default(),
            grid_font_size: FONT_SIZE,
            mono_only: true,
            ui_scale: 1.0,
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
/// pulls in an MPL-2.0 dependency (docs/DECISIONS.md D-019).
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
        let mut s: Settings = std::fs::read(Self::path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        s.grid_font_size = clamp_size(s.grid_font_size);
        s.ui_scale = clamp_scale(s.ui_scale);
        s
    }

    pub fn theme_name(&self) -> String {
        self.theme
            .clone()
            .unwrap_or_else(|| if self.dark { "Dark" } else { "Light" }.into())
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

pub fn clamp_size(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(FONT_MIN, FONT_MAX)
    } else {
        FONT_SIZE
    }
}

pub fn clamp_scale(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.5, 3.0)
    } else {
        1.0
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
    fn files_in(d: &std::path::Path, out: &mut Vec<(PathBuf, u64, std::time::SystemTime)>) {
        if let Ok(rd) = std::fs::read_dir(d) {
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
    for sub in ["index", "transcoded"] {
        files_in(&dir.join(sub), &mut out);
    }
    // CSVs written by forhunt: forhunt/<key>/<name>.csv (D-033).
    if let Ok(rd) = std::fs::read_dir(dir.join("forhunt")) {
        for e in rd.flatten() {
            files_in(&e.path(), &mut out);
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
