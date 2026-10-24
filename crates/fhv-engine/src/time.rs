//! Date-time parsing to `i64` ticks (100 ns since 1970-01-01T00:00:00Z),
//! the unit forhunt uses (FILETIME resolution).
//!
//! Accepted shapes (hand-written byte parser, no allocation):
//! - ISO 8601 / RFC 3339 and the space-separated form used by EvtxECmd,
//!   MFTECmd and forhunt: `yyyy-MM-dd[T| ]HH:mm[:ss[.f{1,9}]]` with an
//!   optional `Z`, `±HH:MM`, `±HHMM` or ` ±HH:MM` (Hayabusa) offset;
//!   `yyyy/MM/dd` works the same way; a date alone is midnight;
//! - slash or dot dates with the year last: `M/d/yyyy` (US), `d/M/yyyy`
//!   (European) and `d.M.yyyy`, optionally followed by `H:mm[:ss[.f]]` and
//!   `AM`/`PM`. Day/month order is a per-column decision (see
//!   [`Order`]); [`parse_any`] assumes US order for slashes, day first for dots.
//!
//! Times without an offset are taken as UTC (forensic tools write UTC).

pub const TICKS_PER_SEC: i64 = 10_000_000;

/// Field order of dates with the year last.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Order {
    /// Year first (ISO) or unknown.
    Ymd,
    /// Month/day/year.
    Mdy,
    /// Day/month/year.
    Dmy,
}

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

struct Cur<'a> {
    b: &'a [u8],
    i: usize,
}

impl Cur<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }
    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    /// 1..=max digits.
    fn num(&mut self, min: usize, max: usize) -> Option<u32> {
        let start = self.i;
        let mut v = 0u32;
        while self.i < self.b.len() && self.i - start < max && self.b[self.i].is_ascii_digit() {
            v = v * 10 + (self.b[self.i] - b'0') as u32;
            self.i += 1;
        }
        (self.i - start >= min).then_some(v)
    }
    fn done(&self) -> bool {
        self.i == self.b.len()
    }
}

fn trim(b: &[u8]) -> &[u8] {
    let s = b
        .iter()
        .position(|c| !c.is_ascii_whitespace())
        .unwrap_or(b.len());
    let e = b
        .iter()
        .rposition(|c| !c.is_ascii_whitespace())
        .map_or(s, |e| e + 1);
    &b[s..e]
}

/// Parses `HH:mm[:ss[.f]][ AM|PM][offset]` after a date; returns ticks of
/// the day plus the offset correction, the unit of the last written
/// component (a minute, a second, or 10^-k s for k fraction digits), and
/// whether an offset (`Z`, `±HH:MM`) was written.
fn time_part(c: &mut Cur<'_>, allow_ampm: bool) -> Option<(i64, i64, bool)> {
    let h = c.num(1, 2)?;
    if !c.eat(b':') {
        return None;
    }
    let mi = c.num(2, 2)?;
    let mut s = 0;
    let mut frac: i64 = 0;
    let mut unit = 60 * TICKS_PER_SEC;
    if c.eat(b':') {
        s = c.num(2, 2)?;
        unit = TICKS_PER_SEC;
        if c.eat(b'.') || c.eat(b',') {
            let start = c.i;
            let mut digits = 0;
            while let Some(d) = c.peek().filter(u8::is_ascii_digit) {
                if digits < 7 {
                    frac = frac * 10 + (d - b'0') as i64;
                    digits += 1;
                    unit /= 10;
                }
                c.i += 1;
            }
            if c.i == start {
                return None;
            }
            while digits < 7 {
                frac *= 10;
                digits += 1;
            }
        }
    }
    let mut h = h;
    if allow_ampm {
        let save = c.i;
        c.eat(b' ');
        let rest = &c.b[c.i..];
        if rest.len() >= 2 && rest[1].eq_ignore_ascii_case(&b'M') {
            let pm = match rest[0].to_ascii_uppercase() {
                b'A' => Some(false),
                b'P' => Some(true),
                _ => None,
            };
            if let Some(pm) = pm {
                if !(1..=12).contains(&h) {
                    return None;
                }
                h = h % 12 + if pm { 12 } else { 0 };
                c.i += 2;
            } else {
                c.i = save;
            }
        } else {
            c.i = save;
        }
    }
    if h > 23 || mi > 59 || s > 60 {
        return None;
    }
    let mut ticks = ((h * 3600 + mi * 60 + s.min(59)) as i64) * TICKS_PER_SEC + frac;
    // Offset.
    let save = c.i;
    let mut zoned = true;
    c.eat(b' ');
    match c.peek() {
        Some(b'Z') | Some(b'z') => {
            c.i += 1;
        }
        Some(sign @ (b'+' | b'-')) => {
            c.i += 1;
            let oh = c.num(2, 2)?;
            c.eat(b':');
            let om = c.num(2, 2).unwrap_or(0);
            if oh > 23 || om > 59 {
                return None;
            }
            let off = ((oh * 3600 + om * 60) as i64) * TICKS_PER_SEC;
            ticks -= if sign == b'+' { off } else { -off };
        }
        _ => {
            c.i = save;
            zoned = false;
        }
    }
    Some((ticks, unit, zoned))
}

