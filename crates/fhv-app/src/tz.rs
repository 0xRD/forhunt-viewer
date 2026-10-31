//! Display time-zone conversion. Data stays as in the file (UTC for
//! forensic tools); only the grid and detail pane show converted times.

use fhv_engine::filter::Op;
#[cfg(test)]
use fhv_engine::time::Order;
use fhv_engine::time::{self, Fmt, TICKS_PER_SEC};

#[derive(Clone)]
pub struct Display {
    tz: Option<jiff::tz::TimeZone>,
    pub name: String,
}

/// Zones offered in the menu (any IANA name can be typed).
pub const COMMON: &[&str] = &[
    "UTC",
    "Europe/London",
    "Europe/Paris",
    "Europe/Berlin",
    "Europe/Moscow",
    "America/New_York",
    "America/Chicago",
    "America/Denver",
    "America/Los_Angeles",
    "America/Sao_Paulo",
    "Asia/Dubai",
    "Asia/Kolkata",
    "Asia/Shanghai",
    "Asia/Tokyo",
    "Australia/Sydney",
];

impl Display {
    pub fn new(name: &str) -> Display {
        let tz = if name == "UTC" {
            None
        } else {
            jiff::tz::TimeZone::get(name).ok()
        };
        Display {
            name: if tz.is_some() || name == "UTC" {
                name.to_string()
            } else {
                "UTC".into()
            },
            tz,
        }
    }

    pub fn is_utc(&self) -> bool {
        self.tz.is_none()
    }

    /// The cell converted to the display zone, or `None` to show it as is
    /// (UTC display of a column whose text is the instant as written).
    /// Columns whose cells are numbers, or read with an assumed zone or
    /// year, always show the parsed time (D-051).
    pub fn convert(&self, cell: &str, f: impl Into<Fmt>) -> Option<String> {
        let f = f.into();
        let ticks = time::parse(cell.as_bytes(), f)?;
        let Some(tz) = self.tz.as_ref() else {
            return f.rewrites().then(|| time::format(ticks, 0));
        };
        let secs = ticks.div_euclid(TICKS_PER_SEC);
        // Past the zone database's range: UTC, labelled as such.
        let off = jiff::Timestamp::from_second(secs).map_or(0, |ts| tz.to_offset(ts).seconds());
        let mut s = time::format(ticks, off);
        let sign = if off < 0 { '-' } else { '+' };
        let a = off.unsigned_abs();
        s.push_str(&format!(" {sign}{:02}:{:02}", a / 3600, a / 60 % 60));
        Some(s)
    }
}

