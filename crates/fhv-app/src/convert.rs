//! Opening forhunt evidence (EVTX, `$MFT`, `$UsnJrnl:$J`, prefetch, or a
//! folder of them): forhunt's own libraries write the events to a CSV in the cache,
//! byte-identical to `forhunt parse -F csv`, and the CSV opens as a normal
//! tab (D-033). Nothing is written next to the evidence.

use std::io::{BufWriter, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::Instant;

use forhunt_core::artifact::ParseOptions;

/// forhunt version linked in (part of the cache key: a new parser rewrites).
pub const FORHUNT_VERSION: &str = "0.4.0";

/// True if `path` is something forhunt parses rather than a CSV: a
/// directory, or a file an artifact recognises by content or name.
pub fn is_evidence(path: &Path) -> bool {
    if path.is_dir() {
        return true;
    }
    let mut head = [0u8; 8192];
    let n = match std::fs::File::open(path) {
        Ok(mut f) => f.read(&mut head).unwrap_or(0),
        Err(_) => return false,
    };
    forhunt_core::artifact::registry()
        .iter()
        .any(|a| a.sniff(&head[..n]) || a.matches_name(path))
}

pub struct Converted {
    pub csv: PathBuf,
    pub events: u64,
    pub warnings: Vec<String>,
    pub cached: bool,
}

/// One background conversion.
pub struct Conversion {
    pub label: String,
    /// The evidence, for `forhunt show <id> -p <evidence>`.
    pub evidence: String,
    pub done: Arc<AtomicU64>,
    pub total: Arc<AtomicU64>,
    pub cancel: Arc<AtomicBool>,
    pub started: Instant,
    rx: Receiver<Result<Converted, String>>,
}

impl Conversion {
    /// Starts converting `inputs` (files and folders, opened together as
    /// one timeline) into a CSV under `cache/forhunt/`.
    pub fn start(inputs: Vec<PathBuf>, cache: &Path) -> Conversion {
        let label = label(&inputs);
        let evidence = evidence_arg(&inputs);
        let done = Arc::new(AtomicU64::new(0));
        let total = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let dir = cache.join("forhunt");
        let job = {
            let (done, total, cancel) = (done.clone(), total.clone(), cancel.clone());
            let label = label.clone();
            move || {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(&inputs, &dir, &label, &done, &total, &cancel)
                }));
                let _ = tx.send(match r {
                    Ok(r) => r,
                    Err(_) => Err("internal error while parsing (panic)".into()),
                });
            }
        };
        // If the thread cannot start, the sender is dropped and `poll`
        // reports it.
        let _ = std::thread::Builder::new()
            .name("fhv-forhunt".into())
            .spawn(job);
        Conversion {
            label,
            evidence,
            done,
            total,
            cancel,
            started: Instant::now(),
            rx,
        }
    }

    /// The result once finished.
    pub fn poll(&self) -> Option<Result<Converted, String>> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("the parser stopped unexpectedly".into())),
        }
    }

    /// Progress in 0..=1 (`None` while the inputs are being opened).
    pub fn fraction(&self) -> Option<f32> {
        let t = self.total.load(Ordering::Relaxed);
        (t > 0).then(|| (self.done.load(Ordering::Relaxed) as f64 / t as f64).min(1.0) as f32)
    }
}

