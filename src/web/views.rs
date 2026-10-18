//! View models: packages turned into pre-formatted rows for the templates.

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};

use crate::duration::humanize;
use crate::model::{Package, Status};

pub fn fmt_datetime(t: Timestamp) -> String {
    t.strftime("%Y-%m-%d %H:%M UTC").to_string()
}

/// "in 12d", "3d ago", "now".
pub fn relative(t: Timestamp, now: Timestamp) -> String {
    let d = t.duration_since(now);
    if d.abs() < SignedDuration::from_secs(60) {
        "now".into()
    } else if d.is_positive() {
        format!("in {}", humanize(d))
    } else {
        format!("{} ago", humanize(d))
    }
}

fn date(t: Timestamp) -> Date {
    t.to_zoned(TimeZone::UTC).date()
}

/// "Today", "Tomorrow", "Yesterday", or "Tue 21 Oct".
pub fn fmt_day(t: Timestamp, now: Timestamp) -> String {
    let (d, today) = (date(t), date(now));
    match (d - today).get_days() {
        0 => "Today".into(),
        1 => "Tomorrow".into(),
        -1 => "Yesterday".into(),
        _ => d.strftime("%a %-d %b").to_string(),
    }
}

/// An ETA as a day or a range of days.
pub fn fmt_eta(from: Option<Timestamp>, to: Option<Timestamp>, now: Timestamp) -> String {
    match (from, to) {
        (Some(f), Some(t)) if date(f) != date(t) => {
            format!("{} – {}", fmt_day(f, now), fmt_day(t, now))
        }
        (Some(d), _) | (None, Some(d)) => fmt_day(d, now),
        (None, None) => String::new(),
    }
}

#[derive(Clone, Debug)]
pub struct PackageRow {
    pub id: i64,
    pub title: String,
    pub merchant: String,
    pub recipient: String,
    pub courier: String,
    pub tracking_number: String,
    pub status: Status,
    pub status_rel: String,
    pub status_date: String,
    pub eta: String,
    pub code: String,
    pub code_label: &'static str,
    pub pickup_name: String,
}

impl PackageRow {
    pub fn new(p: &Package, now: Timestamp) -> PackageRow {
        PackageRow {
            id: p.id,
            title: p.title.clone(),
            merchant: p.merchant.clone().unwrap_or_default(),
            recipient: p.recipient.clone().unwrap_or_default(),
            courier: p.courier.clone().unwrap_or_default(),
            tracking_number: p.tracking_number.clone().unwrap_or_default(),
            status: p.status,
            status_rel: relative(p.status_at, now),
            status_date: fmt_datetime(p.status_at),
            eta: if p.status == Status::Delivered {
                String::new()
            } else {
                fmt_eta(p.eta_from, p.eta_to, now)
            },
            // A code is only useful until the package is in hand.
            code: if p.status == Status::Delivered {
                String::new()
            } else {
                p.code.clone().unwrap_or_default()
            },
            code_label: p.code_kind.map(|k| k.label()).unwrap_or("Code"),
            pickup_name: if p.status == Status::ReadyForPickup {
                p.pickup_point.name.clone().unwrap_or_default()
            } else {
                String::new()
            },
        }
    }

    /// Where the title links to.
    pub fn href(&self) -> String {
        format!("/#package-{}", self.id)
    }

    pub fn status_str(&self) -> &'static str {
        self.status.as_str()
    }

    pub fn status_label(&self) -> &'static str {
        self.status.label()
    }

    pub fn matches(&self, q: &str) -> bool {
        let q = q.to_lowercase();
        [
            &self.title,
            &self.merchant,
            &self.recipient,
            &self.courier,
            &self.tracking_number,
        ]
        .iter()
        .any(|f| f.to_lowercase().contains(&q))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn relative_times() {
        let now = t("2026-09-25T00:00:00Z");
        assert_eq!(relative(t("2026-10-07T00:00:00Z"), now), "in 12d");
        assert_eq!(relative(t("2026-09-22T00:00:00Z"), now), "3d ago");
        assert_eq!(relative(now, now), "now");
    }

    #[test]
    fn etas_are_days() {
        let now = t("2026-10-18T15:00:00Z");
        let day = |s| Some(t(s));
        assert_eq!(fmt_eta(day("2026-10-18T00:00:00Z"), None, now), "Today");
        assert_eq!(
            fmt_eta(
                day("2026-10-19T00:00:00Z"),
                day("2026-10-19T00:00:00Z"),
                now
            ),
            "Tomorrow"
        );
        assert_eq!(
            fmt_eta(
                day("2026-10-21T00:00:00Z"),
                day("2026-10-24T00:00:00Z"),
                now
            ),
            "Wed 21 Oct – Sat 24 Oct"
        );
        assert_eq!(fmt_eta(None, None, now), "");
    }
}
