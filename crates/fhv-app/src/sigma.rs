//! Sigma detection (D-101 to D-106): rule sets chosen in Settings
//! (Yamato Security's hayabusa-rules, downloaded on request, and folders of
//! your own), run on demand over a tab with forhunt's engine
//! (`forhunt_core::sigma`), results shown as a level marker and tint on the
//! rows, a detections panel that filters, and the rules of a row in the
//! detail pane.
//!
//! Rows become events from the CSV itself: the `Channel`, `EventId`,
//! `Provider`, `Computer`... columns and the `Payload` JSON of forhunt and
//! EvtxECmd CSVs, or every column of any other CSV. Only rows whose
//! (Channel, EventId) some rule can match are read (from the columns'
//! dictionaries, no file read), in parallel and cancellable.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Instant;

use forhunt_core::sigma::{LoadOptions, LoadReport, RuleSet};
use forhunt_core::{EventView, Field, Val};
use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};

use crate::tab::Tab;

pub const LICENSE_URL: &str = "https://github.com/SigmaHQ/Detection-Rule-License";
/// What was downloaded, written beside the rules.
const INFO_FILE: &str = ".forhunt-viewer-rules";
/// Largest download accepted (LOLDrivers, the largest, is ~100 MB).
const MAX_DOWNLOAD: u64 = 512 << 20;
/// Largest unpacked archive and number of entries accepted.
const MAX_UNPACKED: u64 = 4 << 30;
const MAX_ENTRIES: usize = 500_000;

/// A public rule set Settings offers to download (D-107).
pub struct Source {
    /// Folder under the rules root.
    pub slug: &'static str,
    pub name: &'static str,
    pub home: &'static str,
    /// GitHub archive of the default branch: the newest rules.
    pub archive: &'static str,
    pub licence: &'static str,
    pub licence_url: &'static str,
    pub about: &'static str,
    /// Only these folders of the archive are kept (empty: all).
    pub keep: &'static [&'static str],
    pub size: &'static str,
}

pub const CATALOG: &[Source] = &[
    Source {
        slug: "hayabusa-rules",
        name: "Yamato Security hayabusa-rules",
        home: "https://github.com/Yamato-Security/hayabusa-rules",
        archive: "https://github.com/Yamato-Security/hayabusa-rules/archive/refs/heads/main.tar.gz",
        licence: "DRL 1.1",
        licence_url: LICENSE_URL,
        about: "4,600+ Sigma and Hayabusa rules for Windows event logs, SigmaHQ's included, with channels and event ids written in.",
        keep: &["hayabusa", "sigma", "config"],
        size: "10 MB",
    },
    Source {
        slug: "sigmahq",
        name: "SigmaHQ",
        home: "https://github.com/SigmaHQ/sigma",
        archive: "https://github.com/SigmaHQ/sigma/archive/refs/heads/master.tar.gz",
        licence: "DRL 1.1",
        licence_url: LICENSE_URL,
        about: "The reference Sigma rules (~2,700 for Windows), scoped by their log source. With hayabusa-rules also used, its converted copies run instead of the originals.",
        keep: &[
            "rules",
            "rules-emerging-threats",
            "rules-threat-hunting",
            "rules-placeholder",
            "rules-compliance",
            "rules-dfir",
        ],
        size: "40 MB",
    },
    Source {
        slug: "mdecrevoisier",
        name: "mdecrevoisier SIGMA-detection-rules",
        home: "https://github.com/mdecrevoisier/SIGMA-detection-rules",
        archive: "https://github.com/mdecrevoisier/SIGMA-detection-rules/archive/refs/heads/main.tar.gz",
        licence: "CC0 1.0",
        licence_url: "https://creativecommons.org/publicdomain/zero/1.0/",
        about: "~300 rules on Active Directory and Windows security logs, many correlations (enumeration, brute force).",
        keep: &[],
        size: "1 MB",
    },
    Source {
        slug: "mbabinski",
        name: "mbabinski Sigma-Rules",
        home: "https://github.com/mbabinski/Sigma-Rules",
        archive: "https://github.com/mbabinski/Sigma-Rules/archive/refs/heads/main.tar.gz",
        licence: "GPL-3.0",
        licence_url: "https://www.gnu.org/licenses/gpl-3.0.html",
        about: "~200 rules from threat reports (ransomware groups, Red Canary reports).",
        keep: &[],
        size: "1 MB",
    },
    Source {
        slug: "loldrivers",
        name: "LOLDrivers",
        home: "https://github.com/magicsword-io/LOLDrivers",
        archive: "https://github.com/magicsword-io/LOLDrivers/archive/refs/heads/main.tar.gz",
        licence: "Apache-2.0",
        licence_url: "https://www.apache.org/licenses/LICENSE-2.0",
        about: "1,300+ rules for vulnerable and malicious drivers being loaded (Sysmon 6), by name and hash.",
        keep: &["detections/sigma"],
        size: "100 MB download",
    },
];

pub fn source(slug: &str) -> Option<&'static Source> {
    CATALOG.iter().find(|s| s.slug == slug)
}

pub const LEVELS: [&str; 6] = ["", "informational", "low", "medium", "high", "critical"];

/// Palette color of a level (theme rule palette: readable in every theme).
pub fn level_color(level: u8) -> &'static str {
    match level {
        5 => "red",
        4 => "orange",
        3 => "yellow",
        2 => "blue",
        _ => "gray",
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SigmaSettings {
    /// Downloaded rule sets in use (catalog slugs), when downloaded.
    pub sets: Vec<String>,
    /// Folders (or files) of your own rules.
    pub folders: Vec<PathBuf>,
    /// How a folder's rules use their logsource (`auto` when absent,
    /// `sigma`, `as-written`; D-067 of forhunt).
    pub profiles: std::collections::BTreeMap<PathBuf, String>,
    /// Lowest rule level run (1 informational .. 5 critical).
    pub min_level: u8,
    /// Also run Hayabusa's noisy rules.
    pub noisy: bool,
    /// Rows at or above this level get a light fill of the level color
    /// (0: none; every detected row keeps its marker in the row gutter).
    pub tint_level: u8,
}

impl Default for SigmaSettings {
    fn default() -> Self {
        SigmaSettings {
            sets: vec!["hayabusa-rules".into()],
            folders: Vec::new(),
            profiles: Default::default(),
            min_level: 2,
            noisy: false,
            tint_level: 3,
        }
    }
}

/// Rules root: next to the other per-user data, never in the cache (which
/// is evicted) nor near the evidence.
pub fn default_root() -> PathBuf {
    let env = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if cfg!(windows) {
        if let Some(p) = env("LOCALAPPDATA") {
            return p.join("forhunt-viewer").join("rules");
        }
    } else if cfg!(target_os = "macos") {
        return crate::settings::dirs().config.join("rules");
    } else if let Some(p) =
        env("XDG_DATA_HOME").or_else(|| env("HOME").map(|h| h.join(".local/share")))
    {
        return p.join("forhunt-viewer").join("rules");
    }
    crate::settings::dirs().config.join("rules")
}

/// The downloaded rule set, as recorded beside it.
#[derive(Clone, Debug, Default)]
pub struct Installed {
    pub commit: String,
    pub downloaded: String,
    pub files: usize,
}

/// A full git commit id (anything else from an archive is not shown).
fn is_commit(c: &str) -> bool {
    c.len() == 40 && c.bytes().all(|b| b.is_ascii_hexdigit())
}

/// What is recorded about the downloaded set `slug`.
pub fn installed(root: &Path, slug: &str) -> Option<Installed> {
    let dir = root.join(slug);
    let text = std::fs::read_to_string(dir.join(INFO_FILE)).ok()?;
    let get = |k: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(k).and_then(|v| v.strip_prefix('=')))
            .unwrap_or("")
            .to_string()
    };
    let commit = get("commit");
    Some(Installed {
        commit: if is_commit(&commit) {
            commit
        } else {
            String::new()
        },
        downloaded: get("downloaded"),
        files: get("files").parse().unwrap_or(0),
    })
}

