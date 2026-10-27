//! Date-time parsing to `i64` ticks (100 ns since 1970-01-01T00:00:00Z),
//! the unit forhunt uses (FILETIME resolution).
//!
//! A time column has a [`Fmt`]: a [`Shape`] (date text, or a number in some
//! epoch unit, or a custom `strftime`-style pattern), the day/month order of
//! year-last dates, the year of year-less dates and the zone of values
//! written without an offset. The full list of accepted shapes is in
//! docs/DATE-FORMATS.md. Date text (hand-written byte parser):
//! - ISO 8601 / RFC 3339 and the space-separated form used by EvtxECmd,
//!   MFTECmd and forhunt: `yyyy-MM-dd[T| ]HH:mm[:ss[.f{1,9}]]` with an
//!   optional `Z`, `±HH[:MM]`, ` ±HH:MM` (Hayabusa) or ` UTC` offset;
//!   `yyyy/MM/dd` and `yyyy.MM.dd` work the same way; a date alone is
//!   midnight; basic format `yyyyMMddTHHmm[ss[.f]][offset]`;
//! - slash, dot or dash dates with the year last: `M/d/yyyy` (US),
//!   `d/M/yyyy` (European) and `d.M.yyyy`, optionally followed by
//!   `H:mm[:ss[.f]]` and `AM`/`PM`. Day/month order is a per-column decision
//!   (see [`Order`]); [`parse_any`] assumes US order for slashes, day first
//!   for dots;
//! - month names (English and French, full or abbreviated) in any order
//!   with the day and year: RFC 2822 (`Fri, 01 Mar 2024 12:34:56 GMT`),
//!   `01-Mar-2024`, `Mar 1, 2024 1:00 PM`, `1 mars 2024`, asctime
//!   (`Fri Mar  1 12:34:56 2024`), Apache CLF (`01/Mar/2024:12:34:56 +0100`);
//! - year-less syslog (`Mar  1 12:34:56`) and `MM-dd HH:mm:ss` when the
//!   format has a year ([`Fmt::year`]).
//!
//! Times without an offset are taken as UTC (forensic tools write UTC)
//! unless the format names a zone.

use std::sync::Mutex;

pub const TICKS_PER_SEC: i64 = 10_000_000;
const DAY: i64 = 86_400 * TICKS_PER_SEC;
/// 1601-01-01 (FILETIME / WebKit epoch) in ticks since 1970.
const EPOCH_1601: i64 = 116_444_736_000_000_000;
/// 0001-01-01 (.NET DateTime ticks) in ticks since 1970.
const EPOCH_0001: i64 = 621_355_968_000_000_000;

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

/// How the cells of a time column are written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// Date text (ISO, RFC 2822, numeric, month names, CLF, syslog...).
    Text,
    EpochS,
    EpochMs,
    EpochUs,
    EpochNs,
    EpochDays,
    /// 100 ns since 1601-01-01.
    FileTime,
    /// .NET `DateTime.Ticks`: 100 ns since 0001-01-01.
    Ticks,
    /// Days since 1899-12-30 (Excel / OLE automation date).
    Excel,
    /// Chrome / WebKit: microseconds since 1601-01-01.
    WebKit,
    /// Mac / Cocoa absolute time: seconds since 2001-01-01.
    Cocoa,
    Custom(&'static Pattern),
}

impl Shape {
    /// Presets offered in the column menu (spec name, label).
    pub const PRESETS: &[(Shape, &str)] = &[
        (
            Shape::Text,
            "Date text (ISO 8601, RFC 2822, 1 Mar 2024, ...)",
        ),
        (Shape::EpochS, "Unix seconds (1709251200[.5])"),
        (Shape::EpochMs, "Unix milliseconds"),
        (Shape::EpochUs, "Unix microseconds"),
        (Shape::EpochNs, "Unix nanoseconds"),
        (Shape::EpochDays, "Unix days"),
        (Shape::FileTime, "Windows FILETIME (100 ns since 1601)"),
        (Shape::Ticks, ".NET ticks (100 ns since 0001)"),
        (Shape::Excel, "Excel serial date (days since 1899-12-30)"),
        (Shape::WebKit, "Chrome / WebKit (µs since 1601)"),
        (Shape::Cocoa, "Mac / Cocoa absolute time (s since 2001)"),
    ];

    fn name(self) -> &'static str {
        match self {
            Shape::Text => "text",
            Shape::EpochS => "epoch-s",
            Shape::EpochMs => "epoch-ms",
            Shape::EpochUs => "epoch-us",
            Shape::EpochNs => "epoch-ns",
            Shape::EpochDays => "epoch-days",
            Shape::FileTime => "filetime",
            Shape::Ticks => "dotnet-ticks",
            Shape::Excel => "excel",
            Shape::WebKit => "webkit",
            Shape::Cocoa => "cocoa",
            Shape::Custom(_) => "custom",
        }
    }

    /// Ticks per unit as a fraction, and the tick of value 0.
    fn unit(self) -> (i128, i128, i64) {
        match self {
            Shape::EpochS => (TICKS_PER_SEC as i128, 1, 0),
            Shape::EpochMs => (10_000, 1, 0),
            Shape::EpochUs => (10, 1, 0),
            Shape::EpochNs => (1, 100, 0),
            Shape::EpochDays => (DAY as i128, 1, 0),
            Shape::FileTime => (1, 1, -EPOCH_1601),
            Shape::Ticks => (1, 1, -EPOCH_0001),
            Shape::Excel => (DAY as i128, 1, -25_569 * DAY),
            Shape::WebKit => (10, 1, -EPOCH_1601),
            Shape::Cocoa => (TICKS_PER_SEC as i128, 1, 978_307_200 * TICKS_PER_SEC),
            Shape::Text | Shape::Custom(_) => (1, 1, 0),
        }
    }

    pub fn is_number(self) -> bool {
        !matches!(self, Shape::Text | Shape::Custom(_))
    }
}

/// Which kind of date text a column holds (for display; parsing accepts
/// every kind).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hint {
    Iso,
    IsoBasic,
    Numeric,
    Rfc2822,
    Clf,
    Textual,
    YearLess,
    Mixed,
}

impl Hint {
    const ALL: [(Hint, &str); 8] = [
        (Hint::Iso, "iso"),
        (Hint::IsoBasic, "iso-basic"),
        (Hint::Numeric, "numeric"),
        (Hint::Rfc2822, "rfc2822"),
        (Hint::Clf, "clf"),
        (Hint::Textual, "textual"),
        (Hint::YearLess, "yearless"),
        (Hint::Mixed, "mixed"),
    ];
    fn name(self) -> &'static str {
        Hint::ALL
            .iter()
            .find(|h| h.0 == self)
            .map_or("iso", |h| h.1)
    }
}

/// The format of a time column. `Copy`: custom patterns and zones are
/// interned (a few per session, kept for the life of the process).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fmt {
    pub shape: Shape,
    pub order: Order,
    /// Detected kind of text (display only).
    pub hint: Hint,
    /// Year of year-less dates (0: year-less dates do not parse).
    pub year: i32,
    /// Months after this one are in the year before `year` (syslog files
    /// that span New Year: `year` is the year of the file's last write).
    /// 12 = no rollover.
    pub roll: u8,
    /// Zone of values written without an offset (None = UTC).
    pub zone: Option<&'static Zone>,
}

