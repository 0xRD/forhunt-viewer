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

/// 1601-01-01 (FILETIME, WebKit) in ticks since 1970.
pub const EPOCH_1601: i64 = 116_444_736_000_000_000;

const MON: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const MOIS: [&str; 12] = [
    "janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.", "nov.",
    "déc.",
];
const WD: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];

fn weekday(ticks: i64) -> &'static str {
    WD[ticks.div_euclid(86_400 * TICKS_PER_SEC).rem_euclid(7) as usize]
}

/// `yyyy-MM-ddTHH:mm:ss.fff+HH:MM` at `off_min` east of UTC.
pub fn iso_offset(ticks: i64, off_min: i64) -> String {
    let c = civil(ticks + off_min * 60 * TICKS_PER_SEC);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}{}{:02}:{:02}",
        c.y,
        c.mo,
        c.d,
        c.h,
        c.mi,
        c.s,
        c.frac / 10_000,
        if off_min < 0 { '-' } else { '+' },
        off_min.abs() / 60,
        off_min.abs() % 60
    )
}

/// ISO 8601 basic format `yyyyMMddTHHmmssZ`.
pub fn iso_basic(ticks: i64) -> String {
    let c = civil(ticks);
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        c.y, c.mo, c.d, c.h, c.mi, c.s
    )
}

/// RFC 2822 / HTTP `Fri, 01 Mar 2024 12:34:56 GMT`.
pub fn rfc2822(ticks: i64) -> String {
    let c = civil(ticks);
    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
        weekday(ticks),
        c.d,
        MON[c.mo as usize - 1],
        c.y,
        c.h,
        c.mi,
        c.s
    )
}

/// `M/d/yyyy h:mm:ss AM` (US locale).
pub fn us_12h(ticks: i64) -> String {
    let c = civil(ticks);
    let h = if c.h.is_multiple_of(12) { 12 } else { c.h % 12 };
    let ap = if c.h < 12 { "AM" } else { "PM" };
    format!(
        "{}/{}/{:04} {h}:{:02}:{:02} {ap}",
        c.mo, c.d, c.y, c.mi, c.s
    )
}

/// `01-Mar-2024 12:34:56`.
pub fn dd_mon_yyyy(ticks: i64) -> String {
    let c = civil(ticks);
    format!(
        "{:02}-{}-{:04} {:02}:{:02}:{:02}",
        c.d,
        MON[c.mo as usize - 1],
        c.y,
        c.h,
        c.mi,
        c.s
    )
}

/// French `1 mars 2024 12:34:56`.
pub fn french(ticks: i64) -> String {
    let c = civil(ticks);
    format!(
        "{} {} {:04} {:02}:{:02}:{:02}",
        c.d,
        MOIS[c.mo as usize - 1],
        c.y,
        c.h,
        c.mi,
        c.s
    )
}

/// Syslog without a year: `Mar  1 12:34:56`.
pub fn syslog(ticks: i64) -> String {
    let c = civil(ticks);
    format!(
        "{} {:2} {:02}:{:02}:{:02}",
        MON[c.mo as usize - 1],
        c.d,
        c.h,
        c.mi,
        c.s
    )
}

/// Apache / CLF `[01/Mar/2024:13:34:56 +0100]` at `off_min` east of UTC.
pub fn clf(ticks: i64, off_min: i64) -> String {
    let c = civil(ticks + off_min * 60 * TICKS_PER_SEC);
    format!(
        "[{:02}/{}/{:04}:{:02}:{:02}:{:02} {}{:02}{:02}]",
        c.d,
        MON[c.mo as usize - 1],
        c.y,
        c.h,
        c.mi,
        c.s,
        if off_min < 0 { '-' } else { '+' },
        off_min.abs() / 60,
        off_min.abs() % 60
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
        let t1 = T0 + 45_296 * TICKS_PER_SEC;
        assert_eq!(iso_offset(t1, 120), "2024-03-01T14:34:56.000+02:00");
        assert_eq!(iso_basic(t1), "20240301T123456Z");
        assert_eq!(rfc2822(t1), "Fri, 01 Mar 2024 12:34:56 GMT");
        assert_eq!(us_12h(t1), "3/1/2024 12:34:56 PM");
        assert_eq!(dd_mon_yyyy(t1), "01-Mar-2024 12:34:56");
        assert_eq!(french(t1), "1 mars 2024 12:34:56");
        assert_eq!(syslog(t1), "Mar  1 12:34:56");
        assert_eq!(clf(t1, 60), "[01/Mar/2024:13:34:56 +0100]");
    }
}
