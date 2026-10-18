//! View models: packages turned into pre-formatted rows for the templates.

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};

use crate::duration::humanize;
use crate::model::{Event, Package, Source, Status};

pub fn fmt_date(t: Timestamp) -> String {
    t.strftime("%Y-%m-%d").to_string()
}

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

/// Only http(s) URLs are rendered as links (never `javascript:` and co).
pub fn safe_link(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    matches!(parsed.scheme(), "http" | "https").then(|| parsed.to_string())
}

/// A map search for an address (opens the phone's maps app).
pub fn map_link(address: &str) -> String {
    let q: String = url::form_urlencoded::byte_serialize(address.as_bytes()).collect();
    format!("https://www.google.com/maps/search/?api=1&query={q}")
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
    /// Why it needs attention, or empty.
    pub attention: String,
}

impl PackageRow {
    pub fn new(p: &Package, now: Timestamp, stale_after: SignedDuration) -> PackageRow {
        PackageRow {
            attention: p.attention(now, stale_after).unwrap_or_default(),
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
        format!("/packages/{}", self.id)
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

pub struct EventRow {
    pub at: String,
    pub at_rel: String,
    pub status: Option<Status>,
    pub description: String,
    pub source: &'static str,
}

impl EventRow {
    pub fn new(e: &Event, now: Timestamp) -> EventRow {
        EventRow {
            at: fmt_datetime(e.at),
            at_rel: relative(e.at, now),
            status: e.status,
            description: e.description.clone(),
            source: match e.source {
                Source::Courier => "courier",
                Source::Message => "message",
                Source::Manual => "by hand",
            },
        }
    }

    pub fn status_str(&self) -> &'static str {
        self.status.map(Status::as_str).unwrap_or("")
    }
}

/// Everything the detail page shows, pre-formatted.
pub struct PackageDetail {
    pub row: PackageRow,
    pub order_ref: String,
    pub tracking_url: Option<String>,
    pub code_kind: &'static str,
    pub code_expires: String,
    pub pickup_address: String,
    pub pickup_map: String,
    pub pickup_hours: String,
    pub notes: String,
    pub status_source: &'static str,
    pub archived: String,
    pub created: String,
    pub events: Vec<EventRow>,
}

impl PackageDetail {
    pub fn new(
        p: &Package,
        events: &[Event],
        now: Timestamp,
        stale_after: SignedDuration,
    ) -> PackageDetail {
        let mut row = PackageRow::new(p, now, stale_after);
        // The detail page shows the pickup point whatever the status.
        row.pickup_name = p.pickup_point.name.clone().unwrap_or_default();
        let address = p.pickup_point.address.clone().unwrap_or_default();
        PackageDetail {
            order_ref: p.order_ref.clone().unwrap_or_default(),
            tracking_url: p.tracking_url.as_deref().and_then(safe_link),
            code_kind: p.code_kind.map(|k| k.as_str()).unwrap_or(""),
            code_expires: p
                .code_expires_at
                .map(|t| format!("{} ({})", fmt_day(t, now), relative(t, now)))
                .unwrap_or_default(),
            pickup_map: if address.is_empty() {
                String::new()
            } else {
                map_link(&address)
            },
            pickup_address: address,
            pickup_hours: p.pickup_point.hours.clone().unwrap_or_default(),
            notes: p.notes.clone().unwrap_or_default(),
            status_source: match p.status_source {
                Source::Courier => "from the courier",
                Source::Message => "from a message",
                Source::Manual => "set by hand",
            },
            archived: p.archived_at.map(|t| relative(t, now)).unwrap_or_default(),
            created: fmt_datetime(p.created_at),
            events: events.iter().map(|e| EventRow::new(e, now)).collect(),
            row,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn only_http_links_are_rendered() {
        assert_eq!(
            safe_link("https://www.ctt.pt/").as_deref(),
            Some("https://www.ctt.pt/")
        );
        assert_eq!(safe_link("javascript:alert(1)"), None);
        assert_eq!(safe_link("not a url"), None);
        assert_eq!(
            map_link("Rua A 1, Lisboa"),
            "https://www.google.com/maps/search/?api=1&query=Rua+A+1%2C+Lisboa"
        );
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
