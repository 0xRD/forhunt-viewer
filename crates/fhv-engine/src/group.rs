//! Grouping of a view by one or several columns (nested, first level
//! outermost).
//!
//! Each level gives every row of the view a value code: from the column's
//! dictionary bitmaps when it has one (no file read), else from a parallel,
//! cancellable scan of the column. Blank cells (only ASCII whitespace, like
//! the `is blank` operator) share one `(blank)` group; other values are
//! grouped by their exact decoded text. Codes are combined level by level
//! into group nodes (parent, value), counted, and ordered per level (by
//! value or by count). The result is the tree in preorder plus the view's
//! rows reordered by group: rows of a leaf group keep the view's order, so
//! a leaf is a contiguous range of display positions.
//!
//! Memory: 4 B per view row for the node of each row and 4 B per view row
//! for the new order, plus, for dictionary levels and sorted views, a
//! transient 4 B per file row; never the cells themselves. At most
//! [`MAX_GROUPS`] groups (all levels): past it the grouping stops with an
//! error instead of materialising millions of headers.

use std::cmp::Ordering;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use roaring::RoaringBitmap;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use crate::columns::{Kind, field_bytes, parse_num, text_cmp};
use crate::filter::Engine;
use crate::scan::nth_field;
use crate::time;
use crate::view::View;
use crate::{Error, Result};

/// Most groups (all levels together) a grouping may produce.
pub const MAX_GROUPS: usize = 1_000_000;

const BLOCK: u64 = 32_768;
const NONE: u32 = u32::MAX;

/// Order of the groups of one level (under each parent).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupSort {
    /// By value, in the column's order (dates, numbers, else text; blank
    /// first).
    #[default]
    ValueAsc,
    ValueDesc,
    /// Largest groups first (ties by value).
    CountDesc,
    CountAsc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Level {
    pub col: usize,
    #[serde(default)]
    pub sort: GroupSort,
}

/// One group, in preorder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Node {
    /// Level (0 = outermost).
    pub depth: u32,
    /// Value code in [`Groups::values`] of its level.
    pub code: u32,
    /// Rows of the view in the group.
    pub count: u64,
    /// First display position of its rows in the grouped view.
    pub start: u64,
    /// Parent node (`u32::MAX` for the outermost level).
    pub parent: u32,
    /// Preorder index just past its subtree.
    pub end: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Groups {
    pub levels: Vec<Level>,
    /// Distinct values of each level (`None`: the blank group).
    pub values: Vec<Vec<Option<String>>>,
    /// The groups in preorder (display order).
    pub nodes: Vec<Node>,
}

impl Groups {
    /// Value of a node (`None`: blank).
    pub fn value(&self, n: usize) -> Option<&str> {
        let nd = self.nodes.get(n)?;
        self.values
            .get(nd.depth as usize)?
            .get(nd.code as usize)?
            .as_deref()
    }

    pub fn is_leaf(&self, n: usize) -> bool {
        self.nodes
            .get(n)
            .is_some_and(|nd| nd.depth as usize + 1 == self.levels.len())
    }

    /// Nodes from the outermost level down to `n`.
    pub fn path(&self, n: usize) -> Vec<usize> {
        let mut v = vec![n];
        let mut p = self.nodes[n].parent;
        while p != NONE {
            v.push(p as usize);
            p = self.nodes[p as usize].parent;
        }
        v.reverse();
        v
    }
}