// ------------------------------------------------------------------ library

#[derive(Clone, Debug, PartialEq)]
struct LoadKey {
    paths: Vec<PathBuf>,
    profiles: Vec<(PathBuf, String)>,
    min_level: u8,
    noisy: bool,
}

pub struct Download {
    pub slug: &'static str,
    rx: Receiver<Result<String, String>>,
    pub bytes: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    pub started: Instant,
}

impl Drop for Download {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// The compiled rules every tab runs (one load, shared), and downloads.
pub struct Library {
    pub root: PathBuf,
    pub set: Option<Arc<RuleSet>>,
    pub report: Option<LoadReport>,
    /// Bumped at each (re)load: results of older generations are stale.
    pub generation: u64,
    loaded_for: Option<LoadKey>,
    loading: Option<(LoadKey, Receiver<(RuleSet, LoadReport)>)>,
    pub download: Option<Download>,
    /// Outcome of the last download (slug, result), for Settings.
    pub message: Option<(&'static str, Result<String, String>)>,
    /// Downloaded sets, by slug.
    pub installed: std::collections::BTreeMap<&'static str, Installed>,
    pub load_error: Option<String>,
    /// Settings the current load was decided for (the check runs once per
    /// change, not every frame).
    seen: Option<SigmaSettings>,
}

impl Library {
    pub fn new(root: PathBuf) -> Library {
        let mut l = Library {
            root,
            set: None,
            report: None,
            generation: 0,
            loaded_for: None,
            loading: None,
            download: None,
            message: None,
            installed: Default::default(),
            load_error: None,
            seen: None,
        };
        l.refresh_installed();
        l
    }

    /// Loads the rules again (rule files edited on disk).
    pub fn reload(&mut self) {
        self.loaded_for = None;
        self.seen = None;
    }

    fn refresh_installed(&mut self) {
        self.seen = None;
        self.installed = CATALOG
            .iter()
            .filter_map(|s| Some((s.slug, installed(&self.root, s.slug)?)))
            .collect();
    }

    /// Rule paths the settings select (downloaded sets only if present).
    pub fn paths(&self, s: &SigmaSettings) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = CATALOG
            .iter()
            .filter(|c| s.sets.iter().any(|x| x == c.slug))
            .map(|c| self.root.join(c.slug))
            .filter(|p| p.is_dir())
            .collect();
        v.extend(s.folders.iter().cloned());
        v
    }

    pub fn loading(&self) -> bool {
        self.loading.is_some()
    }

    /// Rules usable now (loaded, at least one).
    pub fn ready(&self) -> Option<(Arc<RuleSet>, u64)> {
        self.set
            .clone()
            .filter(|s| !s.is_empty())
            .map(|s| (s, self.generation))
    }