impl Default for Fmt {
    fn default() -> Self {
        Fmt {
            shape: Shape::Text,
            order: Order::Ymd,
            hint: Hint::Iso,
            year: 0,
            roll: 12,
            zone: None,
        }
    }
}

impl From<Order> for Fmt {
    fn from(order: Order) -> Fmt {
        Fmt {
            order,
            ..Fmt::default()
        }
    }
}

impl Fmt {
    pub fn shape(shape: Shape) -> Fmt {
        Fmt {
            shape,
            ..Fmt::default()
        }
    }

    /// The year is an assumption (year-less values).
    pub fn year_assumed(&self) -> bool {
        self.year != 0 && !self.shape.is_number()
    }

    /// Cells do not show the instant as written in the file (a number, an
    /// assumed zone or year, a custom pattern): the grid shows the parsed
    /// time instead.
    pub fn rewrites(&self) -> bool {
        self.shape != Shape::Text || self.zone.is_some() || self.year_assumed()
    }

    /// Short description for the column menu and header tooltip.
    pub fn describe(&self) -> String {
        let mut s = match self.shape {
            Shape::Text => match self.hint {
                Hint::Iso => "ISO 8601 (2024-03-01 12:34:56)".to_string(),
                Hint::IsoBasic => "ISO 8601 basic (20240301T123456Z)".into(),
                Hint::Rfc2822 => "RFC 2822 / HTTP (Fri, 01 Mar 2024 12:34:56 GMT)".into(),
                Hint::Clf => "Apache / CLF (01/Mar/2024:12:34:56 +0100)".into(),
                Hint::Textual => "month names (1 Mar 2024, Mar 1 2024)".into(),
                Hint::YearLess => "syslog, no year (Mar  1 12:34:56)".into(),
                Hint::Numeric | Hint::Mixed => {
                    let what = if self.hint == Hint::Mixed {
                        "mixed date text"
                    } else {
                        "numeric dates"
                    };
                    match self.order {
                        Order::Dmy => format!("{what}, day first (31/12/2024)"),
                        Order::Mdy => format!("{what}, month first (12/31/2024)"),
                        Order::Ymd => format!("{what}, month first if unsure (12/31/2024)"),
                    }
                }
            },
            Shape::Custom(p) => format!("custom: {}", p.src),
            s => Shape::PRESETS
                .iter()
                .find(|p| p.0 == s)
                .map_or_else(String::new, |p| p.1.to_string()),
        };
        if self.year_assumed() {
            s.push_str(&format!("; year assumed: {}", self.year));
            if self.roll < 12 {
                s.push_str(&format!(
                    " (months after {} in {})",
                    self.roll,
                    self.year - 1
                ));
            }
        }
        if let Some(z) = self.zone {
            s.push_str(&format!("; no offset = {}", z.name));
        }
        s
    }

    /// Serialized form (sessions, settings, index cache):
    /// `shape[;order=..][;hint=..][;year=Y[/roll]][;tz=..][;pattern=..]`.
    pub fn spec(&self) -> String {
        let mut s = self.shape.name().to_string();
        match self.order {
            Order::Ymd => {}
            Order::Mdy => s.push_str(";order=mdy"),
            Order::Dmy => s.push_str(";order=dmy"),
        }
        if self.hint != Hint::Iso {
            s.push_str(";hint=");
            s.push_str(self.hint.name());
        }
        if self.year != 0 {
            s.push_str(&format!(";year={}", self.year));
            if self.roll < 12 {
                s.push_str(&format!("/{}", self.roll));
            }
        }
        if let Some(z) = self.zone {
            s.push_str(";tz=");
            s.push_str(&z.name);
        }
        if let Shape::Custom(p) = self.shape {
            s.push_str(";pattern=");
            s.push_str(&p.src);
        }
        s
    }

    pub fn from_spec(spec: &str) -> Result<Fmt, String> {
        let (head, pattern) = match spec.split_once(";pattern=") {
            Some((h, p)) => (h, Some(p)),
            None => (spec, None),
        };
        let mut parts = head.split(';');
        let name = parts.next().unwrap_or_default();
        let shape = match name {
            "custom" => Shape::Custom(Pattern::intern(pattern.ok_or("custom: no pattern")?)?),
            n => Shape::PRESETS
                .iter()
                .map(|p| p.0)
                .find(|s| s.name() == n)
                .ok_or_else(|| format!("unknown date format {n:?}"))?,
        };
        let mut f = Fmt::shape(shape);
        for kv in parts {
            let (k, v) = kv.split_once('=').ok_or_else(|| format!("bad {kv:?}"))?;
            match k {
                "order" => {
                    f.order = match v {
                        "mdy" => Order::Mdy,
                        "dmy" => Order::Dmy,
                        _ => Order::Ymd,
                    }
                }
                "hint" => {
                    f.hint = Hint::ALL
                        .iter()
                        .find(|h| h.1 == v)
                        .map_or(Hint::Iso, |h| h.0)
                }
                "year" => {
                    let (y, r) = v.split_once('/').unwrap_or((v, "12"));
                    f.year = y.parse().map_err(|_| format!("bad year {v:?}"))?;
                    f.roll = r.parse().map_err(|_| format!("bad year {v:?}"))?;
                }
                "tz" => f.zone = Zone::intern(v)?,
                _ => {}
            }
        }
        Ok(f)
    }
}

/// A time zone for values without an offset (an IANA name or a fixed
/// `±HH:MM` offset).
#[derive(Debug)]
pub struct Zone {
    pub name: String,
    tz: jiff::tz::TimeZone,
}

impl PartialEq for Zone {
    fn eq(&self, o: &Zone) -> bool {
        self.name == o.name
    }
}
impl Eq for Zone {}

static ZONES: Mutex<Vec<&'static Zone>> = Mutex::new(Vec::new());
static PATTERNS: Mutex<Vec<&'static Pattern>> = Mutex::new(Vec::new());

impl Zone {
    /// `UTC` (or empty) is None.
    pub fn intern(name: &str) -> Result<Option<&'static Zone>, String> {
        let name = name.trim();
        if name.is_empty() || name.eq_ignore_ascii_case("utc") || name == "Z" {
            return Ok(None);
        }
        let mut zs = ZONES.lock().map_err(|_| "lock")?;
        if let Some(z) = zs.iter().find(|z| z.name == name) {
            return Ok(Some(z));
        }
        let tz = match fixed_offset(name.as_bytes()) {
            Some(secs) => jiff::tz::Offset::from_seconds(secs)
                .map(jiff::tz::TimeZone::fixed)
                .map_err(|e| e.to_string())?,
            None => jiff::tz::TimeZone::get(name).map_err(|_| format!("unknown zone {name:?}"))?,
        };
        let z: &'static Zone = Box::leak(Box::new(Zone {
            name: name.to_string(),
            tz,
        }));
        zs.push(z);
        Ok(Some(z))
    }

    /// Wall-clock ticks read in this zone, as UTC ticks (the earlier
    /// instant of a repeated time, the instant after a skipped one).
    pub fn to_utc(&self, wall: i64) -> Option<i64> {
        let secs = wall.div_euclid(TICKS_PER_SEC);
        let frac = wall.rem_euclid(TICKS_PER_SEC);
        let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
        let sod = secs.rem_euclid(86_400);
        let dt = jiff::civil::DateTime::new(
            i16::try_from(y).ok()?,
            m as i8,
            d as i8,
            (sod / 3600) as i8,
            (sod / 60 % 60) as i8,
            (sod % 60) as i8,
            0,
        )
        .ok()?;
        let ts = self.tz.to_ambiguous_timestamp(dt).compatible().ok()?;
        Some(ts.as_second() * TICKS_PER_SEC + frac)
    }
}

