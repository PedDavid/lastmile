//! Packages, their status and their timeline.

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::duration::humanize;

/// Where a package is. Shown grouped into the three filters of
/// [`Filter`]; `exception` only raises the *needs attention* flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Order confirmed or label created; the courier doesn't have it yet.
    Announced,
    InTransit,
    OutForDelivery,
    /// At a locker or shop: the code is needed to collect it.
    ReadyForPickup,
    Delivered,
    /// Failed attempt, returned, held at customs.
    Exception,
}

impl Status {
    pub const ALL: [Status; 6] = [
        Status::Announced,
        Status::InTransit,
        Status::OutForDelivery,
        Status::ReadyForPickup,
        Status::Delivered,
        Status::Exception,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Announced => "announced",
            Status::InTransit => "in_transit",
            Status::OutForDelivery => "out_for_delivery",
            Status::ReadyForPickup => "ready_for_pickup",
            Status::Delivered => "delivered",
            Status::Exception => "exception",
        }
    }

    pub fn parse(s: &str) -> Option<Status> {
        Status::ALL.into_iter().find(|v| v.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Status::Announced => "Preparing",
            Status::InTransit => "In transit",
            Status::OutForDelivery => "Out for delivery",
            Status::ReadyForPickup => "Ready for pickup",
            Status::Delivered => "Delivered",
            Status::Exception => "Problem",
        }
    }

    /// Order of the normal lifecycle. Status only moves forward, except into
    /// and out of `exception`, which has no rank.
    pub fn rank(self) -> Option<u8> {
        match self {
            Status::Announced => Some(0),
            Status::InTransit => Some(1),
            Status::OutForDelivery => Some(2),
            Status::ReadyForPickup => Some(3),
            Status::Delivered => Some(4),
            Status::Exception => None,
        }
    }

    pub fn filter(self) -> Option<Filter> {
        match self {
            Status::Announced => Some(Filter::Preparing),
            Status::InTransit | Status::OutForDelivery => Some(Filter::InTransit),
            Status::ReadyForPickup | Status::Delivered => Some(Filter::Arrived),
            Status::Exception => None,
        }
    }

    /// Packages someone has to act on today come first in the list.
    pub fn is_urgent(self) -> bool {
        matches!(self, Status::ReadyForPickup | Status::OutForDelivery)
    }
}

/// The filter chips of the packages list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    Preparing,
    InTransit,
    Arrived,
    Attention,
    Archived,
}

impl Filter {
    pub const ALL: [Filter; 5] = [
        Filter::Preparing,
        Filter::InTransit,
        Filter::Arrived,
        Filter::Attention,
        Filter::Archived,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Filter::Preparing => "preparing",
            Filter::InTransit => "in-transit",
            Filter::Arrived => "arrived",
            Filter::Attention => "attention",
            Filter::Archived => "archived",
        }
    }

    pub fn parse(s: &str) -> Option<Filter> {
        Filter::ALL.into_iter().find(|f| f.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Filter::Preparing => "Preparing",
            Filter::InTransit => "In transit",
            Filter::Arrived => "Arrived",
            Filter::Attention => "Needs attention",
            Filter::Archived => "Archived",
        }
    }
}

/// What last set a package's status, or produced an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Courier,
    Message,
    Manual,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Courier => "courier",
            Source::Message => "message",
            Source::Manual => "manual",
        }
    }

    pub fn parse(s: &str) -> Option<Source> {
        [Source::Courier, Source::Message, Source::Manual]
            .into_iter()
            .find(|v| v.as_str() == s)
    }
}

/// How a message came in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// An `.eml` file dropped on the inbox.
    #[serde(alias = "email")]
    Upload,
    /// Shared from a phone: the PWA share target.
    Share,
    /// The ingest API (the iOS Shortcut).
    Api,
    /// The inbox's paste box.
    Paste,
}

impl Channel {
    pub const ALL: [Channel; 4] = [
        Channel::Upload,
        Channel::Share,
        Channel::Api,
        Channel::Paste,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Upload => "upload",
            Channel::Share => "share",
            Channel::Api => "api",
            Channel::Paste => "paste",
        }
    }

    pub fn parse(s: &str) -> Option<Channel> {
        Channel::ALL.into_iter().find(|c| c.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Channel::Upload => "Email",
            Channel::Share => "Shared",
            Channel::Api => "Shortcut",
            Channel::Paste => "Pasted",
        }
    }
}

/// What the code is for: given to the courier at the door, or typed at a
/// locker or shown at a shop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeKind {
    Delivery,
    Pickup,
}

