//! `csv-gen`: synthetic CSV files for forhunt-viewer tests and benchmarks.
//!
//! ```text
//! csv-gen gen --layout forhunt --size 5G --seed 1 -o big.csv
//! csv-gen gen --layout mft --rows 100000 -o mft.csv
//! csv-gen edge --out target/edge
//! ```

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use csv_gen::{Layout, Spec, edge, generate};

#[derive(Parser)]
#[command(version, about = "Seeded synthetic CSV generator")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Generate one file in a given layout
    Gen {
        #[arg(long, value_enum, default_value = "forhunt")]
        layout: Layout,
        /// Number of data rows
        #[arg(long, conflicts_with = "size")]
        rows: Option<u64>,
        /// Approximate size (e.g. 500M, 5G); stops at the first block boundary past it
        #[arg(long)]
        size: Option<String>,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// Delimiter: , ; tab |
        #[arg(long, default_value = ",")]
        delim: String,
        /// CRLF line endings
        #[arg(long)]
        crlf: bool,
        #[arg(short, long)]
        out: PathBuf,
    },
    /// Write the edge-case suite to a directory
    Edge {
        #[arg(long)]
        out: PathBuf,
    },
}

fn parse_size(s: &str) -> Result<u64> {
    let s = s.trim();
    let (num, mult) = match s.char_indices().last() {
        Some((i, c)) if c.is_ascii_alphabetic() => {
            let m = match c.to_ascii_uppercase() {
                'K' => 1u64 << 10,
                'M' => 1 << 20,
                'G' => 1 << 30,
                _ => bail!("unknown size suffix in {s}"),
            };
            (&s[..i], m)
        }
        _ => (s, 1),
    };
    let n: f64 = num.parse().with_context(|| format!("bad size {s}"))?;
    Ok((n * mult as f64) as u64)
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Gen {
            layout,
            rows,
            size,
            seed,
            delim,
            crlf,
            out,
        } => {
            let delim = match delim.as_str() {
                "," => b',',
                ";" => b';',
                "tab" | "\t" => b'\t',
                "|" => b'|',
                d => bail!("unsupported delimiter {d:?}"),
            };
            let bytes = size.as_deref().map(parse_size).transpose()?;
            if rows.is_none() && bytes.is_none() {
                bail!("give --rows or --size");
            }
            let spec = Spec {
                layout,
                seed,
                rows,
                bytes,
                delim,
                crlf,
            };
            let f = File::create(&out).with_context(|| format!("create {}", out.display()))?;
            let mut w = BufWriter::with_capacity(4 << 20, f);
            let s = generate(&spec, &mut w)?;
            eprintln!("{}: {} rows, {} bytes", out.display(), s.rows, s.bytes);
        }
        Cmd::Edge { out } => {
            fs::create_dir_all(&out)?;
            for case in edge::cases() {
                fs::write(out.join(case.name), &case.bytes)?;
                eprintln!("{:28} {:>9} B  {}", case.name, case.bytes.len(), case.what);
            }
        }
    }
    Ok(())
}