fn run(
    inputs: &[PathBuf],
    dir: &Path,
    label: &str,
    done: &AtomicU64,
    total: &AtomicU64,
    cancel: &AtomicBool,
) -> Result<Converted, String> {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let pool = forhunt_core::engine::build_pool(0, cores).map_err(|e| e.to_string())?;
    let args: Vec<String> = inputs.iter().map(|p| p.display().to_string()).collect();
    let o = forhunt_core::input::open_all(&args, &ParseOptions::default(), &pool)
        .map_err(|e| format!("{e:#}"))?;
    if o.sources.is_empty() {
        let why = if o.warnings.is_empty() {
            "no event log, NTFS or prefetch file found".to_string()
        } else {
            o.warnings.join("\n")
        };
        return Err(why);
    }
    // Same files (by forhunt's content key, path, size and modification
    // time), same forhunt: same CSV.
    let mut h = blake3::Hasher::new();
    h.update(FORHUNT_VERSION.as_bytes());
    for s in &o.sources {
        h.update(s.key().to_string().as_bytes());
        h.update(s.path().as_os_str().as_encoded_bytes());
        h.update(&[0]);
        let (len, mtime) = std::fs::metadata(s.path()).map_or((0, 0), |m| {
            let t = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            (m.len(), t)
        });
        h.update(&len.to_le_bytes());
        h.update(&mtime.to_le_bytes());
    }
    let key = h.finalize().to_hex();
    let out_dir = dir.join(&key.as_str()[..16]);
    let csv = out_dir.join(format!("{}.csv", file_safe(label)));
    if csv.is_file() {
        return Ok(Converted {
            csv,
            events: 0,
            warnings: o.warnings,
            cached: true,
        });
    }
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
    total.store(o.bytes.max(1), Ordering::Relaxed);
    // Unique per conversion: the same evidence may be converting twice at
    // once (dropped again while the first parse runs).
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let tmp = out_dir.join(format!(
        ".partial-{}-{}.csv",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let f = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let mut w = BufWriter::with_capacity(1 << 20, f);
    let map = forhunt_core::normalize::FieldMap::default_map();
    let writer = forhunt_output::Writer::new(forhunt_output::Format::Csv, Default::default(), map);
    let r =
        forhunt_output::export::write_all(&o.sources, &pool, &writer, &mut w, cancel, &mut |d| {
            done.store(d, Ordering::Relaxed)
        });
    drop(w);
    let stats = match r {
        Ok(s) => s,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            let _ = std::fs::remove_dir(&out_dir);
            return Err(if e.kind() == std::io::ErrorKind::Interrupted {
                "cancelled".into()
            } else {
                e.to_string()
            });
        }
    };
    if csv.is_file() {
        // Another conversion of the same inputs finished first: its CSV is
        // the same bytes and may already be open; keep it.
        let _ = std::fs::remove_file(&tmp);
    } else {
        std::fs::rename(&tmp, &csv).map_err(|e| format!("{}: {e}", csv.display()))?;
    }
    let mut warnings = o.warnings;
    if stats.bad_units > 0 || stats.bad_records > 0 {
        warnings.push(format!(
            "{} damaged chunks, {} damaged records skipped",
            stats.bad_units, stats.bad_records
        ));
    }
    Ok(Converted {
        csv,
        events: stats.events,
        warnings,
        cached: false,
    })
}

/// Tab name: the file or folder name, or "<parent> (N files)".
fn label(inputs: &[PathBuf]) -> String {
    let name = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.display().to_string())
    };
    match inputs {
        [one] => name(one),
        many => {
            let parent = common_parent(many)
                .map(|p| name(&p))
                .unwrap_or_else(|| "evidence".into());
            let dirs = many.iter().filter(|p| p.is_dir()).count();
            let what = match dirs {
                0 => "files",
                d if d == many.len() => "folders",
                _ => "items",
            };
            format!("{parent} ({} {what})", many.len())
        }
    }
}

/// The path to pass to `forhunt show -p`: the input, or the folder that
/// holds them all.
fn evidence_arg(inputs: &[PathBuf]) -> String {
    match inputs {
        [one] => one.display().to_string(),
        many => common_parent(many)
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
    }
}

fn common_parent(paths: &[PathBuf]) -> Option<PathBuf> {
    let mut base = paths.first()?.parent()?.to_path_buf();
    for p in &paths[1..] {
        while !p.starts_with(&base) {
            if !base.pop() {
                return None;
            }
        }
    }
    Some(base)
}