    /// Loads the rules again when the settings changed, collects loads and
    /// downloads. Returns true while one runs.
    pub fn poll(&mut self, s: &SigmaSettings) -> bool {
        if let Some(d) = &self.download {
            match d.rx.try_recv() {
                Ok(r) => {
                    let slug = d.slug;
                    self.download = None;
                    if r.is_ok() {
                        self.refresh_installed();
                        self.reload();
                    }
                    self.message = Some((slug, r));
                }
                Err(TryRecvError::Disconnected) => {
                    let slug = d.slug;
                    self.download = None;
                    self.message = Some((slug, Err("the download stopped unexpectedly".into())));
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some((key, rx)) = &self.loading {
            match rx.try_recv() {
                Ok((set, rep)) => {
                    self.load_error = None;
                    self.loaded_for = Some(key.clone());
                    self.set = Some(Arc::new(set));
                    self.report = Some(rep);
                    self.generation += 1;
                    self.loading = None;
                }
                Err(TryRecvError::Disconnected) => {
                    // The load failed: no rules rather than the previous ones.
                    self.loaded_for = Some(key.clone());
                    self.loading = None;
                    self.set = None;
                    self.report = None;
                    self.load_error = Some("the rules could not be loaded (internal error)".into());
                    self.generation += 1;
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        // Nothing changed since the last check: no file system work.
        if self.loading.is_none() && self.loaded_for.is_some() && self.seen.as_ref() == Some(s) {
            return self.download.is_some();
        }
        self.seen = Some(s.clone());
        let key = LoadKey {
            paths: self.paths(s),
            profiles: s
                .profiles
                .iter()
                .map(|(p, x)| (p.clone(), x.clone()))
                .collect(),
            min_level: s.min_level.clamp(1, 5),
            noisy: s.noisy,
        };
        let stale_load = self.loading.as_ref().is_some_and(|(k, _)| *k != key);
        if self.loaded_for.as_ref() != Some(&key)
            && (self.loading.is_none() || stale_load)
            && self.download.is_none()
        {
            if key.paths.is_empty() {
                if self.set.is_some() || self.report.is_some() {
                    self.generation += 1;
                }
                self.set = None;
                self.report = None;
                self.loaded_for = Some(key);
                self.loading = None;
            } else {
                let (tx, rx) = std::sync::mpsc::channel();
                let k = key.clone();
                let _ = std::thread::Builder::new()
                    .name("fhv-sigma-load".into())
                    .spawn(move || {
                        let opts = LoadOptions {
                            min_level: k.min_level,
                            noisy: k.noisy,
                            profiles: k
                                .profiles
                                .iter()
                                .filter_map(|(p, x)| {
                                    Some((p.clone(), forhunt_core::sigma::Profile::parse(x)?))
                                })
                                .collect(),
                            ..Default::default()
                        };
                        if let Ok(r) =
                            std::panic::catch_unwind(|| forhunt_core::sigma::load(&k.paths, &opts))
                        {
                            let _ = tx.send(r);
                        }
                    });
                self.loading = Some((key, rx));
            }
        }
        self.loading.is_some() || self.download.is_some()
    }

    /// Downloads the newest rules of a catalog set in the background.
    pub fn start_download(&mut self, slug: &str) {
        let Some(src) = source(slug) else {
            return;
        };
        if self.download.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let bytes = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (root, b, c) = (self.root.clone(), bytes.clone(), cancel.clone());
        let _ = std::thread::Builder::new()
            .name("fhv-sigma-download".into())
            .spawn(move || {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    download(&root, src, &b, &c)
                }))
                .unwrap_or_else(|_| Err("internal error (panic)".into()));
                let _ = tx.send(r);
            });
        self.message = None;
        self.download = Some(Download {
            slug: src.slug,
            rx,
            bytes,
            cancel,
            started: Instant::now(),
        });
    }

    pub fn cancel_download(&mut self) {
        if let Some(d) = self.download.take() {
            self.message = Some((d.slug, Err("download cancelled".into())));
        }
    }

    /// Deletes a downloaded set (and stops using it).
    pub fn remove(&mut self, slug: &str) -> Result<(), String> {
        let src = source(slug).ok_or("unknown set")?;
        std::fs::remove_dir_all(self.root.join(src.slug)).map_err(|e| e.to_string())?;
        self.refresh_installed();
        self.reload();
        Ok(())
    }
}

/// Counts bytes and stops when cancelled.
struct Counting<R> {
    inner: R,
    n: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("cancelled"));
        }
        let k = self.inner.read(buf)?;
        self.n.fetch_add(k as u64, Ordering::Relaxed);
        Ok(k)
    }
}

fn agent() -> ureq::Agent {
    use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
    let tls = TlsConfig::builder()
        .provider(TlsProvider::Rustls)
        .root_certs(RootCerts::PlatformVerifier)
        .unversioned_rustls_crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .user_agent(concat!("forhunt-viewer/", env!("CARGO_PKG_VERSION")))
        .timeout_connect(Some(std::time::Duration::from_secs(30)))
        .timeout_global(Some(std::time::Duration::from_secs(900)))
        .build()
        .into()
}

/// Downloads a catalog set (a GitHub tar.gz) and installs it as
/// `<root>/<slug>`, replacing the previous copy only once the new one is
/// complete and holds rules.
pub fn download(
    root: &Path,
    src: &Source,
    bytes: &Arc<AtomicU64>,
    cancel: &Arc<AtomicBool>,
) -> Result<String, String> {
    let url = src.archive;
    std::fs::create_dir_all(root).map_err(|e| format!("cannot create {}: {e}", root.display()))?;
    let resp = agent()
        .get(url)
        .call()
        .map_err(|e| format!("download failed: {e}"))?;
    let reader = resp
        .into_body()
        .into_with_config()
        .limit(MAX_DOWNLOAD)
        .reader();
    let r = Counting {
        inner: reader,
        n: bytes.clone(),
        cancel: cancel.clone(),
    };
    install_tar_gz(root, r, src)
}

/// Unpacks a rule set's tar.gz into `<root>/<slug>` (staged, then renamed
/// into place), keeping only the source's rule folders and its licence and
/// readme files.
pub fn install_tar_gz(root: &Path, r: impl Read, src: &Source) -> Result<String, String> {
    // Unique per download (a cancelled download blocked in a read may still
    // be running); leftovers of earlier runs are swept.
    sweep_leftovers(root);
    let nonce = format!(
        "{}-{}-{}",
        src.slug,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    );
    let stage = root.join(format!(".staging-{nonce}"));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage).map_err(|e| e.to_string())?;
    let res = (|| {
        // Bounded unpacked size and entry count (a hostile archive).
        let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(r).take(MAX_UNPACKED));
        let mut entries = 0usize;
        let mut commit = String::new();
        for e in ar
            .entries()
            .map_err(|e| format!("not a tar.gz archive: {e}"))?
        {
            let mut e = e.map_err(|e| format!("damaged archive: {e}"))?;
            entries += 1;
            if entries > MAX_ENTRIES {
                return Err(format!("the archive holds more than {MAX_ENTRIES} entries"));
            }
            let ty = e.header().entry_type();
            if ty.is_pax_global_extensions() {
                // GitHub archives name their commit in the global header.
                if let Ok(Some(exts)) = e.pax_extensions() {
                    for x in exts.flatten() {
                        if x.key() == Ok("comment") {
                            // A commit id, or nothing (the text is shown later).
                            let c = x.value().unwrap_or("").trim();
                            if is_commit(c) {
                                commit = c.to_string();
                            }
                        }
                    }
                }
                continue;
            }
            // Plain files and folders only (no links), inside the stage.
            if !(ty.is_file() || ty.is_dir()) {
                continue;
            }
            // The source's rule folders (and licence / readme at the top).
            if !src.keep.is_empty() {
                let path = e.path().map_err(|e| e.to_string())?.into_owned();
                let inner: PathBuf = path.components().skip(1).collect();
                let top_file = inner.components().count() == 1 && ty.is_file();
                let wanted = inner.as_os_str().is_empty()
                    || top_file
                    || src
                        .keep
                        .iter()
                        .any(|k| inner.starts_with(k) || Path::new(k).starts_with(&inner));
                if !wanted {
                    continue;
                }
            }
            if !e
                .unpack_in(&stage)
                .map_err(|e| format!("cannot unpack: {e}"))?
            {
                return Err("the archive holds a path outside its folder".to_string());
            }
        }
        let top: Vec<PathBuf> = std::fs::read_dir(&stage)
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        let dir = match top.as_slice() {
            [one] => one.clone(),
            _ => return Err("the archive is not a single rules folder".into()),
        };
        let files = walkdir_count(&dir);
        if files == 0 {
            return Err("the archive holds no rule".into());
        }
        let now = jiff::Timestamp::now()
            .strftime("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let info = format!(
            "commit={commit}\ndownloaded={now}\nfiles={files}\nsource={}\nlicense={} ({})\n",
            src.home, src.licence, src.licence_url
        );
        std::fs::write(dir.join(INFO_FILE), info).map_err(|e| e.to_string())?;
        let dest = root.join(src.slug);
        let old = root.join(format!(".old-{nonce}"));
        if dest.exists() {
            std::fs::rename(&dest, &old)
                .map_err(|e| format!("cannot replace the previous rules: {e}"))?;
        }
        if let Err(e) = std::fs::rename(&dir, &dest) {
            // Put the previous set back.
            let _ = std::fs::rename(&old, &dest);
            return Err(format!("cannot install the rules: {e}"));
        }
        let _ = std::fs::remove_dir_all(&old);
        Ok(format!(
            "{} rule files{}",
            crate::app::fmt_n(files as u64),
            if commit.is_empty() {
                String::new()
            } else {
                format!(", commit {}", &commit[..commit.len().min(12)])
            }
        ))
    })();
    let _ = std::fs::remove_dir_all(&stage);
    res
}