fn date_ticks(y: i64, m: u32, d: u32) -> Option<i64> {
    if !(1..=12).contains(&m) || d == 0 || d > days_in_month(y, m) {
        return None;
    }
    Some(days_from_civil(y, m, d) * 86_400 * TICKS_PER_SEC)
}

/// Parses with a known day/month order for year-last dates.
pub fn parse(b: &[u8], order: Order) -> Option<i64> {
    parse_span(b, order).map(|(t, _)| t)
}

/// Like [`parse`], plus the length in ticks of the period the value names
/// at the precision it is written with: a day for a date alone, a minute
/// for `HH:mm`, a second for `HH:mm:ss`, 10^-k s for k fraction digits
/// (1 tick from 7 digits on). `2024-03-01` covers the whole day.
pub fn parse_span(b: &[u8], order: Order) -> Option<(i64, i64)> {
    parse_zoned(b, order).map(|(t, span, _)| (t, span))
}

/// Like [`parse_span`], plus whether the value carries an explicit offset
/// (`Z`, `+02:00`...). Without one the ticks are the wall-clock time read
/// as UTC (callers may reinterpret it in another zone).
pub fn parse_zoned(b: &[u8], order: Order) -> Option<(i64, i64, bool)> {
    const DAY: i64 = 86_400 * TICKS_PER_SEC;
    let b = trim(b);
    if b.len() < 6 {
        return None;
    }
    let mut c = Cur { b, i: 0 };
    // Year first?
    if b.len() >= 8 && b[..4].iter().all(u8::is_ascii_digit) && (b[4] == b'-' || b[4] == b'/') {
        let sep = b[4];
        let y = c.num(4, 4)? as i64;
        c.eat(sep);
        let m = c.num(1, 2)?;
        if !c.eat(sep) {
            return None;
        }
        let d = c.num(1, 2)?;
        let day = date_ticks(y, m, d)?;
        if c.done() {
            return Some((day, DAY, false));
        }
        if !(c.eat(b'T') || c.eat(b't') || c.eat(b' ')) {
            return None;
        }
        let (t, unit, zoned) = time_part(&mut c, false)?;
        return c.done().then_some((day + t, unit, zoned));
    }
    // Year last: a/b/yyyy or a.b.yyyy
    let a = c.num(1, 2)?;
    let sep = c.peek()?;
    if sep != b'/' && sep != b'.' && sep != b'-' {
        return None;
    }
    c.i += 1;
    let bnum = c.num(1, 2)?;
    if !c.eat(sep) {
        return None;
    }
    let y = c.num(4, 4)? as i64;
    let (m, d) = match (order, sep) {
        (Order::Dmy, _) | (Order::Ymd, b'.') => (bnum, a),
        _ => (a, bnum),
    };
    let day = date_ticks(y, m, d)?;
    if c.done() {
        return Some((day, DAY, false));
    }
    if !c.eat(b' ') {
        return None;
    }
    let (t, unit, zoned) = time_part(&mut c, true)?;
    c.done().then_some((day + t, unit, zoned))
}

/// Parses any accepted shape; slash dates are taken as US month/day.
pub fn parse_any(b: &[u8]) -> Option<i64> {
    parse(b, Order::Ymd)
}

/// For a year-last date, which of the first two numbers can only be a day
/// (> 12): `Some(Dmy)` / `Some(Mdy)`, `None` when ambiguous or not such a date.
pub fn order_hint(b: &[u8]) -> Option<Order> {
    let b = trim(b);
    let mut c = Cur { b, i: 0 };
    let a = c.num(1, 2)?;
    let sep = c.peek()?;
    if sep != b'/' && sep != b'.' && sep != b'-' {
        return None;
    }
    c.i += 1;
    let bn = c.num(1, 2)?;
    if !c.eat(sep) {
        return None;
    }
    c.num(4, 4)?;
    match (a > 12, bn > 12) {
        (true, false) => Some(Order::Dmy),
        (false, true) => Some(Order::Mdy),
        _ => None,
    }
}