/// Codes of one level for the rows of `rows`, in the bitmap's order, and
/// the level's distinct values.
fn level_codes(
    engine: &Engine,
    rows: &RoaringBitmap,
    col: usize,
    done: &AtomicU64,
    cap: usize,
) -> Result<(Vec<u32>, Vec<Option<String>>)> {
    let table = engine.table();
    let blank = |v: &[u8]| v.iter().all(u8::is_ascii_whitespace);
    let mut values: Vec<Option<String>> = Vec::new();
    let mut blank_code = NONE;
    let mut blank_of = |values: &mut Vec<Option<String>>| {
        if blank_code == NONE {
            blank_code = values.len() as u32;
            values.push(None);
        }
        blank_code
    };
    if let Some(ci) = table.column_index()
        && let Some(d) = ci.cols.get(col).and_then(|c| c.dict.as_ref())
    {
        let max = rows.max().map_or(0, |m| m as usize + 1);
        let mut dense = vec![NONE; max];
        for (v, b) in d.values.iter().zip(&d.bitmaps) {
            if table.progress.cancelled() {
                return Err(Error::Cancelled);
            }
            let hit = b & rows;
            if hit.is_empty() {
                continue;
            }
            let code = if blank(v.as_bytes()) {
                blank_of(&mut values)
            } else {
                values.push(Some(v.clone()));
                values.len() as u32 - 1
            };
            for r in &hit {
                dense[r as usize] = code;
            }
        }
        // A row no value bitmap holds (none expected) is blank.
        let mut codes = Vec::with_capacity(rows.len() as usize);
        for r in rows {
            let c = dense[r as usize];
            codes.push(if c == NONE { blank_of(&mut values) } else { c });
        }
        done.fetch_add(rows.len(), Relaxed);
        return Ok((codes, values));
    }
    let t = table.tables();
    let quote = table.dialect.quote;
    let blocks = table.par_blocks_of(rows, BLOCK);
    let mut global: FxHashMap<Vec<u8>, u32> = FxHashMap::default();
    let mut codes = Vec::with_capacity(rows.len() as usize);
    let mut err = None;
    table
        .par_block_list(
            &blocks,
            |b| {
                // Block-local dictionary, merged in order by `consume`.
                let mut local: FxHashMap<Vec<u8>, u32> = FxHashMap::default();
                let mut out = Vec::new();
                let mut tmp = Vec::new();
                let r0 = b.first as u32;
                for r in rows.range(r0..r0 + b.len() as u32) {
                    let row = b.row((r - r0) as usize);
                    let cell = match nth_field(t, row, col) {
                        Some(s) => field_bytes(row, s, quote, &mut tmp),
                        None => &[][..],
                    };
                    if blank(cell) {
                        out.push(NONE);
                        continue;
                    }
                    let code = match local.get(cell) {
                        Some(&c) => c,
                        None => {
                            let c = local.len() as u32;
                            local.insert(cell.to_vec(), c);
                            c
                        }
                    };
                    out.push(code);
                }
                (local, out)
            },
            |_, (local, out)| {
                let mut map = vec![0u32; local.len()];
                for (v, c) in local {
                    let n = global.len() as u32;
                    map[c as usize] = *global.entry(v).or_insert(n);
                }
                if global.len() > cap {
                    err = Some(too_many(table.columns.get(col), cap));
                    return Err(Error::Cancelled);
                }
                done.fetch_add(out.len() as u64, Relaxed);
                codes.extend(out.into_iter().map(
                    |c| {
                        if c == NONE { NONE } else { map[c as usize] }
                    },
                ));
                Ok(())
            },
        )
        .map_err(|e| err.take().unwrap_or(e))?;
    let mut vals: Vec<(u32, Vec<u8>)> = global.into_iter().map(|(v, c)| (c, v)).collect();
    vals.sort_unstable_by_key(|x| x.0);
    values.extend(
        vals.into_iter()
            .map(|(_, v)| Some(String::from_utf8_lossy(&v).into_owned())),
    );
    if codes.contains(&NONE) {
        let b = blank_of(&mut values);
        codes
            .iter_mut()
            .filter(|c| **c == NONE)
            .for_each(|c| *c = b);
    }
    Ok((codes, values))
}

fn too_many(name: Option<&String>, cap: usize) -> Error {
    Error::Other(format!(
        "grouping stopped: more than {cap} groups (column {}); filter first or group by \
         a column with fewer distinct values",
        name.map_or("?", String::as_str)
    ))
}