/// Staging and old folders of earlier downloads left by a crash (older
/// than an hour: a download still running keeps its own).
fn sweep_leftovers(root: &Path) {
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|d| d.as_secs() > 3600);
        if (n.starts_with(".staging-") || n.starts_with(".old-")) && old {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

fn walkdir_count(dir: &Path) -> usize {
    let mut n = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| {
                    let n = n.to_string_lossy();
                    n != "config" && n != "scripts" && !n.starts_with('.')
                }) {
                    stack.push(p);
                }
            } else if p.extension().is_some_and(|x| x == "yml" || x == "yaml") {
                n += 1;
            }
        }
    }
    n
}

// ------------------------------------------------------------ CSV -> events

fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Columns that carry an event's `System` values and payload.
#[derive(Clone, Debug, Default)]
pub struct Cols {
    pub channel: Option<usize>,
    pub eid: Option<usize>,
    provider: Option<usize>,
    computer: Option<usize>,
    level: Option<usize>,
    keywords: Option<usize>,
    user: Option<usize>,
    pid: Option<usize>,
    tid: Option<usize>,
    rec: Option<usize>,
    /// forhunt / EvtxECmd JSON payload: the event's fields.
    pub payload: Option<usize>,
    /// Without a payload: (column, name) used as fields.
    fields: Vec<(usize, String)>,
}

impl Cols {
    pub fn detect(names: &[String]) -> Cols {
        let find = |alts: &[&str]| names.iter().position(|n| alts.contains(&norm(n).as_str()));
        let mut c = Cols {
            channel: find(&["channel"]),
            eid: find(&["eventid", "eventcode", "eid"]),
            provider: find(&["provider", "providername", "sourcename"]),
            computer: find(&["computer", "computername", "hostname"]),
            level: find(&["level"]),
            keywords: find(&["keywords"]),
            user: find(&["userid", "securityuserid"]),
            pid: find(&["processid", "executionprocessid"]),
            tid: find(&["threadid"]),
            rec: find(&["eventrecordid"]),
            payload: None,
            fields: Vec::new(),
        };
        // The payload holds every field of forhunt / EvtxECmd rows; their
        // other columns (UserName, PayloadData1...) are summaries.
        c.payload = names
            .iter()
            .position(|n| norm(n) == "payload")
            .filter(|_| c.channel.is_some());
        if c.payload.is_none() {
            let sys = [
                c.channel, c.eid, c.provider, c.computer, c.level, c.keywords, c.user, c.pid,
                c.tid, c.rec,
            ];
            c.fields = names
                .iter()
                .enumerate()
                .filter(|(i, _)| !sys.contains(&Some(*i)))
                .map(|(i, n)| (i, n.clone()))
                .collect();
        }
        c
    }
}

/// Windows level names (EvtxECmd / forhunt CSV) to the number rules test.
fn level_num(s: &str) -> u8 {
    match s.trim().to_ascii_lowercase().as_str() {
        "logalways" => 0,
        "critical" => 1,
        "error" => 2,
        "warning" => 3,
        "info" | "information" | "informational" => 4,
        "verbose" => 5,
        n => n.parse().unwrap_or(0),
    }
}

/// Flattens a JSON payload into fields: forhunt's flat object, or
/// EvtxECmd's `{"EventData":{"Data":[{"@Name":..,"#text":..}]}}`.
fn flatten(v: serde_json::Value, name: Option<&str>, out: &mut Vec<(String, Val<'static>)>) {
    use serde_json::Value as J;
    match v {
        J::Object(mut m) => {
            if let Some(J::String(n)) = m.remove("@Name") {
                let val = m.remove("#text").map_or(Val::Str("".into()), scalar);
                out.push((n, val));
                return;
            }
            for (k, x) in m {
                if k.starts_with('@') || k == "xmlns" {
                    continue;
                }
                // `#text` is the value of the element named by the parent.
                let key = if k == "#text" {
                    name.unwrap_or("Data").to_string()
                } else {
                    k
                };
                flatten(x, Some(&key), out);
            }
        }
        J::Array(a) => {
            let base = name.unwrap_or("Data").to_string();
            let named = a.iter().all(|x| x.get("@Name").is_some());
            for (i, x) in a.into_iter().enumerate() {
                if named {
                    flatten(x, None, out);
                } else {
                    flatten(x, Some(&format!("{base}[{}]", i + 1)), out);
                }
            }
        }
        x => out.push((name.unwrap_or("Data").to_string(), scalar(x))),
    }
}

/// A JSON scalar as an event value (strings moved, not copied).
fn scalar(v: serde_json::Value) -> Val<'static> {
    use serde_json::Value as J;
    match v {
        J::String(s) => Val::Str(s.into()),
        J::Number(n) => Val::Num(n.to_string().into()),
        J::Bool(b) => Val::Bool(b),
        J::Null => Val::Null,
        other => Val::Str(other.to_string().into()),
    }
}

/// The event a row holds (`cell(c)`: decoded text of column c).
pub fn row_event(cols: &Cols, cell: &dyn Fn(usize) -> Option<String>) -> EventView<'static> {
    let get = |c: Option<usize>| c.and_then(cell).unwrap_or_default();
    let num = |c: Option<usize>| get(c).trim().parse::<u64>().unwrap_or(0);
    let mut ev = EventView {
        channel: get(cols.channel).into(),
        event_id: num(cols.eid) as u32,
        provider: get(cols.provider).into(),
        computer: get(cols.computer).into(),
        level: cols.level.map_or(0, |_| level_num(&get(cols.level))),
        keywords: get(cols.keywords).into(),
        user_sid: get(cols.user).into(),
        process_id: num(cols.pid) as u32,
        thread_id: num(cols.tid) as u32,
        event_record_id: num(cols.rec),
        ..Default::default()
    };
    let mut fields: Vec<(String, Val<'static>)> = Vec::new();
    match cols.payload {
        Some(p) => {
            let text = get(Some(p));
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                flatten(v, None, &mut fields);
            }
        }
        None => {
            for (c, n) in &cols.fields {
                if let Some(v) = cell(*c) {
                    fields.push((n.clone(), Val::Str(v.into())));
                }
            }
        }
    }
    ev.fields = fields
        .into_iter()
        .map(|(n, v)| Field {
            name: n.into(),
            value: v,
        })
        .collect();
    ev
}

