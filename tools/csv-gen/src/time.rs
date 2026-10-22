//! Civil time formatting from 100 ns ticks since the Unix epoch (UTC).
//! Self-contained (no date crate) so output never depends on a tz database.

pub const TICKS_PER_SEC: i64 = 10_000_000;

/// 2024-03-01T00:00:00Z
pub const T0: i64 = 1_709_251_200 * TICKS_PER_SEC;

pub struct Civil {
    pub y: i64,
    pub mo: u32,
    pub d: u32,
    pub h: u32,
    pub mi: u32,
    pub s: u32,
    /// 100 ns units, 0..10_000_000
    pub frac: u32,
}

/// Howard Hinnant's days-to-civil algorithm.
pub fn civil(ticks: i64) -> Civil {
    let secs = ticks.div_euclid(TICKS_PER_SEC);
    let frac = ticks.rem_euclid(TICKS_PER_SEC) as u32;
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400) as u32;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    Civil {
        y: if mo <= 2 { y + 1 } else { y },
        mo,
        d,
        h: sod / 3600,
        mi: sod / 60 % 60,
        s: sod % 60,
        frac,
    }
}

/// `yyyy-MM-dd HH:mm:ss.fffffff` (EvtxECmd / forhunt / MFTECmd).
pub fn evtxecmd(ticks: i64) -> String {
    let c = civil(ticks);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:07}",
        c.y, c.mo, c.d, c.h, c.mi, c.s, c.frac
    )
}

/// `yyyy-MM-dd HH:mm:ss.fff +00:00` (Hayabusa).
pub fn hayabusa(ticks: i64) -> String {
    let c = civil(ticks);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} +00:00",
        c.y,
        c.mo,
        c.d,
        c.h,
        c.mi,
        c.s,
        c.frac / 10_000
    )
}

/// `yyyy-MM-ddTHH:mm:ss.ffffffZ` (ISO 8601).
pub fn iso(ticks: i64) -> String {
    let c = civil(ticks);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}Z",
        c.y,
        c.mo,
        c.d,
        c.h,
        c.mi,
        c.s,
        c.frac / 10
    )
}

/// (`MM/dd/yyyy`, `HH:mm:ss`) as plaso's l2tcsv writes them.
pub fn l2t(ticks: i64) -> (String, String) {
    let c = civil(ticks);
    (
        format!("{:02}/{:02}/{:04}", c.mo, c.d, c.y),
        format!("{:02}:{:02}:{:02}", c.h, c.mi, c.s),
    )
}

/// `d/M/yyyy HH:mm` (European spreadsheet export).
pub fn eu(ticks: i64) -> String {
    let c = civil(ticks);
    format!("{}/{}/{:04} {:02}:{:02}", c.d, c.mo, c.y, c.h, c.mi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_dates() {
        assert_eq!(evtxecmd(T0), "2024-03-01 00:00:00.0000000");
        assert_eq!(evtxecmd(0), "1970-01-01 00:00:00.0000000");
        // 2024-02-29T23:59:59.1234567Z
        assert_eq!(evtxecmd(T0 - 8_765_433), "2024-02-29 23:59:59.1234567");
        assert_eq!(hayabusa(T0 + 5_000_000), "2024-03-01 00:00:00.500 +00:00");
        assert_eq!(iso(T0), "2024-03-01T00:00:00.000000Z");
        assert_eq!(eu(T0), "1/3/2024 00:00");
    }
}