/// Formats ticks as `yyyy-MM-dd HH:mm:ss.fffffff` shifted by `offset_secs`.
pub fn format(ticks: i64, offset_secs: i32) -> String {
    let t = ticks + offset_secs as i64 * TICKS_PER_SEC;
    let secs = t.div_euclid(TICKS_PER_SEC);
    let frac = t.rem_euclid(TICKS_PER_SEC);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}.{frac:07}",
        sod / 3600,
        sod / 60 % 60,
        sod % 60
    )
}

pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_709_251_200 * TICKS_PER_SEC; // 2024-03-01T00:00:00Z

    #[test]
    fn iso_and_tool_forms() {
        let p = |s: &str| parse_any(s.as_bytes());
        assert_eq!(p("2024-03-01 00:00:00.0000000"), Some(T0));
        assert_eq!(p("2024-03-01T00:00:00Z"), Some(T0));
        assert_eq!(p("2024-03-01"), Some(T0));
        assert_eq!(p("2024/03/01 00:00"), Some(T0));
        assert_eq!(p("2024-03-01T00:00:00.5Z"), Some(T0 + 5_000_000));
        assert_eq!(p("2024-03-01 00:00:00.123456789"), Some(T0 + 1_234_567));
        assert_eq!(p("2024-03-01T02:00:00+02:00"), Some(T0));
        assert_eq!(p("2024-03-01T02:00:00+0200"), Some(T0));
        assert_eq!(p("2024-02-29 22:00:00.000 -02:00"), Some(T0));
        assert_eq!(p("2024-03-01 00:00:00.000 +00:00"), Some(T0));
        assert_eq!(p(" 2024-03-01 "), Some(T0));
        assert_eq!(p("2023-02-29"), None);
        assert_eq!(p("2024-13-01"), None);
        assert_eq!(p("2024-03-01 25:00"), None);
        assert_eq!(p("2024-03-01x"), None);
        assert_eq!(p("12345"), None);
        assert_eq!(p("hello world"), None);
        assert_eq!(p("1970-01-01"), Some(0));
        assert_eq!(p("1601-01-01"), Some(-11_644_473_600 * TICKS_PER_SEC));
    }

    #[test]
    fn spans_follow_the_written_precision() {
        let p = |s: &str| parse_span(s.as_bytes(), Order::Ymd).map(|(_, u)| u);
        assert_eq!(p("2024-03-01"), Some(86_400 * TICKS_PER_SEC));
        assert_eq!(p("2024-03-01T10:00Z"), Some(60 * TICKS_PER_SEC));
        assert_eq!(p("2024-03-01 10:00:00"), Some(TICKS_PER_SEC));
        assert_eq!(p("2024-03-01 10:00:00.5 +02:00"), Some(TICKS_PER_SEC / 10));
        assert_eq!(p("2024-03-01 10:00:00.1234567"), Some(1));
        assert_eq!(p("2024-03-01 10:00:00.123456789"), Some(1));
        assert_eq!(p("3/1/2024 1:00 PM"), Some(60 * TICKS_PER_SEC));
        assert_eq!(p("01.03.2024"), Some(86_400 * TICKS_PER_SEC));
    }

    #[test]
    fn year_last() {
        assert_eq!(parse(b"03/01/2024", Order::Mdy), Some(T0));
        assert_eq!(parse(b"01/03/2024", Order::Dmy), Some(T0));
        assert_eq!(parse(b"1/3/2024 00:00", Order::Dmy), Some(T0));
        assert_eq!(parse(b"01.03.2024 00:00:00", Order::Ymd), Some(T0));
        assert_eq!(
            parse(b"2/29/2024 11:00 PM", Order::Mdy),
            Some(T0 - 3600 * TICKS_PER_SEC)
        );
        assert_eq!(parse(b"3/1/2024 12:00:00 AM", Order::Mdy), Some(T0));
        assert_eq!(order_hint(b"25/12/2024"), Some(Order::Dmy));
        assert_eq!(order_hint(b"12/25/2024"), Some(Order::Mdy));
        assert_eq!(order_hint(b"1/2/2024"), None);
    }

    #[test]
    fn format_roundtrip() {
        assert_eq!(format(T0 + 1_234_567, 0), "2024-03-01 00:00:00.1234567");
        assert_eq!(format(T0, 3600), "2024-03-01 01:00:00.0000000");
        assert_eq!(format(T0, -3600), "2024-02-29 23:00:00.0000000");
        for s in ["1999-12-31 23:59:59.9999999", "2024-02-29 12:34:56.0000001"] {
            assert_eq!(format(parse_any(s.as_bytes()).unwrap(), 0), s);
        }
    }
}