/// Rows some rule can match, from the dictionaries of the Channel and
/// EventId columns (None: every row must be read).
fn candidates(table: &fhv_engine::Table, cols: &Cols, set: &RuleSet) -> Option<RoaringBitmap> {
    let ci = table.column_index()?;
    let cd = ci.cols.get(cols.channel?)?.dict.as_ref()?;
    let ed = ci.cols.get(cols.eid?)?.dict.as_ref()?;
    // Per lowercased channel (None key: rules for any channel), the event
    // ids wanted (None: any id).
    let merge =
        |slot: &mut Option<Option<HashSet<u32>>>, eids: Option<&[u32]>| match (slot.as_mut(), eids)
        {
            (None, Some(e)) => *slot = Some(Some(e.iter().copied().collect())),
            (None, None) | (Some(_), None) => *slot = Some(None),
            (Some(Some(s)), Some(e)) => s.extend(e.iter().copied()),
            (Some(None), Some(_)) => {}
        };
    let mut any: Option<Option<HashSet<u32>>> = None;
    let mut by: HashMap<String, Option<Option<HashSet<u32>>>> = HashMap::new();
    for r in set.rules() {
        match r.channels() {
            None => merge(&mut any, r.event_ids()),
            Some(chs) => {
                for c in chs {
                    merge(by.entry(c.clone()).or_default(), r.event_ids());
                }
            }
        }
    }
    let eid_rows: HashMap<u32, &RoaringBitmap> = ed
        .values
        .iter()
        .zip(&ed.bitmaps)
        .filter_map(|(v, b)| Some((v.trim().parse::<u32>().ok()?, b)))
        .collect();
    let mut out = RoaringBitmap::new();
    for (v, rows) in cd.values.iter().zip(&cd.bitmaps) {
        let mine = by.get(&v.trim().to_lowercase()).cloned().flatten();
        let want: Option<HashSet<u32>> = match (&any, &mine) {
            (Some(None), _) | (_, Some(None)) => None,
            (a, m) => Some(
                a.iter()
                    .chain(m.iter())
                    .flatten()
                    .flatten()
                    .copied()
                    .collect(),
            ),
        };
        match want {
            None => out |= rows,
            Some(ids) if ids.is_empty() => {}
            Some(ids) => {
                let mut u = RoaringBitmap::new();
                for e in ids {
                    if let Some(b) = eid_rows.get(&e) {
                        u |= *b;
                    }
                }
                u &= rows;
                out |= u;
            }
        }
    }
    Some(out)
}

// ------------------------------------------------------------------ results

/// Detections of one run over a tab.
pub struct Detections {
    pub set: Arc<RuleSet>,
    /// Library generation the rules came from.
    pub generation: u64,
    /// (rule, rows), most severe first, then most rows.
    pub hits: Vec<(u32, RoaringBitmap)>,
    /// Correlation rule -> number of bursts (detections) it found.
    pub bursts: HashMap<u32, u32>,
    /// Rows by their most severe level (index 1..=5).
    pub level: [RoaringBitmap; 6],
    pub all: RoaringBitmap,
    /// Rows read (candidates), and how long it took.
    pub read: u64,
    pub ms: f64,
    pub note: Option<String>,
}

impl Detections {
    pub fn row_level(&self, row: u32) -> u8 {
        (1..=5u8)
            .rev()
            .find(|&l| self.level[l as usize].contains(row))
            .unwrap_or(0)
    }

    /// Rules matching a row, most severe first.
    pub fn row_rules(&self, row: u32) -> Vec<u32> {
        self.hits
            .iter()
            .filter(|(_, b)| b.contains(row))
            .map(|(r, _)| *r)
            .collect()
    }

    pub fn count_at_least(&self, level: u8) -> u64 {
        (level.max(1)..=5)
            .map(|l| self.level[l as usize].len())
            .sum()
    }
}

/// What the detections filter keeps.
#[derive(Clone, Debug, PartialEq)]
pub enum Focus {
    /// Every detected row at or above a level (1: all).
    AtLeast(u8),
    Rule(u32),
}

struct Job {
    rx: Receiver<Result<Detections, String>>,
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
    started: Instant,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Per prepared grid row: level, and file columns showing a tested field.
#[derive(Default)]
pub struct Paint {
    first: u64,
    levels: Vec<u8>,
    cols: Vec<Vec<usize>>,
    titles: Vec<String>,
}

#[derive(Default)]
pub struct TabSigma {
    pub result: Option<Arc<Detections>>,
    job: Option<Job>,
    pub error: Option<String>,
    /// Detections panel shown.
    pub panel: bool,
    pub focus: Option<Focus>,
    /// The filter is inverted: rows without these detections.
    pub focus_not: bool,
    focus_rows: Option<(Focus, Arc<RoaringBitmap>)>,
    /// Rule list filter of the panel.
    pub search: String,
    pub paint: Paint,
}

impl TabSigma {
    pub fn running(&self) -> bool {
        self.job.is_some()
    }

    /// Sets what the detections filter keeps (None: no filter).
    pub fn set_focus(&mut self, f: Option<Focus>) {
        self.focus_rows = None;
        self.focus_not = false;
        self.focus = f;
        let (Some(f), Some(res)) = (&self.focus, &self.result) else {
            self.focus = None;
            return;
        };
        let rows = match f {
            Focus::AtLeast(l) => {
                let mut b = RoaringBitmap::new();
                for x in (*l).max(1)..=5 {
                    b |= &res.level[x as usize];
                }
                b
            }
            Focus::Rule(r) => res
                .hits
                .iter()
                .find(|(x, _)| x == r)
                .map(|(_, b)| b.clone())
                .unwrap_or_default(),
        };
        self.focus_rows = Some((f.clone(), Arc::new(rows)));
    }

    /// Rows the filter keeps (a term of the tab's query).
    pub fn filter_rows(&self) -> Option<Arc<RoaringBitmap>> {
        let f = self.focus.as_ref()?;
        match &self.focus_rows {
            Some((k, rows)) if k == f => Some(rows.clone()),
            _ => None,
        }
    }

    pub fn focus_label(&self) -> Option<String> {
        let f = self.focus.as_ref()?;
        let res = self.result.as_ref()?;
        Some(match f {
            Focus::AtLeast(1) => "Sigma: all detections".into(),
            Focus::AtLeast(l) if *l >= 5 => "Sigma: critical".into(),
            Focus::AtLeast(l) => format!("Sigma: {} and above", LEVELS[*l as usize]),
            Focus::Rule(r) => format!("Sigma: {}", res.set.rules()[*r as usize].title),
        })
    }

