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

/// Where the Yamato Security rule set is downloaded from: the newest rules
/// (Settings > *Download* / *Update*), over HTTPS.
pub const RULES_URL: &str =
    "https://github.com/Yamato-Security/hayabusa-rules/archive/refs/heads/main.tar.gz";
pub const RULES_HOME: &str = "https://github.com/Yamato-Security/hayabusa-rules";
pub const LICENSE_URL: &str = "https://github.com/SigmaHQ/Detection-Rule-License";
/// Folder of the downloaded rule set under the rules root.
pub const HAYABUSA_DIR: &str = "hayabusa-rules";
/// What was downloaded, written beside the rules.
const INFO_FILE: &str = ".forhunt-viewer-rules";
/// Largest download accepted (the rule set is ~10 MB compressed).
const MAX_DOWNLOAD: u64 = 256 << 20;

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
    /// Use the downloaded Yamato Security rules (when downloaded).
    pub hayabusa: bool,
    /// Folders (or files) of your own rules.
    pub folders: Vec<PathBuf>,
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
            hayabusa: true,
            folders: Vec::new(),
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

pub fn installed(root: &Path) -> Option<Installed> {
    let dir = root.join(HAYABUSA_DIR);
    let text = std::fs::read_to_string(dir.join(INFO_FILE)).ok()?;
    let get = |k: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(k).and_then(|v| v.strip_prefix('=')))
            .unwrap_or("")
            .to_string()
    };
    Some(Installed {
        commit: get("commit"),
        downloaded: get("downloaded"),
        files: get("files").parse().unwrap_or(0),
    })
}

// ------------------------------------------------------------------ library

#[derive(Clone, Debug, PartialEq)]
struct LoadKey {
    paths: Vec<PathBuf>,
    min_level: u8,
    noisy: bool,
}

pub struct Download {
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

/// The compiled rules every tab runs (one load, shared), and the download.
pub struct Library {
    pub root: PathBuf,
    pub set: Option<Arc<RuleSet>>,
    pub report: Option<LoadReport>,
    /// Bumped at each (re)load: results of older generations are stale.
    pub generation: u64,
    loaded_for: Option<LoadKey>,
    loading: Option<(LoadKey, Receiver<(RuleSet, LoadReport)>)>,
    pub download: Option<Download>,
    /// Outcome of the last download, for Settings.
    pub message: Option<Result<String, String>>,
    pub installed: Option<Installed>,
}

impl Library {
    pub fn new(root: PathBuf) -> Library {
        let installed = installed(&root);
        Library {
            root,
            set: None,
            report: None,
            generation: 0,
            loaded_for: None,
            loading: None,
            download: None,
            message: None,
            installed,
        }
    }

    /// Rule paths the settings select (the downloaded set only if present).
    pub fn paths(&self, s: &SigmaSettings) -> Vec<PathBuf> {
        let mut v = Vec::new();
        let hr = self.root.join(HAYABUSA_DIR);
        if s.hayabusa && hr.is_dir() {
            v.push(hr);
        }
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
                    self.download = None;
                    if r.is_ok() {
                        self.installed = installed(&self.root);
                        self.loaded_for = None;
                    }
                    self.message = Some(r);
                }
                Err(TryRecvError::Disconnected) => {
                    self.download = None;
                    self.message = Some(Err("the download stopped unexpectedly".into()));
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some((key, rx)) = &self.loading {
            match rx.try_recv() {
                Ok((set, rep)) => {
                    self.loaded_for = Some(key.clone());
                    self.set = Some(Arc::new(set));
                    self.report = Some(rep);
                    self.generation += 1;
                    self.loading = None;
                }
                Err(TryRecvError::Disconnected) => {
                    self.loaded_for = Some(key.clone());
                    self.loading = None;
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        let key = LoadKey {
            paths: self.paths(s),
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

    /// Downloads the newest Yamato Security rules in the background.
    pub fn start_download(&mut self) {
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
                    download(&root, RULES_URL, &b, &c)
                }))
                .unwrap_or_else(|_| Err("internal error (panic)".into()));
                let _ = tx.send(r);
            });
        self.message = None;
        self.download = Some(Download {
            rx,
            bytes,
            cancel,
            started: Instant::now(),
        });
    }

