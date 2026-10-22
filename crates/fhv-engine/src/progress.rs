//! Progress and cancellation shared between a worker and the UI.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

#[derive(Default)]
pub struct Progress {
    pub phase: AtomicU8,
    /// Bytes processed in the current phase.
    pub bytes: AtomicU64,
    /// Bytes to process in the current phase.
    pub total: AtomicU64,
    /// Rows found so far.
    pub rows: AtomicU64,
    cancel: AtomicBool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    Idle = 0,
    Transcoding = 1,
    Indexing = 2,
    Columns = 3,
    Done = 4,
    Failed = 5,
}

impl Progress {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub fn set_phase(&self, p: Phase, total: u64) {
        self.bytes.store(0, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
        self.phase.store(p as u8, Ordering::Release);
    }

    pub fn phase(&self) -> Phase {
        match self.phase.load(Ordering::Acquire) {
            1 => Phase::Transcoding,
            2 => Phase::Indexing,
            3 => Phase::Columns,
            4 => Phase::Done,
            5 => Phase::Failed,
            _ => Phase::Idle,
        }
    }

    /// Fraction of the current phase, 0..=1.
    pub fn fraction(&self) -> f32 {
        let t = self.total.load(Ordering::Relaxed);
        if t == 0 {
            return 0.0;
        }
        (self.bytes.load(Ordering::Relaxed) as f64 / t as f64).min(1.0) as f32
    }
}