fn file_safe(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect();
    let out = out.trim_matches(['.', ' ']).to_string();
    if out.is_empty() { "events".into() } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forhunt_version_matches_the_lock_file() {
        // Part of the cache key: it must follow the linked forhunt release.
        let lock = include_str!("../../../Cargo.lock");
        let i = lock.find("name = \"forhunt-core\"").unwrap();
        let ver = lock[i..].lines().nth(1).unwrap();
        assert_eq!(ver, format!("version = \"{FORHUNT_VERSION}\""));
    }

    #[test]
    fn labels_and_evidence() {
        let a = PathBuf::from("/case/AD01/Security.evtx");
        let b = PathBuf::from("/case/PC01/System.evtx");
        assert_eq!(label(std::slice::from_ref(&a)), "Security.evtx");
        assert_eq!(label(&[a.clone(), b.clone()]), "case (2 files)");
        let d = tempfile::tempdir().unwrap();
        let (h1, h2) = (d.path().join("AD01"), d.path().join("PC01"));
        std::fs::create_dir_all(&h1).unwrap();
        std::fs::create_dir_all(&h2).unwrap();
        assert!(label(&[h1, h2]).ends_with(" (2 folders)"));
        assert_eq!(evidence_arg(&[a, b]), "/case");
        assert_eq!(file_safe("a/b:c"), "a_b_c");
    }

    /// Synthetic log from forhunt's evtx-gen (forensic-hunter fixtures).
    const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/PowerShell.evtx");

    fn wait(c: &Conversion) -> Result<Converted, String> {
        let t = Instant::now();
        loop {
            if let Some(r) = c.poll() {
                return r;
            }
            assert!(t.elapsed().as_secs() < 30, "conversion timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn prefetch_opens_as_a_timeline() {
        let pf: PathBuf =
            concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/CMD.EXE-0BD30981.pf").into();
        assert!(is_evidence(&pf));
        let cache = tempfile::tempdir().unwrap();
        let done = wait(&Conversion::start(vec![pf], cache.path())).unwrap();
        // One row per stored run time.
        assert_eq!(done.events, 3);
        let csv = std::fs::read_to_string(&done.csv).unwrap();
        assert!(csv.contains("Program executed"));
        assert!(csv.contains("WINDOWS\\SYSTEM32\\CMD.EXE"));
    }

    #[test]
    fn converts_then_reuses_the_cache() {
        let cache = tempfile::tempdir().unwrap();
        let c = Conversion::start(vec![FIXTURE.into()], cache.path());
        assert_eq!(c.label, "PowerShell.evtx");
        let done = wait(&c).unwrap();
        assert!(!done.cached);
        assert!(done.events > 0);
        let csv = std::fs::read_to_string(&done.csv).unwrap();
        assert!(csv.starts_with("RecordNumber,EventRecordId,TimeCreated,"));
        assert!(csv.lines().next().unwrap().ends_with(",ForhuntId"));
        assert!(done.csv.starts_with(cache.path().join("forhunt")));
        assert_eq!(c.fraction(), Some(1.0));
        // Same input again: the CSV is reused, not rewritten.
        let again = wait(&Conversion::start(vec![FIXTURE.into()], cache.path())).unwrap();
        assert!(again.cached);
        assert_eq!(again.csv, done.csv);
    }

    /// The same evidence opened twice at once (dropped again, Open recent
    /// while the first parse runs): every conversion succeeds with the
    /// complete CSV. They used to share one `.partial.csv`: the later ones
    /// truncated the file the first had just published, then failed to
    /// rename it.
    #[test]
    fn concurrent_conversions_of_the_same_input() {
        let cache = tempfile::tempdir().unwrap();
        let want = {
            let solo = tempfile::tempdir().unwrap();
            let d = wait(&Conversion::start(vec![FIXTURE.into()], solo.path())).unwrap();
            std::fs::read(d.csv).unwrap()
        };
        for _ in 0..5 {
            let _ = std::fs::remove_dir_all(cache.path().join("forhunt"));
            let cs: Vec<Conversion> = (0..4)
                .map(|_| Conversion::start(vec![FIXTURE.into()], cache.path()))
                .collect();
            for c in &cs {
                let d = wait(c).unwrap();
                assert_eq!(std::fs::read(&d.csv).unwrap(), want);
            }
        }
        // No temporary file left behind.
        let dir = std::fs::read_dir(cache.path().join("forhunt"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["PowerShell.evtx.csv"]);
    }

    /// The conversion cache key includes each file's size and modification
    /// time, not only forhunt's sampled content key: an evidence file
    /// replaced in place (same sampled head and tail) is parsed again.
    #[test]
    fn conversion_cache_follows_size_and_mtime() {
        let d = tempfile::tempdir().unwrap();
        let evtx = d.path().join("PowerShell.evtx");
        std::fs::copy(FIXTURE, &evtx).unwrap();
        let cache = tempfile::tempdir().unwrap();
        let first = wait(&Conversion::start(vec![evtx.clone()], cache.path())).unwrap();
        assert!(!first.cached);
        assert!(
            wait(&Conversion::start(vec![evtx.clone()], cache.path()))
                .unwrap()
                .cached
        );
        let f = std::fs::File::options().write(true).open(&evtx).unwrap();
        f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(3600))
            .unwrap();
        drop(f);
        let again = wait(&Conversion::start(vec![evtx], cache.path())).unwrap();
        assert!(!again.cached, "same content key, new mtime: parsed again");
        assert_ne!(again.csv, first.csv);
    }

    #[test]
    fn reports_nothing_to_parse() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("notes.txt"), "x").unwrap();
        let cache = tempfile::tempdir().unwrap();
        let r = wait(&Conversion::start(vec![d.path().into()], cache.path()));
        assert!(r.is_err());
    }

    #[test]
    fn detects_evidence() {
        let d = tempfile::tempdir().unwrap();
        let csv = d.path().join("t.csv");
        std::fs::write(&csv, "a,b\n1,2\n").unwrap();
        let evtx = d.path().join("x.evtx");
        std::fs::write(&evtx, b"ElfFile\0rest").unwrap();
        assert!(!is_evidence(&csv));
        assert!(is_evidence(&evtx));
        assert!(is_evidence(d.path()));
    }
}
