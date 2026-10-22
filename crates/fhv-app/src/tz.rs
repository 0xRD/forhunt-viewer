//! Display time-zone conversion. Data stays as in the file (UTC for
//! forensic tools); only the grid and detail pane show converted times.

use fhv_engine::time::{self, Order, TICKS_PER_SEC};

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

    /// The cell converted to the display zone, or `None` to show it as is.
    pub fn convert(&self, cell: &str, order: Order) -> Option<String> {
        let tz = self.tz.as_ref()?;
        let ticks = time::parse(cell.as_bytes(), order)?;
        let secs = ticks.div_euclid(TICKS_PER_SEC);
        let ts = jiff::Timestamp::from_second(secs).ok()?;
        let off = tz.to_offset(ts).seconds();
        let mut s = time::format(ticks, off);
        let sign = if off < 0 { '-' } else { '+' };
        let a = off.unsigned_abs();
        s.push_str(&format!(" {sign}{:02}:{:02}", a / 3600, a / 60 % 60));
        Some(s)
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
    }
}
