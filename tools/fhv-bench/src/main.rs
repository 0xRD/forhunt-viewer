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
    /// Filters and sorts on an indexed file (builds or loads the cache
    /// first). Operations: `name|json Expr|sort cols` lines from --ops.
    Query {
        file: PathBuf,
        /// One operation per line: `filter <json expr>` or `sort <col>[:desc],...`
        #[arg(long)]
        ops: PathBuf,
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

fn query(file: PathBuf, ops: PathBuf, cache: Option<PathBuf>) -> Result<()> {
    use fhv_engine::{Engine, Expr, SortKey};
    let cache = cache.unwrap_or_else(|| std::env::temp_dir().join("fhv-bench-cache"));
    let t0 = Instant::now();
    let table = Arc::new(Table::open(&file, OpenOptions::new(cache))?);
    table.build()?;
    println!(
        "ready_s={:.3} from_cache={} rows={}",
        t0.elapsed().as_secs_f64(),
        table.from_cache.load(std::sync::atomic::Ordering::Relaxed),
        table.row_count()
    );
    let engine = Engine::new(table.clone());
    let col = |name: &str| -> Result<usize> {
        table
            .columns
            .iter()
            .position(|c| c == name)
            .ok_or_else(|| anyhow::anyhow!("no column {name}"))
    };
    let mut last = roaring::RoaringBitmap::new();
    last.insert_range(0..table.row_count() as u32);
    for line in std::fs::read_to_string(&ops)?.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (kind, arg) = line.split_once(' ').unwrap_or((line, ""));
        let t = Instant::now();
        let detail = match kind {
            "filter" => {
                // Column names in the JSON are resolved: {"column":"Name"}.
                let mut v: serde_json::Value = serde_json::from_str(arg)?;
                resolve(&mut v, &col)?;
                let e: Expr = serde_json::from_value(v)?;
                last = engine.eval(&e)?;
                format!("matches={}", last.len())
            }
            "sort" => {
                let keys: Vec<SortKey> = arg
                    .split(',')
                    .map(|k| {
                        let (n, d) = k.split_once(':').unwrap_or((k, ""));
                        Ok(SortKey {
                            col: col(n)?,
                            desc: d == "desc",
                        })
                    })
                    .collect::<Result<_>>()?;
                let v = fhv_engine::sort::sort(&table, &last, &keys)?;
                format!("sorted={}", v.len())
            }
            "reset" => {
                last = roaring::RoaringBitmap::new();
                last.insert_range(0..table.row_count() as u32);
                "all rows".into()
            }
            _ => anyhow::bail!("unknown op {kind}"),
        };
        println!(
            "op_ms={:>9.1} {detail:<18} {line}",
            t.elapsed().as_secs_f64() * 1e3
        );
    }
    println!("{}", mem());
    Ok(())
}

/// Replaces `{"column": "Name"}` by `{"column": index}` in a JSON expr.
fn resolve(v: &mut serde_json::Value, col: &dyn Fn(&str) -> Result<usize>) -> Result<()> {
    match v {
        serde_json::Value::Object(m) => {
            if let Some(serde_json::Value::String(name)) = m.get("column").cloned() {
                m.insert("column".into(), serde_json::Value::from(col(&name)?));
            }
            for (_, x) in m.iter_mut() {
                resolve(x, col)?;
            }
        }
        serde_json::Value::Array(a) => {
            for x in a {
                resolve(x, col)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Query { file, ops, cache } => query(file, ops, cache)?,
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
            let mut t_rows = None;
            while !worker.is_finished() {
                if t_rows.is_none() && table.progress.phase() == fhv_engine::Phase::Columns {
                    t_rows = Some(t0.elapsed());
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            worker.join().expect("worker panicked")?;
            let t_all = t0.elapsed();
            let t_rows = t_rows.unwrap_or(t_all);
            let cols = table.column_index();
            let col_bytes: usize = cols.as_ref().map_or(0, |c| {
                c.cols
                    .iter()
                    .map(|c| {
                        c.dict.as_ref().map_or(0, |d| {
                            d.bitmaps.iter().map(|b| b.serialized_size()).sum::<usize>()
                        }) + c.time.as_ref().map_or(0, |t| {
                            t.ticks.len() * 8 + t.perm.as_ref().map_or(0, |p| p.len() * 4)
                        })
                    })
                    .sum()
            });
            let summary = cols.as_ref().map_or(String::new(), |c| {
                c.cols
                    .iter()
                    .zip(&table.columns)
                    .filter(|(c, _)| c.dict.is_some() || c.time.is_some())
                    .map(|(c, n)| {
                        format!(
                            "{n}:{}{}",
                            c.dict
                                .as_ref()
                                .map_or(String::new(), |d| format!("dict{}", d.values.len())),
                            if c.time.is_some() { "+time" } else { "" }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            });
            println!(
                "rows_ready_s={:.3} columns_s={:.3} column_index_mib={:.1} ragged={} indexed=[{summary}]",
                t_rows.as_secs_f64(),
                (t_all - t_rows).as_secs_f64(),
                col_bytes as f64 / (1 << 20) as f64,
                cols.as_ref().map_or(0, |c| c.ragged.len()),
            );
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