    /// Progress of a run: (text, fraction).
    pub fn progress(&self) -> Option<(String, f32)> {
        let j = self.job.as_ref()?;
        let total = j.total.load(Ordering::Relaxed);
        let done = j.done.load(Ordering::Relaxed);
        let frac = if total == 0 {
            0.0
        } else {
            (done as f64 / total as f64).min(1.0) as f32
        };
        Some((
            format!(
                "Sigma detection: {} of {} rows ({:.1} s, Esc cancels)",
                crate::app::fmt_n(done),
                crate::app::fmt_n(total),
                j.started.elapsed().as_secs_f64()
            ),
            frac,
        ))
    }

    pub fn cancel(&mut self) {
        self.job = None;
    }

    /// The results came from older rules (settings changed since).
    pub fn stale(&self, generation: u64) -> bool {
        self.result
            .as_ref()
            .is_some_and(|r| r.generation != generation)
    }
}

/// Starts a run over the whole file (a running one is replaced).
pub fn start(t: &mut Tab, set: Arc<RuleSet>, generation: u64) {
    if !t.ready() {
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicU64::new(0));
    let total = Arc::new(AtomicU64::new(0));
    let (table, c, d, n) = (t.table.clone(), cancel.clone(), done.clone(), total.clone());
    let _ = std::thread::Builder::new()
        .name("fhv-sigma".into())
        .spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(&table, set, generation, &c, &d, &n)
            }))
            .unwrap_or_else(|_| Err("internal error (panic)".into()));
            let _ = tx.send(r);
        });
    t.sigma.error = None;
    t.sigma.job = Some(Job {
        rx,
        cancel,
        done,
        total,
        started: Instant::now(),
    });
}

/// Collects a finished run. Returns true while one runs.
pub fn poll(t: &mut Tab) -> bool {
    let Some(j) = &t.sigma.job else {
        return false;
    };
    match j.rx.try_recv() {
        Ok(r) => {
            t.sigma.job = None;
            match r {
                Ok(d) => {
                    // A re-run keeps the filter: a level as it is, a rule
                    // by its id (indexes change when the rules reload).
                    let focus = match (t.sigma.focus.clone(), &t.sigma.result) {
                        (Some(Focus::Rule(r)), Some(old)) => {
                            let id = old.set.rules().get(r as usize).map(|x| x.id.clone());
                            id.and_then(|id| d.set.rules().iter().position(|x| x.id == id))
                                .map(|k| Focus::Rule(k as u32))
                        }
                        (f, _) => f,
                    };
                    let first = t.sigma.result.is_none();
                    let not = t.sigma.focus_not;
                    t.sigma.result = Some(Arc::new(d));
                    // The panel opens on the first run, not again once closed.
                    t.sigma.panel |= first;
                    if t.sigma.focus.is_some() {
                        t.sigma.set_focus(focus);
                        t.sigma.focus_not = not && t.sigma.focus.is_some();
                        t.requery();
                    }
                    t.repaint_cells();
                }
                Err(e) if e == "cancelled" => {}
                Err(e) => t.sigma.error = Some(e),
            }
            false
        }
        Err(TryRecvError::Disconnected) => {
            t.sigma.job = None;
            t.sigma.error = Some("detection stopped unexpectedly".into());
            false
        }
        Err(TryRecvError::Empty) => true,
    }
}

fn run(
    table: &fhv_engine::Table,
    set: Arc<RuleSet>,
    generation: u64,
    cancel: &AtomicBool,
    done: &AtomicU64,
    total: &AtomicU64,
) -> Result<Detections, String> {
    let started = Instant::now();
    let cols = Cols::detect(&table.columns);
    let cand = candidates(table, &cols, &set);
    let n = table.row_count();
    total.store(cand.as_ref().map_or(n, |c| c.len()), Ordering::Relaxed);
    let blocks: Vec<Range<u64>> = match &cand {
        Some(c) => table.par_blocks_of(c, 4096),
        None => (0..n).step_by(4096).map(|s| s..(s + 4096).min(n)).collect(),
    };
    let quote = table.dialect.quote;
    let tables = table.tables();
    // Event times for correlations: the time column's parsed ticks (the
    // same 100 ns unit as forhunt).
    let ci = table.column_index();
    let ticks: Option<&[i64]> = ci
        .as_ref()
        .and_then(|ci| ci.primary_time.and_then(|c| ci.cols.get(c)))
        .and_then(|c| c.time.as_ref())
        .map(|t| t.ticks.as_slice());
    // Without event times every match would be "at the same instant":
    // correlations need the time column.
    let correlate = set.has_correlations() && ticks.is_some();
    type Part = (
        Vec<(u32, Vec<u32>)>,
        Vec<(forhunt_core::sigma::corr::Obs, u32)>,
    );
    let map = |b: &fhv_engine::table::Block<'_>| -> Part {
        let mut out = Vec::new();
        let mut obs = Vec::new();
        if cancel.load(Ordering::Relaxed) {
            return (out, obs);
        }
        let mut m = set.matcher();
        let mut spans = Vec::new();
        let mut hits = Vec::new();
        let mut read = 0u64;
        for k in 0..b.len() {
            let row = b.first + k as u64;
            if cand.as_ref().is_some_and(|c| !c.contains(row as u32)) {
                continue;
            }
            read += 1;
            let raw = b.row(k);
            spans.clear();
            fhv_engine::scan::split_row(tables, raw, &mut spans);
            let cell = |c: usize| -> Option<String> {
                let s = spans.get(c)?;
                let mut tmp = Vec::new();
                Some(fhv_engine::table::decode_field(raw, s, quote, &mut tmp).into_owned())
            };
            let mut ev = row_event(&cols, &cell);
            m.matches(&ev, &mut hits);
            if !hits.is_empty() {
                // Rows with a blank or unreadable time take no part.
                if let Some(&t) = ticks
                    .and_then(|t| t.get(row as usize))
                    .filter(|&&t| correlate && t != i64::MIN)
                {
                    ev.ts = forhunt_core::Ts(t);
                    let mut o = Vec::new();
                    set.observe(&ev, &hits, &mut o);
                    obs.extend(o.into_iter().map(|x| (x, row as u32)));
                }
                out.push((row as u32, std::mem::take(&mut hits)));
            }
        }
        done.fetch_add(read, Ordering::Relaxed);
        (out, obs)
    };
    let mut per_rule: HashMap<u32, RoaringBitmap> = HashMap::new();
    let mut level: [RoaringBitmap; 6] = Default::default();
    let mut all = RoaringBitmap::new();
    let rules = set.rules();
    let mut row_top: HashMap<u32, u8> = HashMap::new();
    let mut obs = Vec::new();
    table
        .par_block_list_until(
            &blocks,
            &|| cancel.load(Ordering::Relaxed),
            map,
            |_, (v, o)| {
                if cancel.load(Ordering::Relaxed) {
                    return Err(fhv_engine::Error::Cancelled);
                }
                obs.extend(o);
                for (row, rs) in v {
                    // Rules used only by correlations are not shown.
                    for r in rs.into_iter().filter(|&r| !rules[r as usize].hidden) {
                        let t = row_top.entry(row).or_default();
                        *t = (*t).max(rules[r as usize].level);
                        per_rule.entry(r).or_default().insert(row);
                    }
                }
                Ok(())
            },
        )
        .map_err(|e| e.to_string())?;
    // Correlations: every member row of a burst is a row of the rule.
    let mut bursts: HashMap<u32, u32> = HashMap::new();
    for b in set.correlate(obs) {
        *bursts.entry(b.rule).or_default() += 1;
        let lvl = rules[b.rule as usize].level;
        let rows = per_rule.entry(b.rule).or_default();
        for m in b.members {
            rows.insert(m);
            let t = row_top.entry(m).or_default();
            *t = (*t).max(lvl);
        }
    }
    for (row, top) in row_top {
        level[top.min(5) as usize].insert(row);
        all.insert(row);
    }
    let mut hits: Vec<(u32, RoaringBitmap)> = per_rule.into_iter().collect();
    hits.sort_by(|a, b| {
        let (ra, rb) = (&rules[a.0 as usize], &rules[b.0 as usize]);
        rb.level
            .cmp(&ra.level)
            .then(b.1.len().cmp(&a.1.len()))
            .then_with(|| ra.title.cmp(&rb.title))
            .then(a.0.cmp(&b.0))
    });
    // System values a CSV layout does not keep (forhunt and EvtxECmd have
    // no Version, Task or Opcode column): rules testing them cannot match.
    let lacking: Vec<&str> = ["Version", "Task", "Opcode"]
        .into_iter()
        .filter(|f| !table.columns.iter().any(|c| c.eq_ignore_ascii_case(f)))
        .collect();
    let blind = rules
        .iter()
        .filter(|r| !r.hidden && r.fields.iter().any(|f| lacking.contains(&f.as_str())))
        .count();
    let mut notes: Vec<String> = Vec::new();
    if cols.channel.is_none() || cols.eid.is_none() {
        notes.push("No Channel / EventId column: rules that name a channel or event id cannot match this file.".into());
    } else if cols.payload.is_none() {
        notes.push("No Payload column: the columns are taken as the event's fields.".into());
    }
    if blind > 0 {
        notes.push(format!(
            "{blind} rules also test {} of the event, which this file does not hold: they cannot match here (`forhunt sigma` on the logs can).",
            lacking.join(" / ")
        ));
    }
    if set.has_correlations() && ticks.is_none() {
        notes.push(
            "No time column was recognised: correlation rules (counts over time) were not run."
                .into(),
        );
    }
    let note = (!notes.is_empty()).then(|| notes.join("\n"));
    Ok(Detections {
        set,
        generation,
        bursts,
        hits,
        level,
        all,
        read: done.load(Ordering::Relaxed),
        ms: started.elapsed().as_secs_f64() * 1e3,
        note,
    })
}