impl Display {
    /// Range operands of a time-column condition, with every time written
    /// without an offset read in the display zone (what the analyst sees)
    /// and rewritten as an exact UTC instant the engine compares with the
    /// file (D-038). Lower bounds become the first tick of the period
    /// written, `<=` and the end of `between` its last tick (D-034), so a
    /// day in this zone is 23 or 25 hours across a DST change. A wall-clock
    /// time that happens twice (or not at all) widens the range rather
    /// than narrowing it. Other operators and UTC display: unchanged.
    pub fn op_to_utc(&self, op: &Op, f: impl Into<Fmt>) -> Op {
        let f = f.into();
        let Some(tz) = &self.tz else {
            return op.clone();
        };
        let conv = |v: &String, upper: bool| -> String {
            let Some((wall, span, zoned)) = time::parse_operand(v.as_bytes(), f) else {
                return v.clone();
            };
            if zoned {
                return v.clone();
            }
            let at = |w: i64, latest: bool| -> Option<i64> {
                let secs = w.div_euclid(TICKS_PER_SEC);
                let frac = w.rem_euclid(TICKS_PER_SEC);
                let dt = jiff::Timestamp::from_second(secs)
                    .ok()?
                    .to_zoned(jiff::tz::TimeZone::UTC)
                    .datetime();
                let amb = tz.to_ambiguous_zoned(dt);
                let a = amb.clone().earlier().ok()?.timestamp().as_second();
                let b = amb.later().ok()?.timestamp().as_second();
                let s = if latest { a.max(b) } else { a.min(b) };
                Some(s * TICKS_PER_SEC + frac)
            };
            let utc = if upper {
                at(wall + span, true).map(|t| t - 1)
            } else {
                at(wall, false)
            };
            match utc {
                Some(t) => format!("{}Z", time::format(t, 0)),
                None => v.clone(),
            }
        };
        match op {
            Op::Gt(v) => Op::Gt(conv(v, false)),
            Op::Ge(v) => Op::Ge(conv(v, false)),
            Op::Lt(v) => Op::Lt(conv(v, false)),
            Op::Le(v) => Op::Le(conv(v, true)),
            Op::Between(a, b) => Op::Between(conv(a, false), conv(b, true)),
            Op::Not(x) => Op::Not(Box::new(self.op_to_utc(x, f))),
            other => other.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_with_dst() {
        let d = Display::new("Europe/Paris");
        assert_eq!(
            d.convert("2024-03-01 12:00:00.0000000", Order::Ymd)
                .as_deref(),
            Some("2024-03-01 13:00:00.0000000 +01:00")
        );
        assert_eq!(
            d.convert("2024-07-01T12:00:00Z", Order::Ymd).as_deref(),
            Some("2024-07-01 14:00:00.0000000 +02:00")
        );
        assert!(
            Display::new("UTC")
                .convert("2024-03-01", Order::Ymd)
                .is_none()
        );
        assert!(Display::new("Not/AZone").is_utc());
        // Epoch numbers show as times, in UTC too.
        let epoch = Fmt::shape(fhv_engine::time::Shape::EpochS);
        assert_eq!(
            Display::new("UTC").convert("1709251200", epoch).as_deref(),
            Some("2024-03-01 00:00:00.0000000")
        );
        assert_eq!(
            d.convert("1709251200", epoch).as_deref(),
            Some("2024-03-01 01:00:00.0000000 +01:00")
        );
    }

    #[test]
    fn operands_in_the_display_zone() {
        let d = Display::new("Europe/Paris");
        let o = |op: Op| d.op_to_utc(&op, Order::Ymd);
        let b = |a: &str, z: &str| Op::Between(a.into(), z.into());
        // 25-hour day (DST ends 2025-10-26 03:00 CEST -> 02:00 CET).
        assert_eq!(
            o(b("2025-10-26", "2025-10-26")),
            b(
                "2025-10-25 22:00:00.0000000Z",
                "2025-10-26 22:59:59.9999999Z"
            )
        );
        // 23-hour day (DST starts 2025-03-30 02:00 CET -> 03:00 CEST).
        assert_eq!(
            o(b("2025-03-30", "2025-03-30")),
            b(
                "2025-03-29 23:00:00.0000000Z",
                "2025-03-30 21:59:59.9999999Z"
            )
        );
        // 02:30 twice on 2025-10-26: lower bound the first, upper the last.
        assert_eq!(
            o(Op::Ge("2025-10-26 02:30".into())),
            Op::Ge("2025-10-26 00:30:00.0000000Z".into())
        );
        assert_eq!(
            o(Op::Le("2025-10-26 02:30".into())),
            Op::Le("2025-10-26 01:30:59.9999999Z".into())
        );
        // 02:30 never happens on 2025-03-30: the widest reading.
        assert_eq!(
            o(Op::Ge("2025-03-30 02:30".into())),
            Op::Ge("2025-03-30 00:30:00.0000000Z".into())
        );
        // Explicit offsets, text operators and UTC display are unchanged.
        let z = Op::Le("2025-10-26T02:30:00+02:00".into());
        assert_eq!(o(z.clone()), z);
        let c = Op::Contains("15:08".into());
        assert_eq!(o(c.clone()), c);
        let le = Op::Le("2025-10-26".into());
        assert_eq!(Display::new("UTC").op_to_utc(&le, Order::Ymd), le);
    }
}
