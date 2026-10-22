//! `fhv-bench`: measures the engine on real files, headless.
//!
//! ```text
//! fhv-bench index big.csv --access mmap
//! ```
//! Prints one `key=value` line per measurement, for docs/PERF.md.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use fhv_engine::{Access, OpenOptions, Rows, Table};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum Acc {
    Mmap,
    Pread,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open + index a file; time to first rows, full index, memory
    Index {
        file: PathBuf,
        #[arg(long, value_enum, default_value = "pread")]
        access: Acc,
        /// Cache directory (default: a temp dir under target/)
        #[arg(long)]
        cache: Option<PathBuf>,
    },
}

/// (peak RSS, current RSS anon, current RSS file) in MiB, Linux only.
fn mem() -> String {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let get = |k: &str| {
        s.lines()
            .find(|l| l.starts_with(k))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<u64>().ok())
            .map_or(0, |kb| kb / 1024)
    };
    format!(
        "peak_rss_mib={} rss_anon_mib={} rss_file_mib={}",
        get("VmHWM:"),
        get("RssAnon:"),
        get("RssFile:")
    )
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Index {
            file,
            access,
            cache,
        } => {
            let cache = cache.unwrap_or_else(|| std::env::temp_dir().join("fhv-bench-cache"));
            let mut opts = OpenOptions::new(cache);
            opts.access = match access {
                Acc::Mmap => Access::Mmap,
                Acc::Pread => Access::Pread,
            };
            let t0 = Instant::now();
            let table = Arc::new(Table::open(&file, opts)?);
            let t_open = t0.elapsed();
            let worker = {
                let t = table.clone();
                std::thread::spawn(move || t.build())
            };
            // First screen: 100 rows readable and decoded.
            let mut rows = Rows::default();
            let t_first = loop {
                if table.row_count() >= 100 || worker.is_finished() {
                    table.read_rows(0..100, &mut rows)?;
                    break t0.elapsed();
                }
                std::thread::sleep(Duration::from_micros(200));
            };
            worker.join().expect("worker panicked")?;
            let t_all = t0.elapsed();
            let ix_bytes = table.index().heap_bytes();
            let mb = table.file_len as f64 / 1e6;
            println!(
                "from_cache={} file={} bytes={} rows={} cols={} encoding={} access={:?} open_ms={:.1} first_rows_ms={:.1} index_s={:.3} mb_per_s={:.0} index_mib={:.1} index_pct_of_file={:.2} {}",
                table.from_cache.load(std::sync::atomic::Ordering::Relaxed),
                file.display(),
                table.file_len,
                table.row_count(),
                table.columns.len(),
                table.encoding.name(),
                table.access().unwrap_or(Access::Mmap),
                t_open.as_secs_f64() * 1e3,
                t_first.as_secs_f64() * 1e3,
                t_all.as_secs_f64(),
                mb / t_all.as_secs_f64(),
                ix_bytes as f64 / (1 << 20) as f64,
                ix_bytes as f64 * 100.0 / table.file_len.max(1) as f64,
                mem()
            );
        }
    }
    Ok(())
}