/// `+02:00`, `-0530`, `+2`, `UTC+1` as seconds east of UTC.
fn fixed_offset(b: &[u8]) -> Option<i32> {
    let b = b
        .strip_prefix(b"UTC")
        .or_else(|| b.strip_prefix(b"GMT"))
        .unwrap_or(b);
    let (sign, rest) = match b.first()? {
        b'+' => (1, &b[1..]),
        b'-' => (-1, &b[1..]),
        _ => return None,
    };
    let digits: Vec<u8> = rest.iter().copied().filter(|&c| c != b':').collect();
    if digits.is_empty() || digits.len() > 4 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let n: i32 = std::str::from_utf8(&digits).ok()?.parse().ok()?;
    let (h, m) = if digits.len() <= 2 {
        (n, 0)
    } else {
        (n / 100, n % 100)
    };
    (h <= 23 && m <= 59).then_some(sign * (h * 3600 + m * 60))
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
    /// min..=max digits.
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
    /// A word of letters (ASCII or UTF-8), lowercased into `buf`.
    fn word<'b>(&mut self, buf: &'b mut [u8; 16]) -> &'b [u8] {
        let start = self.i;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphabetic() || c >= 0x80)
        {
            self.i += 1;
        }
        let w = &self.b[start..self.i];
        let n = w.len().min(16);
        for (d, s) in buf.iter_mut().zip(&w[..n]) {
            *d = s.to_ascii_lowercase();
        }
        // UTF-8 uppercase É / Û / Ô / Ç (C3 89 / 9B / 94 / 87) -> lowercase.
        for k in 1..n {
            if buf[k - 1] == 0xC3 && (0x80..=0x9E).contains(&buf[k]) {
                buf[k] += 0x20;
            }
        }
        if w.len() > 16 { &buf[..0] } else { &buf[..n] }
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

/// Month of an English or French month name (full or abbreviated).
fn month(w: &[u8]) -> Option<u32> {
    const NAMES: &[(&[u8], u32)] = &[
        (b"jan", 1),
        (b"january", 1),
        (b"janv", 1),
        (b"janvier", 1),
        (b"feb", 2),
        (b"february", 2),
        ("fév".as_bytes(), 2),
        ("févr".as_bytes(), 2),
        ("février".as_bytes(), 2),
        (b"fev", 2),
        (b"fevr", 2),
        (b"fevrier", 2),
        (b"mar", 3),
        (b"march", 3),
        (b"mars", 3),
        (b"apr", 4),
        (b"april", 4),
        (b"avr", 4),
        (b"avril", 4),
        (b"may", 5),
        (b"mai", 5),
        (b"jun", 6),
        (b"june", 6),
        (b"juin", 6),
        (b"jul", 7),
        (b"july", 7),
        (b"juil", 7),
        (b"juillet", 7),
        (b"aug", 8),
        (b"august", 8),
        ("août".as_bytes(), 8),
        (b"aout", 8),
        ("aoû".as_bytes(), 8),
        (b"sep", 9),
        (b"sept", 9),
        (b"september", 9),
        (b"septembre", 9),
        (b"oct", 10),
        (b"october", 10),
        (b"octobre", 10),
        (b"nov", 11),
        (b"november", 11),
        (b"novembre", 11),
        (b"dec", 12),
        (b"december", 12),
        ("déc".as_bytes(), 12),
        ("décembre".as_bytes(), 12),
        (b"decembre", 12),
    ];
    NAMES.iter().find(|n| n.0 == w).map(|n| n.1)
}

fn weekday(w: &[u8]) -> bool {
    const NAMES: &[&[u8]] = &[
        b"mon",
        b"monday",
        b"tue",
        b"tues",
        b"tuesday",
        b"wed",
        b"wednesday",
        b"thu",
        b"thur",
        b"thurs",
        b"thursday",
        b"fri",
        b"friday",
        b"sat",
        b"saturday",
        b"sun",
        b"sunday",
        b"lun",
        b"lundi",
        b"mardi",
        b"mer",
        b"mercredi",
        b"jeu",
        b"jeudi",
        b"ven",
        b"vendredi",
        b"sam",
        b"samedi",
        b"dim",
        b"dimanche",
    ];
    NAMES.contains(&w)
}

/// Offset in minutes of a zone abbreviation.
fn zone_word(w: &[u8]) -> Option<i32> {
    Some(match w {
        b"z" | b"utc" | b"gmt" | b"ut" => 0,
        b"est" => -300,
        b"edt" => -240,
        b"cst" => -360,
        b"cdt" => -300,
        b"mst" => -420,
        b"mdt" => -360,
        b"pst" => -480,
        b"pdt" => -420,
        b"cet" => 60,
        b"cest" => 120,
        _ => return None,
    })
}