/// Rank of each value in the column's order: blank first, then dates /
/// numbers that parse (unparsable before them), else text; ties by
/// [`text_cmp`].
pub fn value_ranks(values: &[Option<String>], kind: Kind) -> Vec<u32> {
    #[derive(PartialEq, PartialOrd)]
    enum K {
        Blank,
        Bad,
        Time(i64),
        Num(f64),
        Text,
    }
    let keys: Vec<K> = values
        .iter()
        .map(|v| match (v, kind) {
            (None, _) => K::Blank,
            (Some(s), Kind::Time(f)) => time::parse(s.as_bytes(), f).map_or(K::Bad, K::Time),
            (Some(s), Kind::Number) => {
                parse_num(s.as_bytes()).map_or(K::Bad, |n| K::Num(n.as_f64()))
            }
            (Some(_), Kind::Text) => K::Text,
        })
        .collect();
    let cmp_num = |a: &str, b: &str| match (parse_num(a.as_bytes()), parse_num(b.as_bytes())) {
        (Some(x), Some(y)) => x.total_cmp(y),
        _ => Ordering::Equal,
    };
    let mut idx: Vec<usize> = (0..values.len()).collect();
    idx.sort_by(|&a, &b| {
        let (ka, kb) = (&keys[a], &keys[b]);
        let by_kind = match (ka, kb) {
            (K::Time(x), K::Time(y)) => x.cmp(y),
            (K::Num(_), K::Num(_)) => cmp_num(
                values[a].as_deref().unwrap_or(""),
                values[b].as_deref().unwrap_or(""),
            ),
            _ => ka.partial_cmp(kb).unwrap_or(Ordering::Equal),
        };
        by_kind.then_with(|| {
            text_cmp(
                values[a].as_deref().unwrap_or("").as_bytes(),
                values[b].as_deref().unwrap_or("").as_bytes(),
            )
        })
    });
    let mut rank = vec![0u32; values.len()];
    for (r, &i) in idx.iter().enumerate() {
        rank[i] = r as u32;
    }
    rank
}

/// One level's nodes: (parent, value code, count).
struct Tier {
    nodes: Vec<(u32, u32, u64)>,
    rank: Vec<u32>,
    sort: GroupSort,
}

impl Tier {
    fn cmp(&self, a: u32, b: u32) -> Ordering {
        let (x, y) = (self.nodes[a as usize], self.nodes[b as usize]);
        let (rx, ry) = (self.rank[x.1 as usize], self.rank[y.1 as usize]);
        match self.sort {
            GroupSort::ValueAsc => rx.cmp(&ry),
            GroupSort::ValueDesc => ry.cmp(&rx),
            GroupSort::CountDesc => y.2.cmp(&x.2).then(rx.cmp(&ry)),
            GroupSort::CountAsc => x.2.cmp(&y.2).then(rx.cmp(&ry)),
        }
    }
}

/// Groups the rows of `view` by `levels`. Returns the view reordered by
/// group (same rows) and the group tree. `done` counts rows processed per
/// level (progress). Cancellable through the table's progress.
pub fn group(
    engine: &Engine,
    view: &View,
    levels: &[Level],
    done: &AtomicU64,
) -> Result<(View, Groups)> {
    group_with_cap(engine, view, levels, done, MAX_GROUPS)
}