// ------------------------------------------------------------------- paint

/// Levels, tested-field columns and tooltips of the prepared rows
/// (`ids[k]`: data row of grid slot k; cells in `t.rows` slots `first..`).
pub fn paint(t: &mut Tab, first: u64, ids: &[u32]) {
    let Some(res) = t.sigma.result.clone() else {
        t.sigma.paint = Paint::default();
        return;
    };
    let names: Vec<String> = t.table.columns.iter().map(|n| norm(n)).collect();
    let payload = names.iter().position(|n| n == "payload");
    let pdata: Vec<usize> = (0..names.len())
        .filter(|&c| names[c].starts_with("payloaddata"))
        .collect();
    let p = &mut t.sigma.paint;
    p.first = first;
    p.levels.clear();
    p.cols.clear();
    p.titles.clear();
    for (k, &row) in ids.iter().enumerate() {
        let lvl = res.row_level(row);
        p.levels.push(lvl);
        let mut cols = Vec::new();
        let mut titles = String::new();
        if lvl > 0 {
            let rs = res.row_rules(row);
            let mut fields: Vec<&str> = Vec::new();
            for &r in &rs {
                let rule = &res.set.rules()[r as usize];
                fields.extend(rule.fields.iter().map(String::as_str));
                if !titles.is_empty() {
                    titles.push('\n');
                }
                titles.push_str(&format!(
                    "Sigma ({}): {}",
                    LEVELS[rule.level as usize], rule.title
                ));
            }
            for f in fields {
                let nf = norm(f);
                if let Some(c) = names.iter().position(|n| *n == nf) {
                    cols.push(c);
                    continue;
                }
                // forhunt / EvtxECmd layout: the field is in the payload,
                // and maybe summarised in a PayloadDataN cell.
                let pos = first + k as u64;
                for &c in &pdata {
                    let v = t.rows.cell(pos, c);
                    if v.split(':').next().is_some_and(|n| norm(n) == nf) {
                        cols.push(c);
                    }
                }
                if let Some(c) = payload
                    && t.rows.cell(pos, c).contains(&format!("\"{f}\""))
                {
                    cols.push(c);
                }
            }
            cols.sort_unstable();
            cols.dedup();
        }
        p.cols.push(cols);
        p.titles.push(titles);
    }
}

impl Paint {
    fn slot(&self, k: u64) -> Option<usize> {
        let i = k.checked_sub(self.first)? as usize;
        (i < self.levels.len()).then_some(i)
    }

    /// Level of the prepared slot `k` (0: none).
    pub fn level(&self, k: u64) -> u8 {
        self.slot(k).map_or(0, |i| self.levels[i])
    }

    /// File column `c` of slot `k` shows a field a matching rule tests.
    pub fn tested(&self, k: u64, c: usize) -> bool {
        self.slot(k).is_some_and(|i| self.cols[i].contains(&c))
    }