/// Parses `HH:mm[:ss[.f]][ AM|PM][offset]` (or `HHmm[ss[.f]][offset]`
/// when `basic`) after a date; returns ticks of the day minus the offset,
/// the unit of the last written component (a minute, a second, or 10^-k s
/// for k fraction digits), and whether an offset (`Z`, `±HH:MM`, `UTC`)
/// was written.
fn time_part(c: &mut Cur<'_>, allow_ampm: bool, basic: bool) -> Option<(i64, i64, bool)> {
    let h = if basic { c.num(2, 2)? } else { c.num(1, 2)? };
    if !basic && !c.eat(b':') {
        return None;
    }
    let mi = c.num(2, 2)?;
    let mut s = 0;
    let mut frac: i64 = 0;
    let mut unit = 60 * TICKS_PER_SEC;
    let has_sec = if basic {
        c.peek().is_some_and(|d| d.is_ascii_digit())
    } else {
        c.eat(b':')
    };
    if has_sec {
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
        let pm = match rest {
            [a, m, ..] if m.eq_ignore_ascii_case(&b'M') => match a.to_ascii_uppercase() {
                b'A' => Some(false),
                b'P' => Some(true),
                _ => None,
            },
            _ => None,
        };
        // "AM" must end the word ("Mar" after a time is not "M...").
        let ends = rest.get(2).is_none_or(|x| !x.is_ascii_alphabetic());
        match pm {
            Some(pm) if ends => {
                if !(1..=12).contains(&h) {
                    return None;
                }
                h = h % 12 + if pm { 12 } else { 0 };
                c.i += 2;
            }
            _ => c.i = save,
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
        Some(x) if x.is_ascii_alphabetic() => {
            let mut buf = [0u8; 16];
            match zone_word(c.word(&mut buf)) {
                Some(m) => ticks -= m as i64 * 60 * TICKS_PER_SEC,
                None => {
                    c.i = save;
                    zoned = false;
                }
            }
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
    Some(days_from_civil(y, m, d) * DAY)
}

/// Year of a year-less date in month `m`.
fn assumed_year(f: &Fmt, m: u32) -> Option<i64> {
    (f.year != 0).then(|| f.year as i64 - (m > f.roll as u32) as i64)
}

/// Parses with a known format (or day/month order for year-last dates).
pub fn parse(b: &[u8], f: impl Into<Fmt>) -> Option<i64> {
    parse_zoned(b, f).map(|(t, _, _)| t)
}

/// Like [`parse`], plus the length in ticks of the period the value names
/// at the precision it is written with: a day for a date alone, a minute
/// for `HH:mm`, a second for `HH:mm:ss`, 10^-k s for k fraction digits
/// (1 tick from 7 digits on). `2024-03-01` covers the whole day.
pub fn parse_span(b: &[u8], f: impl Into<Fmt>) -> Option<(i64, i64)> {
    parse_zoned(b, f).map(|(t, span, _)| (t, span))
}

/// Like [`parse_span`], plus whether the value names an instant (an
/// explicit offset such as `Z` or `+02:00`, an epoch number, or a time read
/// in the format's zone). Otherwise the ticks are the wall-clock time read
/// as UTC (callers may reinterpret it in another zone).
pub fn parse_zoned(b: &[u8], f: impl Into<Fmt>) -> Option<(i64, i64, bool)> {
    let f = f.into();
    let (t, span, zoned) = match f.shape {
        Shape::Text => parse_text(b, &f).map(|(t, s, z, _)| (t, s, z))?,
        Shape::Custom(p) => p.parse(b, &f)?,
        s => parse_number(b, s).map(|(t, span)| (t, span, true))?,
    };
    match f.zone {
        Some(z) if !zoned => Some((z.to_utc(t)?, span, true)),
        _ => Some((t, span, zoned)),
    }
}

/// A range-filter operand on a column of format `f`: date text (with the
/// column's day/month order and year, without its zone: operands are in
/// the display zone, D-038), else a value written like the column's cells.
pub fn parse_operand(b: &[u8], f: Fmt) -> Option<(i64, i64, bool)> {
    let f = Fmt { zone: None, ..f };
    parse_zoned(
        b,
        Fmt {
            shape: Shape::Text,
            ..f
        },
    )
    .or_else(|| parse_zoned(b, f))
}

/// Date text, with the kind of text it was.
fn parse_text(b: &[u8], f: &Fmt) -> Option<(i64, i64, bool, Hint)> {
    let mut b = trim(b);
    if let [b'[', inner @ .., b']'] = b {
        b = inner;
    }
    if b.len() < 6 || b.len() > 64 {
        return None;
    }
    let c0 = b[0];
    if c0.is_ascii_digit() {
        if let Some(r) = iso(b) {
            return Some(r);
        }
        if let Some(r) = year_last(b, f) {
            return Some(r);
        }
    } else if !(c0.is_ascii_alphabetic() || c0 >= 0x80) {
        return None;
    }
    textual(b, f)
}

/// Year-first dates (`yyyy-MM-dd...`, `yyyy/MM/dd`, `yyyy.MM.dd`) and the
/// ISO basic format.
fn iso(b: &[u8]) -> Option<(i64, i64, bool, Hint)> {
    let mut c = Cur { b, i: 0 };
    if b.len() >= 8 && b[..4].iter().all(u8::is_ascii_digit) && matches!(b[4], b'-' | b'/' | b'.') {
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
            return Some((day, DAY, false, Hint::Iso));
        }
        if !(c.eat(b'T') || c.eat(b't') || c.eat(b' ')) {
            return None;
        }
        let (t, unit, zoned) = time_part(&mut c, false, false)?;
        return c.done().then_some((day + t, unit, zoned, Hint::Iso));
    }
    // Basic: yyyyMMddTHHmm[ss[.f]][offset].
    if b.len() >= 13 && b[..8].iter().all(u8::is_ascii_digit) && matches!(b[8], b'T' | b't') {
        let y = c.num(4, 4)? as i64;
        let m = c.num(2, 2)?;
        let d = c.num(2, 2)?;
        let day = date_ticks(y, m, d)?;
        c.i += 1;
        let (t, unit, zoned) = time_part(&mut c, false, true)?;
        return c.done().then_some((day + t, unit, zoned, Hint::IsoBasic));
    }
    None
}

/// `a/b/yyyy`, `a.b.yyyy`, `a-b-yyyy` [time]; `MM-dd HH:mm:ss` with a year.
fn year_last(b: &[u8], f: &Fmt) -> Option<(i64, i64, bool, Hint)> {
    let mut c = Cur { b, i: 0 };
    let a = c.num(1, 2)?;
    let sep = c.peek()?;
    if sep != b'/' && sep != b'.' && sep != b'-' {
        return None;
    }
    c.i += 1;
    let bnum = c.num(1, 2)?;
    let (m, d) = match (f.order, sep) {
        (Order::Dmy, _) | (Order::Ymd, b'.') => (bnum, a),
        _ => (a, bnum),
    };
    if c.peek() == Some(b' ') && sep != b'.' {
        // Year-less `MM-dd HH:mm:ss` (Android logcat...).
        c.i += 1;
        let y = assumed_year(f, m)?;
        let day = date_ticks(y, m, d)?;
        let (t, unit, zoned) = time_part(&mut c, true, false)?;
        return c.done().then_some((day + t, unit, zoned, Hint::YearLess));
    }
    if !c.eat(sep) {
        return None;
    }
    let y = c.num(4, 4)? as i64;
    let day = date_ticks(y, m, d)?;
    if c.done() {
        return Some((day, DAY, false, Hint::Numeric));
    }
    c.eat(b',');
    if !(c.eat(b' ') || c.eat(b'T')) {
        return None;
    }
    let (t, unit, zoned) = time_part(&mut c, true, false)?;
    c.done().then_some((day + t, unit, zoned, Hint::Numeric))
}

/// Dates with a month name, in any common order: RFC 2822, asctime,
/// `01-Mar-2024`, `Mar 1, 2024`, `1er mars 2024`, CLF, syslog (year-less).
fn textual(b: &[u8], f: &Fmt) -> Option<(i64, i64, bool, Hint)> {
    let mut c = Cur { b, i: 0 };
    let (mut y, mut m, mut d) = (None::<i64>, None::<u32>, None::<u32>);
    let mut time: Option<(i64, i64, bool)> = None;
    let mut month_at0 = false;
    let mut weekday_comma = false;
    let mut clf = false;
    let mut ntok = 0;
    let mut buf = [0u8; 16];
    loop {
        while matches!(c.peek(), Some(b' ' | b',' | b'-' | b'/' | b'.')) {
            if c.peek() == Some(b',') && ntok == 1 && m.is_none() {
                weekday_comma = true;
            }
            c.i += 1;
        }
        let Some(ch) = c.peek() else { break };
        ntok += 1;
        if ntok > 10 {
            return None;
        }
        if ch.is_ascii_digit() {
            let start = c.i;
            let v = c.num(1, 4)?;
            let nd = c.i - start;
            if c.peek().is_some_and(|x| x.is_ascii_digit()) {
                return None;
            }
            if c.peek() == Some(b':') {
                if time.is_some() {
                    return None;
                }
                if nd == 4 && y.is_none() {
                    // CLF: dd/Mon/yyyy:HH:mm:ss.
                    y = Some(v as i64);
                    c.i += 1;
                    clf = true;
                } else {
                    c.i = start;
                }
                time = Some(time_part(&mut c, true, false)?);
                continue;
            }
            // Ordinals: 1st, 2nd, 3rd, 4th, 1er.
            let save = c.i;
            let w = c.word(&mut buf);
            if !matches!(w, b"st" | b"nd" | b"rd" | b"th" | b"er" | b"") {
                c.i = save;
            }
            match nd {
                4 if y.is_none() => y = Some(v as i64),
                1 | 2 if d.is_none() => d = Some(v),
                2 if y.is_none() && m.is_some() && time.is_none() => {
                    y = Some(if v < 70 { 2000 } else { 1900 } + v as i64)
                }
                _ => return None,
            }
        } else if ch.is_ascii_alphabetic() || ch >= 0x80 {
            let w = c.word(&mut buf);
            // Longest month / weekday name: 9 bytes ("septembre", "décembre").
            if w.is_empty() || w.len() > 9 {
                return None;
            }
            if let Some(mo) = month(w) {
                match m {
                    None => {
                        m = Some(mo);
                        month_at0 = ntok == 1;
                    }
                    // "mar." (French Tuesday) then the month.
                    Some(_) if month_at0 => {
                        m = Some(mo);
                        month_at0 = false;
                    }
                    Some(_) => return None,
                }
            } else if weekday(w) && ntok == 1 {
            } else if let (Some(off), Some(t)) = (zone_word(w), &mut time)
                && !t.2
            {
                t.0 -= off as i64 * 60 * TICKS_PER_SEC;
                t.2 = true;
            } else {
                return None;
            }
        } else {
            return None;
        }
    }
    let (m, d) = (m?, d?);
    let mut hint = if clf {
        Hint::Clf
    } else if weekday_comma && time.is_some_and(|t| t.2) {
        Hint::Rfc2822
    } else {
        Hint::Textual
    };
    let y = match y {
        Some(y) => y,
        None => {
            time?;
            hint = Hint::YearLess;
            assumed_year(f, m)?
        }
    };
    let day = date_ticks(y, m, d)?;
    Some(match time {
        Some((t, unit, zoned)) => (day + t, unit, zoned, hint),
        None => (day, DAY, false, hint),
    })
}

/// An epoch number: `[-]digits[.digits]` in the unit of `shape`; returns
/// ticks and the span of the written precision.
fn parse_number(b: &[u8], shape: Shape) -> Option<(i64, i64)> {
    let (v, fden) = plain_number(b)?;
    number_ticks(v, fden, shape)
}

/// `[-]digits[.digits]` as (value × 10^k, 10^k), k ≤ 9 fraction digits.
fn plain_number(b: &[u8]) -> Option<(i128, i128)> {
    let b = trim(b);
    let (neg, b) = match b.first()? {
        b'-' => (true, &b[1..]),
        b'+' => (false, &b[1..]),
        _ => (false, b),
    };
    let (ip, fp) = match b.iter().position(|&c| c == b'.') {
        Some(i) => (&b[..i], &b[i + 1..]),
        None => (b, &[][..]),
    };
    if ip.is_empty()
        || ip.len() > 20
        || fp.len() > 18
        || !ip.iter().chain(fp).all(u8::is_ascii_digit)
        || (fp.is_empty() && b.len() != ip.len())
    {
        return None;
    }
    let dec = |s: &[u8]| s.iter().fold(0i128, |v, &c| v * 10 + (c - b'0') as i128);
    let fd = fp.len().min(9);
    let fden = 10i128.pow(fd as u32);
    let v = dec(ip).checked_mul(fden)? + dec(&fp[..fd]);
    Some((if neg { -v } else { v }, fden))
}

fn number_ticks(v: i128, fden: i128, shape: Shape) -> Option<(i64, i64)> {
    let (num, den, zero) = shape.unit();
    let t = v.checked_mul(num)?.div_euclid(fden * den) + zero as i128;
    const MIN: i128 = -62_135_596_800 * TICKS_PER_SEC as i128; // 0001-01-01
    const MAX: i128 = 253_402_300_800 * TICKS_PER_SEC as i128; // 10000-01-01
    if !(MIN..MAX).contains(&t) {
        return None;
    }
    Some((t as i64, (num / (fden * den)).max(1) as i64))
}

/// A custom `strftime`-style format (see docs/DATE-FORMATS.md).
#[derive(Debug)]
pub struct Pattern {
    pub src: String,
    toks: Vec<Tok>,
}

impl PartialEq for Pattern {
    fn eq(&self, o: &Pattern) -> bool {
        self.src == o.src
    }
}
impl Eq for Pattern {}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Tok {
    Lit(u8),
    Space,
    Year,
    Year2,
    Month,
    MonthName,
    Day,
    Hour,
    Minute,
    Second,
    /// Fraction digits (0 = any number); `true`: preceded by `.`/`,` and optional.
    Frac(u8, bool),
    AmPm,
    Offset,
    ZoneName,
    Weekday,
    DayOfYear,
    Epoch,
}

impl Pattern {
    /// Compiles (or finds) a pattern.
    pub fn intern(src: &str) -> Result<&'static Pattern, String> {
        let mut ps = PATTERNS.lock().map_err(|_| "lock")?;
        if let Some(p) = ps.iter().find(|p| p.src == src) {
            return Ok(p);
        }
        let p: &'static Pattern = Box::leak(Box::new(Pattern::compile(src)?));
        ps.push(p);
        Ok(p)
    }

    fn compile(src: &str) -> Result<Pattern, String> {
        let mut toks = Vec::new();
        let b = src.as_bytes();
        let mut i = 0;
        while i < b.len() {
            let c = b[i];
            i += 1;
            if c.is_ascii_whitespace() {
                if toks.last() != Some(&Tok::Space) {
                    toks.push(Tok::Space);
                }
                continue;
            }
            if c != b'%' {
                toks.push(Tok::Lit(c));
                continue;
            }
            // Flags (padding) are accepted and ignored.
            while i < b.len() && matches!(b[i], b'-' | b'_' | b'0' | b'^' | b'#' | b':') {
                i += 1;
            }
            let dot = i < b.len() && b[i] == b'.';
            if dot {
                i += 1;
            }
            let mut n = 0u8;
            if i < b.len() && b[i].is_ascii_digit() {
                n = b[i] - b'0';
                i += 1;
            }
            let Some(&s) = b.get(i) else {
                return Err("format ends with %".into());
            };
            i += 1;
            if dot && s != b'f' {
                return Err(format!("%.{} is not supported (only %.f)", s as char));
            }
            match s {
                b'Y' => toks.push(Tok::Year),
                b'y' => toks.push(Tok::Year2),
                b'm' => toks.push(Tok::Month),
                b'b' | b'B' | b'h' => toks.push(Tok::MonthName),
                b'd' | b'e' => toks.push(Tok::Day),
                b'H' | b'k' | b'I' | b'l' => toks.push(Tok::Hour),
                b'M' => toks.push(Tok::Minute),
                b'S' => toks.push(Tok::Second),
                b'f' => toks.push(Tok::Frac(n, dot)),
                b'p' | b'P' => toks.push(Tok::AmPm),
                b'z' => toks.push(Tok::Offset),
                b'Z' => toks.push(Tok::ZoneName),
                b'a' | b'A' => toks.push(Tok::Weekday),
                b'j' => toks.push(Tok::DayOfYear),
                b's' => toks.push(Tok::Epoch),
                b'T' => toks.extend([
                    Tok::Hour,
                    Tok::Lit(b':'),
                    Tok::Minute,
                    Tok::Lit(b':'),
                    Tok::Second,
                ]),
                b'R' => toks.extend([Tok::Hour, Tok::Lit(b':'), Tok::Minute]),
                b'F' => toks.extend([
                    Tok::Year,
                    Tok::Lit(b'-'),
                    Tok::Month,
                    Tok::Lit(b'-'),
                    Tok::Day,
                ]),
                b'D' => toks.extend([
                    Tok::Month,
                    Tok::Lit(b'/'),
                    Tok::Day,
                    Tok::Lit(b'/'),
                    Tok::Year2,
                ]),
                b'%' => toks.push(Tok::Lit(b'%')),
                b't' | b'n' => toks.push(Tok::Space),
                x => return Err(format!("%{} is not supported", x as char)),
            }
        }
        let has = |t: Tok| toks.contains(&t);
        if !has(Tok::Epoch)
            && !has(Tok::DayOfYear)
            && !(has(Tok::Day) && (has(Tok::Month) || has(Tok::MonthName)))
        {
            return Err("the format needs a day and a month (%d %m or %b), %j or %s".into());
        }
        Ok(Pattern {
            src: src.to_string(),
            toks,
        })
    }

    fn parse(&self, b: &[u8], f: &Fmt) -> Option<(i64, i64, bool)> {
        let b = trim(b);
        let mut c = Cur { b, i: 0 };
        let (mut y, mut m, mut d, mut doy) = (None::<i64>, None::<u32>, None::<u32>, None::<u32>);
        let (mut h, mut mi, mut s, mut frac) = (0u32, 0u32, 0u32, 0i64);
        let mut unit = DAY;
        let mut pm: Option<bool> = None;
        let mut off: Option<i64> = None;
        let mut epoch: Option<(i64, i64)> = None;
        let mut buf = [0u8; 16];
        for t in &self.toks {
            match *t {
                Tok::Lit(x) => {
                    if !c.peek()?.eq_ignore_ascii_case(&x) {
                        return None;
                    }
                    c.i += 1;
                }
                Tok::Space => {
                    while c.peek().is_some_and(|x| x.is_ascii_whitespace()) {
                        c.i += 1;
                    }
                }
                Tok::Year => y = Some(c.num(4, 4)? as i64),
                Tok::Year2 => {
                    let v = c.num(2, 2)? as i64;
                    y = Some(if v < 70 { 2000 } else { 1900 } + v);
                }
                Tok::Month => m = Some(c.num(1, 2)?),
                Tok::MonthName => {
                    m = Some(month(c.word(&mut buf))?);
                    c.eat(b'.');
                }
                Tok::Day => {
                    c.eat(b' ');
                    d = Some(c.num(1, 2)?);
                }
                Tok::DayOfYear => doy = Some(c.num(1, 3)?),
                Tok::Hour => {
                    h = c.num(1, 2)?;
                    unit = unit.min(3600 * TICKS_PER_SEC);
                }
                Tok::Minute => {
                    mi = c.num(1, 2)?;
                    unit = unit.min(60 * TICKS_PER_SEC);
                }
                Tok::Second => {
                    s = c.num(1, 2)?;
                    unit = unit.min(TICKS_PER_SEC);
                }
                Tok::Frac(n, dot) => {
                    if dot && !(c.eat(b'.') || c.eat(b',')) {
                        continue;
                    }
                    let start = c.i;
                    while c.peek().is_some_and(|x| x.is_ascii_digit())
                        && (n == 0 || c.i - start < n as usize)
                    {
                        c.i += 1;
                    }
                    let ds = &c.b[start..c.i];
                    if ds.is_empty() || (n != 0 && ds.len() != n as usize) {
                        return None;
                    }
                    frac = 0;
                    for k in 0..7 {
                        frac = frac * 10 + ds.get(k).map_or(0, |x| (x - b'0') as i64);
                    }
                    unit = unit.min(10i64.pow(7u32.saturating_sub(ds.len() as u32)).max(1));
                }
                Tok::AmPm => {
                    let w = c.word(&mut buf);
                    pm = Some(match w {
                        b"am" | b"a" => false,
                        b"pm" | b"p" => true,
                        _ => return None,
                    });
                }
                Tok::Offset => {
                    if c.eat(b'Z') || c.eat(b'z') {
                        off = Some(0);
                    } else {
                        let sign = match c.peek()? {
                            b'+' => 1,
                            b'-' => -1,
                            _ => return None,
                        };
                        c.i += 1;
                        let oh = c.num(2, 2)? as i64;
                        c.eat(b':');
                        let om = c.num(2, 2).unwrap_or(0) as i64;
                        if oh > 23 || om > 59 {
                            return None;
                        }
                        off = Some(sign * (oh * 60 + om));
                    }
                }
                Tok::ZoneName => off = Some(zone_word(c.word(&mut buf))? as i64),
                Tok::Weekday => {
                    if c.word(&mut buf).is_empty() {
                        return None;
                    }
                    c.eat(b'.');
                }
                Tok::Epoch => {
                    let start = c.i;
                    c.eat(b'-');
                    while c.peek().is_some_and(|x| x.is_ascii_digit() || x == b'.') {
                        c.i += 1;
                    }
                    epoch = Some(parse_number(&c.b[start..c.i], Shape::EpochS)?);
                }
            }
        }
        if !c.done() {
            return None;
        }
        if let Some((t, span)) = epoch {
            return Some((t, span, true));
        }
        if let Some(pm) = pm {
            if !(1..=12).contains(&h) {
                return None;
            }
            h = h % 12 + if pm { 12 } else { 0 };
        }
        if h > 23 || mi > 59 || s > 60 {
            return None;
        }
        let day = match (doy, m, d) {
            (Some(n), _, _) => {
                let y = y.or_else(|| assumed_year(f, 12))?;
                let first = date_ticks(y, 1, 1)?;
                let len = if days_in_month(y, 2) == 29 { 366 } else { 365 };
                if n == 0 || n > len {
                    return None;
                }
                first + (n as i64 - 1) * DAY
            }
            (None, Some(m), Some(d)) => date_ticks(y.or_else(|| assumed_year(f, m))?, m, d)?,
            _ => return None,
        };
        let mut t = day + ((h * 3600 + mi * 60 + s.min(59)) as i64) * TICKS_PER_SEC + frac;
        if let Some(o) = off {
            t -= o * 60 * TICKS_PER_SEC;
        }
        Some((t, unit, off.is_some()))
    }
}

/// Parses any accepted date text; slash dates are taken as US month/day.
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

/// Column detection: the kind of date text `b` is (None: not date text).
/// Year-less dates are recognised as such without a year.
pub fn text_hint(b: &[u8], order: Order) -> Option<Hint> {
    let f = Fmt {
        order,
        year: 2000,
        ..Fmt::default()
    };
    parse_text(b, &f).map(|r| r.3)
}

/// Epoch shapes tried by column detection, and whether they need a header
/// name that says so (their value range overlaps plain numbers).
pub const EPOCH_CANDIDATES: &[(Shape, &[&str])] = &[
    (Shape::Excel, &["excel", "serial", "oadate"]),
    (
        Shape::Cocoa,
        &[
            "cocoa",
            "cfabsolute",
            "nsdate",
            "coredata",
            "macabsolute",
            "mac_absolute",
        ],
    ),
    (Shape::EpochDays, &["days", "epochday", "epoch_day"]),
    (Shape::FileTime, &[]),
    (Shape::Ticks, &[]),
    (Shape::WebKit, &[]),
    (Shape::EpochNs, &[]),
    (Shape::EpochUs, &[]),
    (Shape::EpochMs, &[]),
    (Shape::EpochS, &[]),
];

/// Ticks of a plain number read in each unit of [`EPOCH_CANDIDATES`]
/// (detection; `i64::MIN` where out of range). None: not a plain number.
pub fn epoch_ticks(b: &[u8]) -> Option<[i64; EPOCH_CANDIDATES.len()]> {
    let (v, fden) = plain_number(b)?;
    let mut out = [i64::MIN; EPOCH_CANDIDATES.len()];
    for (o, (shape, _)) in out.iter_mut().zip(EPOCH_CANDIDATES) {
        if let Some((t, _)) = number_ticks(v, fden, *shape) {
            *o = t;
        }
    }
    Some(out)
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

    /// 2024-03-01T12:34:56Z plus `frac` ticks.
    const T1: i64 = T0 + (12 * 3600 + 34 * 60 + 56) * TICKS_PER_SEC;

    /// One row per accepted shape: text, format, expected ticks, span,
    /// hint. Each is checked for parse, display round trip (the formatted
    /// time parses back to the same instant), and its use by the engine in
    /// tests/date_formats.rs (sort order, range filter).
    pub fn table() -> Vec<(&'static str, Fmt, i64, i64, Option<Hint>)> {
        let tx = |o: Order| Fmt::from(o);
        let sh = Fmt::shape;
        let year = Fmt {
            year: 2024,
            ..Fmt::default()
        };
        let s = TICKS_PER_SEC;
        let h = 3600 * s;
        use Hint::*;
        vec![
            ("2024-03-01T12:34:56Z", tx(Order::Ymd), T1, s, Some(Iso)),
            (
                "2024-03-01T14:34:56+02:00",
                tx(Order::Ymd),
                T1,
                s,
                Some(Iso),
            ),
            ("2024-03-01T14:34:56+0200", tx(Order::Ymd), T1, s, Some(Iso)),
            ("2024-03-01T14:34:56+02", tx(Order::Ymd), T1, s, Some(Iso)),
            (
                "2024-03-01 12:34:56.123Z",
                tx(Order::Ymd),
                T1 + 1_230_000,
                10_000,
                Some(Iso),
            ),
            (
                "2024-03-01 12:34:56.123456",
                tx(Order::Ymd),
                T1 + 1_234_560,
                10,
                Some(Iso),
            ),
            (
                "2024-03-01 12:34:56.1234567",
                tx(Order::Ymd),
                T1 + 1_234_567,
                1,
                Some(Iso),
            ),
            (
                "2024-03-01T12:34:56.123456789Z",
                tx(Order::Ymd),
                T1 + 1_234_567,
                1,
                Some(Iso),
            ),
            (
                "2024-03-01 12:34:56,5 UTC",
                tx(Order::Ymd),
                T1 + 5_000_000,
                s / 10,
                Some(Iso),
            ),
            ("2024-03-01", tx(Order::Ymd), T0, DAY, Some(Iso)),
            ("2024.03.01 12:34:56", tx(Order::Ymd), T1, s, Some(Iso)),
            ("20240301T123456Z", tx(Order::Ymd), T1, s, Some(IsoBasic)),
            (
                "20240301T143456.5+0200",
                tx(Order::Ymd),
                T1 + 5_000_000,
                s / 10,
                Some(IsoBasic),
            ),
            (
                "Fri, 01 Mar 2024 12:34:56 GMT",
                tx(Order::Ymd),
                T1,
                s,
                Some(Rfc2822),
            ),
            (
                "Fri, 1 Mar 2024 07:34:56 -0500",
                tx(Order::Ymd),
                T1,
                s,
                Some(Rfc2822),
            ),
            (
                "Fri, 01 Mar 2024 04:34:56 PST",
                tx(Order::Ymd),
                T1,
                s,
                Some(Rfc2822),
            ),
            ("2024/03/01 12:34:56", tx(Order::Ymd), T1, s, Some(Iso)),
            ("01/03/2024 12:34:56", tx(Order::Dmy), T1, s, Some(Numeric)),
            ("03/01/2024 12:34:56", tx(Order::Mdy), T1, s, Some(Numeric)),
            ("3/1/2024 12:34:56 PM", tx(Order::Mdy), T1, s, Some(Numeric)),
            (
                "3/1/2024, 12:34 PM",
                tx(Order::Mdy),
                T1 - 56 * s,
                60 * s,
                Some(Numeric),
            ),
            ("1/3/2024 12:34:56 PM", tx(Order::Dmy), T1, s, Some(Numeric)),
            ("01.03.2024 12:34:56", tx(Order::Ymd), T1, s, Some(Numeric)),
            ("01-03-2024 12:34:56", tx(Order::Dmy), T1, s, Some(Numeric)),
            ("01-Mar-2024", tx(Order::Ymd), T0, DAY, Some(Textual)),
            ("01-MAR-2024 12:34:56", tx(Order::Ymd), T1, s, Some(Textual)),
            ("Mar 1 2024 12:34:56", tx(Order::Ymd), T1, s, Some(Textual)),
            (
                "Mar 1, 2024 12:34:56 PM",
                tx(Order::Ymd),
                T1,
                s,
                Some(Textual),
            ),
            ("March 1st, 2024", tx(Order::Ymd), T0, DAY, Some(Textual)),
            ("1 March 2024", tx(Order::Ymd), T0, DAY, Some(Textual)),
            (
                "Fri Mar  1 12:34:56 2024",
                tx(Order::Ymd),
                T1,
                s,
                Some(Textual),
            ),
            (
                "Fri Mar  1 12:34:56 UTC 2024",
                tx(Order::Ymd),
                T1,
                s,
                Some(Textual),
            ),
            ("1er mars 2024", tx(Order::Ymd), T0, DAY, Some(Textual)),
            (
                "vendredi 1 mars 2024 13:34:56 CET",
                tx(Order::Ymd),
                T1,
                s,
                Some(Textual),
            ),
            (
                "1 févr. 2024",
                tx(Order::Ymd),
                T0 - 29 * DAY,
                DAY,
                Some(Textual),
            ),
            (
                "15 AOÛT 2024",
                tx(Order::Ymd),
                T0 + 167 * DAY,
                DAY,
                Some(Textual),
            ),
            ("mar. 1 mars 2024", tx(Order::Ymd), T0, DAY, Some(Textual)),
            (
                "01/Mar/2024:13:34:56 +0100",
                tx(Order::Ymd),
                T1,
                s,
                Some(Clf),
            ),
            (
                "[01/Mar/2024:12:34:56 +0000]",
                tx(Order::Ymd),
                T1,
                s,
                Some(Clf),
            ),
            ("Mar  1 12:34:56", year, T1, s, Some(YearLess)),
            (
                "Mar 1 12:34:56.250",
                year,
                T1 + 2_500_000,
                10_000,
                Some(YearLess),
            ),
            (
                "03-01 12:34:56.789",
                year,
                T1 + 7_890_000,
                10_000,
                Some(YearLess),
            ),
            ("1709296496", sh(Shape::EpochS), T1, s, None),
            (
                "1709296496.5",
                sh(Shape::EpochS),
                T1 + 5_000_000,
                s / 10,
                None,
            ),
            (
                "1709296496123",
                sh(Shape::EpochMs),
                T1 + 1_230_000,
                10_000,
                None,
            ),
            (
                "1709296496123456",
                sh(Shape::EpochUs),
                T1 + 1_234_560,
                10,
                None,
            ),
            (
                "1709296496123456789",
                sh(Shape::EpochNs),
                T1 + 1_234_567,
                1,
                None,
            ),
            ("19783", sh(Shape::EpochDays), T0, DAY, None),
            (
                "133537700961234567",
                sh(Shape::FileTime),
                T1 + 1_234_567,
                1,
                None,
            ),
            (
                "638448932961234567",
                sh(Shape::Ticks),
                T1 + 1_234_567,
                1,
                None,
            ),
            ("45352", sh(Shape::Excel), T0, DAY, None),
            ("45352.5", sh(Shape::Excel), T0 + 12 * h, DAY / 10, None),
            (
                "13353770096123456",
                sh(Shape::WebKit),
                T1 + 1_234_560,
                10,
                None,
            ),
            ("730989296", sh(Shape::Cocoa), T1, s, None),
            (
                "730989296.25",
                sh(Shape::Cocoa),
                T1 + 2_500_000,
                s / 100,
                None,
            ),
        ]
    }

    #[test]
    fn every_format_parses_and_round_trips() {
        for (text, f, want, span, hint) in table() {
            let got = parse_span(text.as_bytes(), f);
            assert_eq!(got, Some((want, span)), "{text:?} as {}", f.spec());
            // Display round trip: the shown form parses back.
            let shown = format(want, 0);
            assert_eq!(parse_any(shown.as_bytes()), Some(want), "{text:?}");
            // Detection sees the same kind of text.
            if f.shape == Shape::Text {
                assert_eq!(text_hint(text.as_bytes(), f.order), hint, "{text:?}");
            }
            // The spec string survives a round trip.
            assert_eq!(Fmt::from_spec(&f.spec()), Ok(f));
        }
    }

    #[test]
    fn not_dates() {
        for s in [
            "Mar",
            "March 2024",
            "1 2 3",
            "Marché 1 2024",
            "12:34:56",
            "2024",
            "v1.2.3",
            "1.2.2024x",
            "Fri 12:34:56",
            "01-02",
            "1709296496",
            "Mar 1 2024 25:00",
            "May the 4th",
            "32 Jan 2024",
            "Feb 30 2024",
            "Monday",
            "100 Mar 2024",
        ] {
            assert_eq!(parse_any(s.as_bytes()), None, "{s:?}");
        }
        // Year-less values need a year.
        assert_eq!(parse_any(b"Mar  1 12:34:56"), None);
        // Epoch numbers: plain decimal only, within years 1..9999.
        for s in ["0x10", "1e9", "12a", "", "-", "1.", "99999999999999999999"] {
            assert_eq!(
                parse(s.as_bytes(), Fmt::shape(Shape::EpochS)),
                None,
                "{s:?}"
            );
        }
    }

    #[test]
    fn syslog_year_rolls_back_after_the_file_month() {
        let f = Fmt {
            year: 2025,
            roll: 2,
            hint: Hint::YearLess,
            ..Fmt::default()
        };
        assert_eq!(
            parse(b"Dec 31 23:00:00", f),
            parse_any(b"2024-12-31 23:00:00")
        );
        assert_eq!(
            parse(b"Feb  1 10:00:00", f),
            parse_any(b"2025-02-01 10:00:00")
        );
        assert!(f.year_assumed() && f.rewrites());
        assert_eq!(Fmt::from_spec(&f.spec()), Ok(f));
    }

    #[test]
    fn zones_for_values_without_offset() {
        let paris = Zone::intern("Europe/Paris").unwrap();
        let f = Fmt {
            zone: paris,
            ..Fmt::default()
        };
        // Winter +01:00, summer +02:00; explicit offsets win.
        assert_eq!(parse(b"2024-03-01 13:34:56", f), Some(T1));
        assert_eq!(
            parse(b"2024-07-01 14:00:00", f),
            parse_any(b"2024-07-01T12:00:00Z")
        );
        assert_eq!(parse(b"2024-03-01T12:34:56Z", f), Some(T1));
        // Epoch numbers are instants already.
        let e = Fmt {
            zone: paris,
            ..Fmt::shape(Shape::EpochS)
        };
        assert_eq!(parse(b"1709296496", e), Some(T1));
        let fixed = Fmt {
            zone: Zone::intern("+05:30").unwrap(),
            ..Fmt::default()
        };
        assert_eq!(parse(b"2024-03-01 18:04:56", fixed), Some(T1));
        assert!(Zone::intern("UTC").unwrap().is_none());
        assert!(Zone::intern("Nowhere/Land").is_err());
        assert_eq!(Fmt::from_spec(&f.spec()), Ok(f));
        // Operands ignore the column zone (they are in the display zone).
        assert_eq!(
            parse_operand(b"2024-03-01 12:34:56", f).map(|r| r.0),
            Some(T1)
        );
        assert_eq!(parse_operand(b"1709296496", e).map(|r| r.0), Some(T1));
    }

    #[test]
    fn custom_patterns() {
        let c = |p: &str| Fmt::shape(Shape::Custom(Pattern::intern(p).unwrap()));
        let cases: &[(&str, &str, i64)] = &[
            ("%d/%m/%Y %H:%M:%S", "01/03/2024 12:34:56", T1),
            ("%Y%m%d%H%M%S", "20240301123456", T1),
            (
                "%d.%m.%y %I:%M %p",
                "01.03.24 12:34 PM",
                T1 - 56 * TICKS_PER_SEC,
            ),
            ("%a %b %e %T %Y", "Fri Mar  1 12:34:56 2024", T1),
            (
                "%F %T%.f %z",
                "2024-03-01 14:34:56.5 +02:00",
                T1 + 5_000_000,
            ),
            ("%F %T%.f", "2024-03-01 12:34:56", T1),
            ("%Y-%j %H:%M:%S", "2024-061 12:34:56", T1),
            ("%s", "1709296496", T1),
            (
                "%d %B %Y à %Hh%M",
                "1 mars 2024 à 12h34",
                T1 - 56 * TICKS_PER_SEC,
            ),
            ("%Y/%m/%d %H:%M:%S %Z", "2024/03/01 07:34:56 EST", T1),
            (
                "%d/%m/%Y %H:%M:%S.%3f",
                "01/03/2024 12:34:56.123",
                T1 + 1_230_000,
            ),
        ];
        for (p, v, want) in cases {
            assert_eq!(parse(v.as_bytes(), c(p)), Some(*want), "{p:?} on {v:?}");
        }
        assert_eq!(
            parse(b"01/03/2024 12:34:56 extra", c("%d/%m/%Y %H:%M:%S")),
            None
        );
        assert_eq!(
            parse(b"01/03/2024 12:34:56.12", c("%d/%m/%Y %H:%M:%S.%3f")),
            None
        );
        let ym = Fmt {
            year: 2024,
            ..c("%b %d %H:%M:%S")
        };
        assert_eq!(parse(b"Mar 01 12:34:56", ym), Some(T1));
        assert!(Pattern::intern("%H:%M").is_err());
        assert!(Pattern::intern("%Q %d %m").is_err());
        assert!(std::ptr::eq(
            Pattern::intern("%s").unwrap(),
            Pattern::intern("%s").unwrap()
        ));
        let f = c("%d/%m/%Y;x");
        assert_eq!(Fmt::from_spec(&f.spec()), Ok(f));
    }
}