/// [`group`] with another group limit (tests).
pub fn group_with_cap(
    engine: &Engine,
    view: &View,
    levels: &[Level],
    done: &AtomicU64,
    cap: usize,
) -> Result<(View, Groups)> {
    let table = engine.table();
    let ncols = table.columns.len();
    if levels.iter().any(|l| l.col >= ncols) {
        return Err(Error::Other("group column out of range".into()));
    }
    let mut out = Groups {
        levels: levels.to_vec(),
        ..Default::default()
    };
    if levels.is_empty() || view.is_empty() {
        return Ok((view.clone(), out));
    }
    let rows = &*view.rows;
    let m = rows.len() as usize;
    // Node of each row (bitmap order) at the current level.
    let mut node_of: Vec<u32> = vec![NONE; m];
    let mut tiers: Vec<Tier> = Vec::with_capacity(levels.len());
    let mut total = 0usize;
    for (li, lv) in levels.iter().enumerate() {
        let (codes, values) = level_codes(engine, rows, lv.col, done, cap)?;
        let mut map: FxHashMap<u64, u32> = FxHashMap::default();
        let mut nodes: Vec<(u32, u32, u64)> = Vec::new();
        for (i, &code) in codes.iter().enumerate() {
            if i % (1 << 20) == 0 && table.progress.cancelled() {
                return Err(Error::Cancelled);
            }
            let parent = node_of[i];
            let key = if li == 0 {
                code as u64
            } else {
                (parent as u64) << 32 | code as u64
            };
            let id = *map.entry(key).or_insert_with(|| {
                nodes.push((parent, code, 0));
                nodes.len() as u32 - 1
            });
            nodes[id as usize].2 += 1;
            node_of[i] = id;
        }
        total += nodes.len();
        if total > cap {
            return Err(too_many(table.columns.get(lv.col), cap));
        }
        let rank = value_ranks(&values, engine.kind(lv.col));
        out.values.push(values);
        tiers.push(Tier {
            nodes,
            rank,
            sort: lv.sort,
        });
    }
    // Children of every node of each level, in display order.
    let mut kids: Vec<Vec<Vec<u32>>> = Vec::with_capacity(tiers.len());
    let mut roots: Vec<u32> = (0..tiers[0].nodes.len() as u32).collect();
    roots.sort_by(|&a, &b| tiers[0].cmp(a, b));
    for l in 1..tiers.len() {
        let mut k: Vec<Vec<u32>> = vec![Vec::new(); tiers[l - 1].nodes.len()];
        for (i, n) in tiers[l].nodes.iter().enumerate() {
            k[n.0 as usize].push(i as u32);
        }
        for v in &mut k {
            v.sort_by(|&a, &b| tiers[l].cmp(a, b));
        }
        kids.push(k);
    }
    // Preorder walk: positions, subtree ends, leaf starts.
    let last = tiers.len() - 1;
    let mut leaf_start = vec![0u64; tiers[last].nodes.len()];
    let mut pos = 0u64;
    let mut stack: Vec<(u32, u32, u32, usize)> = Vec::new(); // (level, tier id, preorder, next kid)
    for &r in &roots {
        stack.push((0, r, push_node(&mut out, &tiers, 0, r, NONE, pos), 0));
        while let Some(&mut (l, id, pre, ref mut next)) = stack.last_mut() {
            if l as usize == last {
                leaf_start[id as usize] = pos;
                pos += tiers[last].nodes[id as usize].2;
                out.nodes[pre as usize].end = out.nodes.len() as u32;
                stack.pop();
                continue;
            }
            let ks = &kids[l as usize][id as usize];
            if *next < ks.len() {
                let k = ks[*next];
                *next += 1;
                let p = push_node(&mut out, &tiers, l + 1, k, pre, pos);
                stack.push((l + 1, k, p, 0));
            } else {
                out.nodes[pre as usize].end = out.nodes.len() as u32;
                stack.pop();
            }
        }
    }
    // Rows in group order, each leaf in the view's order.
    let mut next = leaf_start;
    let mut order = vec![0u32; m];
    match &view.order {
        None => {
            for (i, r) in rows.iter().enumerate() {
                let l = node_of[i] as usize;
                order[next[l] as usize] = r;
                next[l] += 1;
            }
        }
        Some(o) => {
            let max = rows.max().map_or(0, |x| x as usize + 1);
            let mut dense = vec![NONE; max];
            for (i, r) in rows.iter().enumerate() {
                dense[r as usize] = node_of[i];
            }
            drop(node_of);
            for &r in o.iter() {
                let l = dense[r as usize] as usize;
                order[next[l] as usize] = r;
                next[l] += 1;
            }
        }
    }
    Ok((
        View {
            rows: view.rows.clone(),
            order: Some(Arc::new(order)),
        },
        out,
    ))
}

fn push_node(out: &mut Groups, tiers: &[Tier], l: u32, id: u32, parent: u32, start: u64) -> u32 {
    let (_, code, count) = tiers[l as usize].nodes[id as usize];
    out.nodes.push(Node {
        depth: l,
        code,
        count,
        start,
        parent,
        end: 0,
    });
    out.nodes.len() as u32 - 1
}