    pub fn titles(&self, k: u64) -> Option<&str> {
        self.slot(k)
            .map(|i| self.titles[i].as_str())
            .filter(|s| !s.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_of_known_layouts() {
        let names = |s: &str| s.split(',').map(String::from).collect::<Vec<_>>();
        let c = Cols::detect(&names(
            "RecordNumber,EventRecordId,TimeCreated,EventId,Level,Provider,Channel,ProcessId,ThreadId,Computer,UserId,MapDescription,UserName,RemoteHost,PayloadData1,PayloadData2,PayloadData3,PayloadData4,PayloadData5,PayloadData6,ExecutableInfo,HiddenRecord,SourceFile,Keywords,Payload,ForhuntId",
        ));
        assert_eq!(
            (c.channel, c.eid, c.payload, c.rec),
            (Some(6), Some(3), Some(24), Some(1))
        );
        assert!(c.fields.is_empty());
        // Any CSV: columns are fields.
        let c = Cols::detect(&names("Time,Channel,Event ID,CommandLine,Image"));
        assert_eq!((c.channel, c.eid, c.payload), (Some(1), Some(2), None));
        assert_eq!(
            c.fields.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>(),
            ["Time", "CommandLine", "Image"]
        );
    }

    #[test]
    fn payloads_flatten() {
        let mut out = Vec::new();
        flatten(
            serde_json::from_str(r#"{"CommandLine":"a b","LogonType":3,"X":null}"#).unwrap(),
            None,
            &mut out,
        );
        assert_eq!(out[0], ("CommandLine".into(), Val::Str("a b".into())));
        assert_eq!(out[1], ("LogonType".into(), Val::Num("3".into())));
        let mut out = Vec::new();
        let evtxecmd = r##"{"EventData":{"Data":[{"@Name":"SubjectUserName","#text":"bob"},{"@Name":"Empty"}],"Binary":""}}"##;
        flatten(serde_json::from_str(evtxecmd).unwrap(), None, &mut out);
        assert_eq!(out[0], ("SubjectUserName".into(), Val::Str("bob".into())));
        assert_eq!(out[1].0, "Empty");
        let mut out = Vec::new();
        flatten(
            serde_json::from_str(r#"{"EventData":{"Data":["a","b"]}}"#).unwrap(),
            None,
            &mut out,
        );
        assert_eq!(
            out.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(),
            ["Data[1]", "Data[2]"]
        );
    }

    #[test]
    fn installs_a_rules_archive() {
        // A GitHub-style archive: pax global header with the commit, one
        // top folder holding config/ and sigma/.
        let mut tarb = tar::Builder::new(Vec::new());
        let mut add = |path: &str, data: &[u8]| {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            tarb.append_data(&mut h, path, data).unwrap();
        };
        add("hayabusa-rules-abc/config/exclude_rules.txt", b"");
        // Not a rule folder of the source: left out.
        add(
            "hayabusa-rules-abc/scripts/tests/x.yml",
            b"title: not a rule\n",
        );
        add("hayabusa-rules-abc/LICENSE.md", b"DRL");
        add(
            "hayabusa-rules-abc/sigma/r.yml",
            b"title: R\nid: r\nlevel: high\ndetection:\n  a:\n    A: x\n  condition: a\n",
        );
        let tar_bytes = tarb.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tar_bytes).unwrap();
        let gz = gz.finish().unwrap();
        let root = tempfile::tempdir().unwrap();
        let src = source("hayabusa-rules").unwrap();
        let msg = install_tar_gz(root.path(), &gz[..], src).unwrap();
        assert!(msg.starts_with("1 rule files"), "{msg}");
        assert_eq!(installed(root.path(), "hayabusa-rules").unwrap().files, 1);
        let dir = root.path().join("hayabusa-rules");
        assert!(!dir.join("scripts").exists() && dir.join("LICENSE.md").is_file());
        // Again: replaced in place.
        install_tar_gz(root.path(), &gz[..], src).unwrap();
        let (set, _) =
            forhunt_core::sigma::load(&[root.path().join("hayabusa-rules")], &Default::default());
        assert_eq!(set.len(), 1);
        // Not a rules archive: refused, the installed set stays.
        assert!(install_tar_gz(root.path(), &b"not gzip"[..], src).is_err());
        assert!(
            root.path()
                .join("hayabusa-rules")
                .join("sigma/r.yml")
                .is_file()
        );
        assert_eq!(
            std::fs::read_dir(root.path()).unwrap().count(),
            1,
            "no staging left"
        );
    }

    /// A tar.gz of raw entries: (type, path, data). Paths are written as
    /// bytes, so `..` and links get through to the unpacker.
    fn raw_tar_gz(entries: &[(tar::EntryType, &str, &[u8])]) -> Vec<u8> {
        let mut tarb = tar::Builder::new(Vec::new());
        for (ty, path, data) in entries {
            let mut h = tar::Header::new_ustar();
            h.set_entry_type(*ty);
            h.as_ustar_mut().unwrap().name[..path.len()].copy_from_slice(path.as_bytes());
            if *ty == tar::EntryType::Symlink {
                h.set_link_name("/etc/passwd").unwrap();
                h.set_size(0);
            } else {
                h.set_size(data.len() as u64);
            }
            h.set_mode(0o644);
            h.set_cksum();
            tarb.append(&h, *data).unwrap();
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tarb.into_inner().unwrap()).unwrap();
        gz.finish().unwrap()
    }

    const RULE: &[u8] =
        b"title: R\nid: r\nlevel: high\ndetection:\n  a:\n    A: x\n  condition: a\n";

    #[test]
    fn hostile_archives() {
        use tar::EntryType as T;
        let src = source("hayabusa-rules").unwrap();
        // A path climbing out of the folder: refused, nothing installed.
        let root = tempfile::tempdir().unwrap();
        let gz = raw_tar_gz(&[
            (T::Regular, "x/sigma/r.yml", RULE),
            (T::Regular, "x/sigma/../../../escape.yml", RULE),
        ]);
        assert!(install_tar_gz(root.path(), &gz[..], src).is_err());
        assert!(!root.path().join("hayabusa-rules").exists());
        assert!(!root.path().parent().unwrap().join("escape.yml").exists());
        assert_eq!(
            std::fs::read_dir(root.path()).unwrap().count(),
            0,
            "no staging left"
        );
        // Links are skipped, the rest installs.
        let gz = raw_tar_gz(&[
            (T::Regular, "x/sigma/r.yml", RULE),
            (T::Symlink, "x/sigma/link.yml", b""),
        ]);
        install_tar_gz(root.path(), &gz[..], src).unwrap();
        let dir = root.path().join("hayabusa-rules/sigma");
        assert!(dir.join("r.yml").is_file());
        assert!(std::fs::symlink_metadata(dir.join("link.yml")).is_err());
        // A comment that is not a commit id is not recorded.
        let gz = raw_tar_gz(&[
            (
                T::XGlobalHeader,
                "pax_global_header",
                b"33 comment=<b>not a commit</b>\n",
            ),
            (T::Regular, "x/sigma/r.yml", RULE),
        ]);
        install_tar_gz(root.path(), &gz[..], src).unwrap();
        assert_eq!(installed(root.path(), "hayabusa-rules").unwrap().commit, "");
        std::fs::write(
            root.path().join("hayabusa-rules").join(INFO_FILE),
            "commit=abc\nfiles=1\n",
        )
        .unwrap();
        assert_eq!(installed(root.path(), "hayabusa-rules").unwrap().commit, "");
    }

    #[test]
    fn levels() {
        assert_eq!(level_num("Info"), 4);
        assert_eq!(level_num("Warning"), 3);
        assert_eq!(level_num("2"), 2);
    }
}
