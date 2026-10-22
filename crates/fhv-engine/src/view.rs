//! A view of a table: the rows a filter keeps, in sort order.

use std::sync::Arc;

use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};

use crate::Result;
use crate::filter::{Engine, Expr};
use crate::sort::{SortKey, sort};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Query {
    pub filter: Option<Expr>,
    pub sort: Vec<SortKey>,
}

#[derive(Clone, Debug, Default)]
pub struct View {
    /// Data rows kept by the filter.
    pub rows: Arc<RoaringBitmap>,
    /// Display order when sorted (else ascending row order).
    pub order: Option<Arc<Vec<u32>>>,
}

impl View {
    pub fn all(n: u64) -> View {
        let mut b = RoaringBitmap::new();
        b.insert_range(0..n as u32);
        View {
            rows: Arc::new(b),
            order: None,
        }
    }

    pub fn len(&self) -> u64 {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Data row shown at display position `k`.
    pub fn row_at(&self, k: u64) -> Option<u32> {
        match &self.order {
            Some(o) => o.get(k as usize).copied(),
            None => self.rows.select(k as u32),
        }
    }

    /// Display position of data row `r`, if it is in the view.
    pub fn position_of(&self, r: u32) -> Option<u64> {
        if !self.rows.contains(r) {
            return None;
        }
        match &self.order {
            Some(o) => o.iter().position(|&x| x == r).map(|p| p as u64),
            None => Some(self.rows.rank(r) - 1),
        }
    }

    /// Rows in display order.
    pub fn iter(&self) -> Box<dyn Iterator<Item = u32> + '_> {
        match &self.order {
            Some(o) => Box::new(o.iter().copied()),
            None => Box::new(self.rows.iter()),
        }
    }
}

/// Evaluates a query.
pub fn run(engine: &Engine, q: &Query) -> Result<View> {
    let n = engine.table().row_count();
    let rows = match &q.filter {
        Some(f) => engine.eval(f)?,
        None => {
            let mut b = RoaringBitmap::new();
            b.insert_range(0..n as u32);
            b
        }
    };
    let order = if q.sort.is_empty() {
        None
    } else {
        Some(Arc::new(sort(engine.table(), &rows, &q.sort)?))
    };
    Ok(View {
        rows: Arc::new(rows),
        order,
    })
}
