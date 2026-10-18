//! What a rule says, as written in the editor (YAML) and stored (JSON).
//! Rules are data, never code: this fixed schema plus Rust `regex`
//! patterns, which run in linear time.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::{Channel, CodeKind, Status};

/// The fields a rule can extract from a message, each with a regex whose
/// named group `v` is the value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    Title,
    Merchant,
    TrackingNumber,
    OrderRef,
    TrackingUrl,
    Code,
    /// The last day to collect it, as a date.
    CodeExpiresAt,
    PickupPoint,
    PickupAddress,
    PickupHours,
    /// The expected delivery day, as a date.
    Eta,
}

impl Field {
    pub const ALL: [Field; 11] = [
        Field::Title,
        Field::Merchant,
        Field::TrackingNumber,
        Field::OrderRef,
        Field::TrackingUrl,
        Field::Code,
        Field::CodeExpiresAt,
        Field::PickupPoint,
        Field::PickupAddress,
        Field::PickupHours,
        Field::Eta,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Field::Title => "title",
            Field::Merchant => "merchant",
            Field::TrackingNumber => "tracking_number",
            Field::OrderRef => "order_ref",
            Field::TrackingUrl => "tracking_url",
            Field::Code => "code",
            Field::CodeExpiresAt => "code_expires_at",
            Field::PickupPoint => "pickup_point",
            Field::PickupAddress => "pickup_address",
            Field::PickupHours => "pickup_hours",
            Field::Eta => "eta",
        }
    }

    /// Fields holding a date, normalised to `YYYY-MM-DD` when extracted.
    pub fn is_date(self) -> bool {
        matches!(self, Field::CodeExpiresAt | Field::Eta)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Match {
    /// Channels the rule applies to; all if empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<Channel>,
    /// Sender patterns (`*` is a wildcard, case is ignored), checked only
    /// when the message has a sender: shared texts don't.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sender: Vec<String>,
    /// Regexes that must all match the text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub all: Vec<String>,
    /// Regexes none of which may match.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub none: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Set {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_kind: Option<CodeKind>,
    /// A fixed merchant, for messages that come from one shop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merchant: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RuleDef {
    /// Short stable name, e.g. `ctt-ready-for-pickup`.
    pub slug: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The courier of the packages it reads, as shown in the app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub courier: Option<String>,
    #[serde(rename = "match")]
    pub matcher: Match,
    /// Field → regex with a named group `v`; `null` for nothing.
    #[serde(default)]
    pub extract: BTreeMap<Field, Option<String>>,
    #[serde(default)]
    pub set: Set,
    /// The highest-priority matching rule wins; ties go to the most
    /// specific one (most conditions).
    #[serde(default)]
    pub priority: i64,
    /// The message is about a package seen before but may not name it
    /// ("your parcel is waiting"): without a tracking number or order
    /// reference it goes to the inbox to be attached, instead of erroring.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub follow_up: bool,
}

impl RuleDef {
    pub fn from_yaml(source: &str) -> Result<RuleDef, String> {
        serde_yaml::from_str(source).map_err(|e| e.to_string())
    }

    pub fn to_yaml(&self) -> String {
        serde_yaml::to_string(self).expect("rules always serialise")
    }
}

/// The template a new rule starts from.
pub const NEW_RULE: &str = r#"slug: courier-what-it-says
name: Courier – what the message says
courier: CTT
match:
  # Leave out to match every channel: upload (emails), share, api, paste.
  channels: [share, api, paste]
  # Regexes (Rust syntax: no look-around or backreferences).
  all: ['(?i)disponível para levantamento']
  none: ['(?i)devolvid']
extract:
  # The named group `v` is the value.
  tracking_number: '\b(?P<v>[A-Z]{2}\d{9}PT)\b'
  code: '(?i)código[^0-9]{0,20}(?P<v>\d{6})'
set:
  status: ready_for_pickup
  codeKind: pickup
priority: 100
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_parses() {
        let def = RuleDef::from_yaml(NEW_RULE).unwrap();
        assert_eq!(def.set.status, Some(Status::ReadyForPickup));
        assert_eq!(def.set.code_kind, Some(CodeKind::Pickup));
        assert_eq!(def.matcher.channels.len(), 3);
        assert!(def.extract[&Field::TrackingNumber].is_some());
        // And survives a round trip through the stored form.
        let json = serde_json::to_string(&def).unwrap();
        assert_eq!(serde_json::from_str::<RuleDef>(&json).unwrap(), def);
        assert_eq!(RuleDef::from_yaml(&def.to_yaml()).unwrap(), def);
    }

    #[test]
    fn unknown_keys_are_errors() {
        let err = RuleDef::from_yaml("slug: a\nname: b\nmatch: {}\nprioriy: 3\n").unwrap_err();
        assert!(err.contains("prioriy"), "{err}");
        let err =
            RuleDef::from_yaml("slug: a\nname: b\nmatch: {}\nextract: {colour: x}\n").unwrap_err();
        assert!(err.contains("colour"), "{err}");
    }

    #[test]
    fn email_is_another_name_for_uploads() {
        let def = RuleDef::from_yaml("slug: a\nname: b\nmatch: {channels: [email]}\n").unwrap();
        assert_eq!(def.matcher.channels, [Channel::Upload]);
        assert!(def.extract.is_empty());
        assert_eq!(Field::ALL.len(), 11);
        assert!(Field::ALL.iter().all(|f| {
            f.as_str()
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '_')
        }));
    }
}
