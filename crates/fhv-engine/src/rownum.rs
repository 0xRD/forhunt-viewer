//! Detection of a row-number column the file already has (`RecordNumber`,
//! `#`, `No.`, `Index`...), so the viewer need not add its own `#` next to
//! it (D-042). The decision uses the header name and a sample of values (the
//! first and last rows and a few windows between), so it is instant on any
//! file. Only a column that really is a counter qualifies: a time-sorted
//! `EventRecordId` with gaps or repeats is not one, and when unsure the
//! viewer keeps its own `#`.

use crate::table::Table;

/// Header names of a row-number column, normalised by `norm_name`.
const NAMES: &[&str] = &[
    "#",
    "no",
    "nr",
    "num",
    "number",
    "n°",
    "numero",
    "index",
    "idx",
    "line",
    "lineno",
    "linenumber",
    "ligne",
    "row",
    "rowno",
    "rownum",
    "rownumber",
    "rowid",
    "id",
    "rec",
    "recno",
    "recnum",
    "record",
    "recordid",
    "recordno",
    "recordnum",
    "recordnumber",
    "eventrecordid",
    "entry",
    "entrynumber",
    "serial",
    "seq",
    "sequence",
];

/// Lowercase, accents of the French names folded, and separators dropped
/// (`Record ID`, `record_id` and `RecordId` are one name).
fn norm_name(h: &str) -> String {
    h.trim()
        .chars()
        .filter_map(|c| match c {
            'é' | 'É' | 'è' | 'È' => Some('e'),
            '#' | '°' => Some(c),
            c if c.is_alphanumeric() => Some(c.to_ascii_lowercase()),
            _ => None,
        })
        .collect()
}

/// Whether header `h` is a usual name of a row-number column.
pub fn name_matches(h: &str) -> bool {
    NAMES.contains(&norm_name(h).as_str())
}

/// Rows whose values decide: all of a small file, else the first and last
/// 64 rows and six windows of 8 in between.
fn sample_rows(n: u64) -> Vec<u64> {
    if n <= 256 {
        return (0..n).collect();
    }
    let mut v: Vec<u64> = (0..64).chain(n - 64..n).collect();
    for k in 1..=6 {
        let s = n / 7 * k;
        v.extend(s..s + 8);
    }
    v.sort_unstable();
    v.dedup();
    v
}