    pub fn cancel_download(&mut self) {
        if self.download.take().is_some() {
            self.message = Some(Err("download cancelled".into()));
        }
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

/// Downloads `url` (a GitHub tar.gz of hayabusa-rules) and installs it as
/// `<root>/hayabusa-rules`, replacing the previous set only once the new
/// one is complete and looks like a rule set.
pub fn download(
    root: &Path,
    url: &str,
    bytes: &Arc<AtomicU64>,
    cancel: &Arc<AtomicBool>,
) -> Result<String, String> {
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
    install_tar_gz(root, r, url)
}

/// Unpacks a hayabusa-rules tar.gz into `<root>/hayabusa-rules` (staged,
/// then renamed into place).
pub fn install_tar_gz(root: &Path, r: impl Read, source: &str) -> Result<String, String> {
    let stage = root.join(format!(".staging-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage).map_err(|e| e.to_string())?;
    let res = (|| {
        let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(r));
        let mut commit = String::new();
        for e in ar
            .entries()
            .map_err(|e| format!("not a tar.gz archive: {e}"))?
        {
            let mut e = e.map_err(|e| format!("damaged archive: {e}"))?;
            let ty = e.header().entry_type();
            if ty.is_pax_global_extensions() {
                // GitHub archives name their commit in the global header.
                if let Ok(Some(exts)) = e.pax_extensions() {
                    for x in exts.flatten() {
                        if x.key() == Ok("comment") {
                            commit = x.value().unwrap_or("").trim().to_string();
                        }
                    }
                }
                continue;
            }
            // Plain files and folders only (no links), inside the stage.
            if !(ty.is_file() || ty.is_dir()) {
                continue;
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
        let src = match top.as_slice() {
            [one] => one.clone(),
            _ => return Err("the archive is not a single rules folder".into()),
        };
        if !src.join("config").is_dir()
            || !(src.join("sigma").is_dir() || src.join("hayabusa").is_dir())
        {
            return Err("the archive is not a hayabusa-rules tree (no config/ and sigma/)".into());
        }
        let files = walkdir_count(&src);
        if files == 0 {
            return Err("the archive holds no rule".into());
        }
        let now = jiff::Timestamp::now()
            .strftime("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let info = format!(
            "commit={commit}\ndownloaded={now}\nfiles={files}\nsource={source}\nlicense=Detection Rule License 1.1 ({LICENSE_URL})\n"
        );
        std::fs::write(src.join(INFO_FILE), info).map_err(|e| e.to_string())?;
        let dest = root.join(HAYABUSA_DIR);
        let old = root.join(format!(".old-{}", std::process::id()));
        if dest.exists() {
            std::fs::rename(&dest, &old)
                .map_err(|e| format!("cannot replace the previous rules: {e}"))?;
        }
        if let Err(e) = std::fs::rename(&src, &dest) {
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
                if p.file_name()
                    .is_some_and(|n| n != "config" && n != "scripts")
                {
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
fn flatten(v: &serde_json::Value, name: Option<&str>, out: &mut Vec<(String, Val<'static>)>) {
    use serde_json::Value as J;
    match v {
        J::Object(m) => {
            if let Some(n) = m.get("@Name").and_then(J::as_str) {
                let val = m.get("#text").map(scalar).unwrap_or(Val::Str("".into()));
                out.push((n.to_string(), val));
                return;
            }
            for (k, x) in m {
                if k.starts_with('@') || k == "xmlns" {
                    continue;
                }
                let key = if k == "#text" {
                    name.unwrap_or("Data")
                } else {
                    k.as_str()
                };
                match x {
                    // Wrappers: EventData, UserData and its element.
                    J::Object(_)
                        if matches!(k.as_str(), "EventData" | "UserData")
                            || name == Some("UserData") =>
                    {
                        flatten(x, Some(k), out)
                    }
                    _ => flatten(x, Some(key), out),
                }
            }
        }
        J::Array(a) => {
            let base = name.unwrap_or("Data");
            let named = a.iter().all(|x| x.get("@Name").is_some());
            for (i, x) in a.iter().enumerate() {
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

fn scalar(v: &serde_json::Value) -> Val<'static> {
    use serde_json::Value as J;
    match v {
        J::String(s) => Val::Str(s.clone().into()),
        J::Number(n) => Val::Num(n.to_string().into()),
        J::Bool(b) => Val::Bool(*b),
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
                flatten(&v, None, &mut fields);
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
                    t.sigma.result = Some(Arc::new(d));
                    t.sigma.panel = true;
                    // A rule filter of the previous run may name another rule.
                    if t.sigma.focus.is_some() {
                        t.sigma.set_focus(None);
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
    let map = |b: &fhv_engine::table::Block<'_>| -> Vec<(u32, Vec<u32>)> {
        let mut out = Vec::new();
        if cancel.load(Ordering::Relaxed) {
            return out;
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
            let ev = row_event(&cols, &cell);
            m.matches(&ev, &mut hits);
            if !hits.is_empty() {
                out.push((row as u32, std::mem::take(&mut hits)));
            }
        }
        done.fetch_add(read, Ordering::Relaxed);
        out
    };
    let mut per_rule: HashMap<u32, RoaringBitmap> = HashMap::new();
    let mut level: [RoaringBitmap; 6] = Default::default();
    let mut all = RoaringBitmap::new();
    let rules = set.rules();
    table
        .par_block_list(&blocks, map, |_, v| {
            if cancel.load(Ordering::Relaxed) {
                return Err(fhv_engine::Error::Cancelled);
            }
            for (row, rs) in v {
                let mut top = 0;
                for r in rs {
                    top = top.max(rules[r as usize].level);
                    per_rule.entry(r).or_default().insert(row);
                }
                level[top.min(5) as usize].insert(row);
                all.insert(row);
            }
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    let mut hits: Vec<(u32, RoaringBitmap)> = per_rule.into_iter().collect();
    hits.sort_by(|a, b| {
        let (ra, rb) = (&rules[a.0 as usize], &rules[b.0 as usize]);
        rb.level
            .cmp(&ra.level)
            .then(b.1.len().cmp(&a.1.len()))
            .then_with(|| ra.title.cmp(&rb.title))
            .then(a.0.cmp(&b.0))
    });
    let note = if cols.channel.is_none() || cols.eid.is_none() {
        Some("No Channel / EventId column: rules that name a channel or event id cannot match this file.".into())
    } else if cols.payload.is_none() {
        Some("No Payload column: the columns are taken as the event's fields.".into())
    } else {
        None
    };
    Ok(Detections {
        set,
        generation,
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
            &serde_json::from_str(r#"{"CommandLine":"a b","LogonType":3,"X":null}"#).unwrap(),
            None,
            &mut out,
        );
        assert_eq!(out[0], ("CommandLine".into(), Val::Str("a b".into())));
        assert_eq!(out[1], ("LogonType".into(), Val::Num("3".into())));
        let mut out = Vec::new();
        let evtxecmd = r##"{"EventData":{"Data":[{"@Name":"SubjectUserName","#text":"bob"},{"@Name":"Empty"}],"Binary":""}}"##;
        flatten(&serde_json::from_str(evtxecmd).unwrap(), None, &mut out);
        assert_eq!(out[0], ("SubjectUserName".into(), Val::Str("bob".into())));
        assert_eq!(out[1].0, "Empty");
        let mut out = Vec::new();
        flatten(
            &serde_json::from_str(r#"{"EventData":{"Data":["a","b"]}}"#).unwrap(),
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
        add(
            "hayabusa-rules-abc/sigma/r.yml",
            b"title: R\nid: r\nlevel: high\ndetection:\n  a:\n    A: x\n  condition: a\n",
        );
        let tar_bytes = tarb.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tar_bytes).unwrap();
        let gz = gz.finish().unwrap();
        let root = tempfile::tempdir().unwrap();
        let msg = install_tar_gz(root.path(), &gz[..], "test").unwrap();
        assert!(msg.starts_with("1 rule files"), "{msg}");
        assert_eq!(installed(root.path()).unwrap().files, 1);
        // Again: replaced in place.
        install_tar_gz(root.path(), &gz[..], "test").unwrap();
        let (set, _) =
            forhunt_core::sigma::load(&[root.path().join(HAYABUSA_DIR)], &Default::default());
        assert_eq!(set.len(), 1);
        // Not a rules archive: refused, the installed set stays.
        assert!(install_tar_gz(root.path(), &b"not gzip"[..], "test").is_err());
        assert!(root.path().join(HAYABUSA_DIR).join("sigma/r.yml").is_file());
        assert_eq!(
            std::fs::read_dir(root.path()).unwrap().count(),
            1,
            "no staging left"
        );
    }

    #[test]
    fn levels() {
        assert_eq!(level_num("Info"), 4);
        assert_eq!(level_num("Warning"), 3);
        assert_eq!(level_num("2"), 2);
    }
}