impl CodeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            CodeKind::Delivery => "delivery",
            CodeKind::Pickup => "pickup",
        }
    }

    pub fn parse(s: &str) -> Option<CodeKind> {
        [CodeKind::Delivery, CodeKind::Pickup]
            .into_iter()
            .find(|v| v.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            CodeKind::Delivery => "Delivery code",
            CodeKind::Pickup => "Pickup code",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PickupPoint {
    pub name: Option<String>,
    pub address: Option<String>,
    pub hours: Option<String>,
}

impl PickupPoint {
    pub fn is_empty(&self) -> bool {
        self.name.is_none() && self.address.is_none() && self.hours.is_none()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    pub id: i64,
    pub title: String,
    pub merchant: Option<String>,
    /// Household member it is for.
    pub recipient: Option<String>,
    pub courier: Option<String>,
    pub tracking_number: Option<String>,
    pub order_ref: Option<String>,
    pub tracking_url: Option<String>,
    pub status: Status,
    pub status_source: Source,
    pub status_at: Timestamp,
    pub eta_from: Option<Timestamp>,
    pub eta_to: Option<Timestamp>,
    pub code: Option<String>,
    pub code_kind: Option<CodeKind>,
    pub code_expires_at: Option<Timestamp>,
    pub pickup_point: PickupPoint,
    pub notes: Option<String>,
    pub created_at: Timestamp,
    pub archived_at: Option<Timestamp>,
}

/// How close to its end a pickup has to be to need attention.
pub const PICKUP_ENDING: SignedDuration = SignedDuration::from_hours(48);

impl Package {
    /// Why someone should look at this package, if they should: a problem
    /// reported, no news for `stale_after`, or a pickup about to end.
    pub fn attention(&self, now: Timestamp, stale_after: SignedDuration) -> Option<String> {
        if self.archived_at.is_some() || self.status == Status::Delivered {
            return None;
        }
        if self.status == Status::Exception {
            return Some("Problem reported".into());
        }
        if self.status == Status::ReadyForPickup
            && let Some(end) = self.code_expires_at
        {
            let left = end.duration_since(now);
            if left.is_negative() {
                return Some("Pickup period over".into());
            }
            if left <= PICKUP_ENDING {
                return Some(format!("Pickup ends in {}", humanize(left)));
            }
        }
        let quiet = now.duration_since(self.status_at);
        if quiet >= stale_after {
            return Some(format!("No news for {}", humanize(quiet)));
        }
        None
    }

    /// Whether a status reported by a message at `at` replaces the current
    /// one: only newer news, and only forwards, except into and out of
    /// `exception`. (Manual changes skip this and always apply.)
    pub fn accepts(&self, status: Status, at: Timestamp) -> bool {
        if at < self.status_at || status == self.status {
            return false;
        }
        match (self.status.rank(), status.rank()) {
            (Some(current), Some(new)) => new > current,
            _ => true,
        }
    }

    /// The editable part of the package.
    pub fn to_new(&self) -> NewPackage {
        NewPackage {
            title: self.title.clone(),
            merchant: self.merchant.clone(),
            recipient: self.recipient.clone(),
            courier: self.courier.clone(),
            tracking_number: self.tracking_number.clone(),
            order_ref: self.order_ref.clone(),
            tracking_url: self.tracking_url.clone(),
            status: self.status,
            status_source: self.status_source,
            status_at: self.status_at,
            eta_from: self.eta_from,
            eta_to: self.eta_to,
            code: self.code.clone(),
            code_kind: self.code_kind,
            code_expires_at: self.code_expires_at,
            pickup_point: self.pickup_point.clone(),
            notes: self.notes.clone(),
        }
    }

    /// The filter chip this package is counted under (besides *needs
    /// attention*, which is a flag).
    pub fn filter(&self) -> Option<Filter> {
        if self.archived_at.is_some() {
            Some(Filter::Archived)
        } else {
            self.status.filter()
        }
    }
}

/// Everything about a package except what the database assigns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewPackage {
    pub title: String,
    pub merchant: Option<String>,
    pub recipient: Option<String>,
    pub courier: Option<String>,
    pub tracking_number: Option<String>,
    pub order_ref: Option<String>,
    pub tracking_url: Option<String>,
    pub status: Status,
    pub status_source: Source,
    pub status_at: Timestamp,
    pub eta_from: Option<Timestamp>,
    pub eta_to: Option<Timestamp>,
    pub code: Option<String>,
    pub code_kind: Option<CodeKind>,
    pub code_expires_at: Option<Timestamp>,
    pub pickup_point: PickupPoint,
    pub notes: Option<String>,
}

impl NewPackage {
    /// A package with only a title, as just announced.
    pub fn titled(title: impl Into<String>, at: Timestamp) -> NewPackage {
        NewPackage {
            title: title.into(),
            merchant: None,
            recipient: None,
            courier: None,
            tracking_number: None,
            order_ref: None,
            tracking_url: None,
            status: Status::Announced,
            status_source: Source::Manual,
            status_at: at,
            eta_from: None,
            eta_to: None,
            code: None,
            code_kind: None,
            code_expires_at: None,
            pickup_point: PickupPoint::default(),
            notes: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub id: i64,
    pub package_id: i64,
    pub at: Timestamp,
    pub status: Option<Status>,
    pub description: String,
    pub source: Source,
}

/// Sorts for the packages list: what needs collecting or someone at the door
/// first, then by ETA (unknown last), then newest first.
pub fn sort_packages(packages: &mut [Package]) {
    packages.sort_by(|a, b| {
        b.status
            .is_urgent()
            .cmp(&a.status.is_urgent())
            .then_with(|| match (a.eta_from, b.eta_from) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            })
            .then_with(|| b.created_at.cmp(&a.created_at))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for s in Status::ALL {
            assert_eq!(Status::parse(s.as_str()), Some(s));
        }
        for f in Filter::ALL {
            assert_eq!(Filter::parse(f.as_str()), Some(f));
        }
        assert_eq!(Status::parse("lost"), None);
        assert_eq!(CodeKind::parse("pickup"), Some(CodeKind::Pickup));
        assert_eq!(Source::parse("manual"), Some(Source::Manual));
        for c in Channel::ALL {
            assert_eq!(Channel::parse(c.as_str()), Some(c));
        }
    }

    #[test]
    fn every_status_but_exception_has_a_filter() {
        for s in Status::ALL {
            assert_eq!(s.filter().is_none(), s == Status::Exception, "{s:?}");
            assert_eq!(s.rank().is_none(), s == Status::Exception, "{s:?}");
        }
    }

    #[test]
    fn attention() {
        let now: Timestamp = "2026-10-18T12:00:00Z".parse().unwrap();
        let week = SignedDuration::from_hours(7 * 24);
        let ago = |h| now - SignedDuration::from_hours(h);
        let p = |status, status_at| Package {
            status,
            status_at,
            ..sample(now)
        };
        assert_eq!(p(Status::InTransit, ago(24)).attention(now, week), None);
        assert_eq!(
            p(Status::InTransit, ago(9 * 24))
                .attention(now, week)
                .as_deref(),
            Some("No news for 9d")
        );
        assert_eq!(
            p(Status::Exception, ago(1)).attention(now, week).as_deref(),
            Some("Problem reported")
        );
        // Delivered and archived packages are done, however old.
        assert_eq!(p(Status::Delivered, ago(900)).attention(now, week), None);
        let archived = Package {
            archived_at: Some(now),
            ..p(Status::InTransit, ago(900))
        };
        assert_eq!(archived.attention(now, week), None);
        assert_eq!(archived.filter(), Some(Filter::Archived));

        let pickup = |hours_left| Package {
            code_expires_at: Some(now + SignedDuration::from_hours(hours_left)),
            ..p(Status::ReadyForPickup, ago(1))
        };
        assert_eq!(pickup(72).attention(now, week), None);
        assert_eq!(
            pickup(30).attention(now, week).as_deref(),
            Some("Pickup ends in 1d")
        );
        assert_eq!(
            pickup(-1).attention(now, week).as_deref(),
            Some("Pickup period over")
        );
    }

    #[test]
    fn messages_only_move_status_forward() {
        let now: Timestamp = "2026-10-18T12:00:00Z".parse().unwrap();
        let later = now + SignedDuration::from_hours(1);
        let earlier = now - SignedDuration::from_hours(1);
        let p = |status| Package {
            status,
            status_at: now,
            ..sample(now)
        };
        assert!(p(Status::InTransit).accepts(Status::ReadyForPickup, later));
        assert!(!p(Status::ReadyForPickup).accepts(Status::InTransit, later));
        assert!(
            !p(Status::InTransit).accepts(Status::Delivered, earlier),
            "old news"
        );
        assert!(!p(Status::InTransit).accepts(Status::InTransit, later));
        assert!(p(Status::InTransit).accepts(Status::Exception, later));
        assert!(p(Status::Exception).accepts(Status::InTransit, later));
        assert_eq!(p(Status::Delivered).to_new().status, Status::Delivered);
    }

    #[test]
    fn urgent_packages_sort_first_then_by_eta() {
        let at: Timestamp = "2026-10-18T12:00:00Z".parse().unwrap();
        let mk = |id, status, eta: Option<&str>| Package {
            id,
            status,
            eta_from: eta.map(|e| e.parse().unwrap()),
            ..sample(at)
        };
        let mut list = vec![
            mk(1, Status::InTransit, None),
            mk(2, Status::InTransit, Some("2026-10-20T00:00:00Z")),
            mk(3, Status::ReadyForPickup, None),
            mk(4, Status::Announced, Some("2026-10-19T00:00:00Z")),
            mk(5, Status::OutForDelivery, Some("2026-10-18T00:00:00Z")),
        ];
        sort_packages(&mut list);
        let ids: Vec<i64> = list.iter().map(|p| p.id).collect();
        assert_eq!(ids, [5, 3, 4, 2, 1]);
    }

    fn sample(at: Timestamp) -> Package {
        Package {
            id: 0,
            title: "x".into(),
            merchant: None,
            recipient: None,
            courier: None,
            tracking_number: None,
            order_ref: None,
            tracking_url: None,
            status: Status::Announced,
            status_source: Source::Manual,
            status_at: at,
            eta_from: None,
            eta_to: None,
            code: None,
            code_kind: None,
            code_expires_at: None,
            pickup_point: PickupPoint::default(),
            notes: None,
            created_at: at,
            archived_at: None,
        }
    }
}