fn parse(s: &str) -> Option<u64> {
    let s = s.trim();
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// The first column named like a row-number column whose values are a
/// counter: integers starting at 0 or 1, strictly increasing and following
/// the row position (within 0.1 % of the row count), on every sampled row.
/// `row(r)` returns the fields of data row `r`.
pub fn detect(
    columns: &[String],
    rows: u64,
    mut row: impl FnMut(u64) -> Option<Vec<String>>,
) -> Option<usize> {
    let mut cands: Vec<usize> = (0..columns.len())
        .filter(|&c| name_matches(&columns[c]))
        .collect();
    if cands.is_empty() || rows < 2 {
        return None;
    }
    let mut vals: Vec<Vec<(u64, u64)>> = vec![Vec::new(); columns.len()];
    for r in sample_rows(rows) {
        let f = row(r)?;
        cands.retain(|&c| match f.get(c).and_then(|s| parse(s)) {
            Some(v) => {
                vals[c].push((r, v));
                true
            }
            None => false,
        });
        if cands.is_empty() {
            return None;
        }
    }
    let tol = rows / 1000;
    cands.into_iter().find(|&c| {
        let v = &vals[c];
        let base = v[0].1;
        base <= 1
            && v.iter().all(|&(r, x)| x.abs_diff(base + r) <= tol)
            && v.windows(2).all(|w| w[1].1 > w[0].1)
    })
}

/// `detect` on an indexed table.
pub fn detect_table(t: &Table) -> Option<usize> {
    detect(&t.columns, t.row_count(), |r| t.row_fields(r).ok())
}

/// Data row whose value is `target`, in a column that increases with the row
/// position (what `detect` accepts): a guess from the value, then a binary
/// search, at most ~35 reads. `value(r)` is the number in data row `r`.
pub fn find(rows: u64, mut value: impl FnMut(u64) -> Option<u64>, target: u64) -> Option<u64> {
    if rows == 0 {
        return None;
    }
    let base = value(0)?;
    let guess = target.checked_sub(base)?.min(rows - 1);
    if value(guess) == Some(target) {
        return Some(guess);
    }
    let (mut lo, mut hi) = (0, rows);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let v = value(mid)?;
        if v == target {
            return Some(mid);
        }
        if v < target {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    None
}

/// `find` on a table column (cell text parsed as an integer).
pub fn find_in_table(t: &Table, col: usize, target: u64) -> Option<u64> {
    find(
        t.row_count(),
        |r| t.row_fields(r).ok()?.get(col).and_then(|s| parse(s)),
        target,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn run(names: &[&str], data: &[Vec<String>]) -> Option<usize> {
        detect(&cols(names), data.len() as u64, |r| {
            data.get(r as usize).cloned()
        })
    }

    fn counter(n: u64, start: u64) -> Vec<Vec<String>> {
        (0..n)
            .map(|i| vec![(start + i).to_string(), format!("x{i}")])
            .collect()
    }

    fn swap(d: Vec<Vec<String>>) -> Vec<Vec<String>> {
        d.into_iter()
            .map(|r| vec![r[1].clone(), r[0].clone()])
            .collect()
    }

    #[test]
    fn names() {
        for n in [
            "#",
            "No",
            "No.",
            "NUM",
            "Number",
            "Index",
            "idx",
            "Line",
            "Row",
            "RowNum",
            "RecordNumber",
            "Record ID",
            "record_id",
            "RecordID",
            "Rec",
            "numéro",
            "Numéro",
            "N°",
            "ligne",
            "Id",
            "EntryNumber",
        ] {
            assert!(name_matches(n), "{n}");
        }
        for n in [
            "Time",
            "EventId",
            "Message",
            "ProcessId",
            "Identifier",
            "Computer",
            "",
        ] {
            assert!(!name_matches(n), "{n}");
        }
    }

    #[test]
    fn counters_one_or_zero_based() {
        assert_eq!(run(&["No.", "v"], &counter(100, 1)), Some(0));
        assert_eq!(run(&["v", "Index"], &swap(counter(5000, 0))), Some(1));
        assert_eq!(run(&["#", "v"], &counter(2, 1)), Some(0));
    }

    #[test]
    fn not_a_counter() {
        assert_eq!(run(&["Value", "v"], &counter(100, 1)), None);
        assert_eq!(run(&["EventRecordId", "v"], &counter(1000, 5000)), None);
        let mut d = counter(1000, 1);
        d.reverse();
        assert_eq!(run(&["No", "v"], &d), None);
        let d: Vec<Vec<String>> = (0..1000u64)
            .map(|i| vec![(i / 2 + 1).to_string(), "x".into()])
            .collect();
        assert_eq!(run(&["No", "v"], &d), None);
        let mut d = counter(1000, 1);
        d[500][0] = "n/a".into();
        d[999][0] = String::new();
        assert_eq!(run(&["No", "v"], &d), None);
        assert_eq!(run(&["No", "v"], &counter(1, 1)), None);
        assert_eq!(run(&["No", "v"], &[]), None);
    }

    /// A record id sorted by time has gaps and repeats: not a row counter.
    #[test]
    fn gappy_event_record_id_is_not_detected() {
        let mut id = 1u64;
        let d: Vec<Vec<String>> = (0..20_000u64)
            .map(|i| {
                id += [1, 1, 3, 0, 2, 1, 7][(i % 7) as usize];
                vec![if i == 0 { 1 } else { id }.to_string(), "x".into()]
            })
            .collect();
        assert_eq!(run(&["EventRecordId", "v"], &d), None);
        // A few missing records stay within the 0.1 % tolerance.
        let d: Vec<Vec<String>> = (0..20_000u64)
            .map(|i| vec![(i + 1 + i / 5000).to_string(), "x".into()])
            .collect();
        assert_eq!(run(&["EventRecordId", "v"], &d), Some(0));
        let d: Vec<Vec<String>> = (0..20_000u64)
            .map(|i| vec![(i + 1 + i / 100).to_string(), "x".into()])
            .collect();
        assert_eq!(run(&["EventRecordId", "v"], &d), None);
    }

    #[test]
    fn first_valid_candidate_wins() {
        let d: Vec<Vec<String>> = (0..300u64)
            .map(|i| vec![(i * 7).to_string(), (i + 1).to_string()])
            .collect();
        assert_eq!(run(&["Id", "Line"], &d), Some(1));
    }

    #[test]
    fn find_by_value() {
        let v = |r: u64| Some(r + 1);
        assert_eq!(find(100, v, 1), Some(0));
        assert_eq!(find(100, v, 100), Some(99));
        assert_eq!(find(100, v, 101), None);
        assert_eq!(find(100, v, 0), None);
        let w = |r: u64| Some(r + 1 + r / 10);
        assert_eq!(find(1000, w, w(777).unwrap()), Some(777));
        assert_eq!(find(1000, w, 11), None);
        assert_eq!(find(0, v, 1), None);
    }
}
