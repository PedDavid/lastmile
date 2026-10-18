//! Prometheus metrics. Gauges are computed from the database on every
//! scrape (a household's packages are a handful of rows); counters track
//! incoming messages.

use std::collections::BTreeMap;

use jiff::Timestamp;
use prometheus_client::encoding::{DescriptorEncoder, EncodeLabelSet, EncodeMetric};
use prometheus_client::metrics::MetricType;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::ConstGauge;
use prometheus_client::registry::Registry;

use crate::db::messages::{self, MessageResult};
use crate::db::packages::{self, Scope};
use crate::db::{Db, rules};
use crate::model::{Channel, Package, Status};

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct MessageLabels {
    pub channel: String,
    pub result: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct ResultLabels {
    pub result: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct KindLabels {
    pub kind: String,
}

pub struct Metrics {
    registry: Registry,
    messages: Family<MessageLabels, Counter>,
    llm_requests: Family<ResultLabels, Counter>,
    llm_tokens: Family<KindLabels, Counter>,
}

impl Metrics {
    pub fn new(db: Db) -> std::sync::Arc<Self> {
        let mut registry = Registry::default();
        let messages = Family::<MessageLabels, Counter>::default();
        // Every series starts at 0: `increase()` ignores a counter's first
        // sample, so one that first appears at 1 would hide a message.
        for channel in Channel::ALL {
            for result in MessageResult::ALL {
                let _ = messages.get_or_create(&MessageLabels {
                    channel: channel.as_str().into(),
                    result: result.as_str().into(),
                });
            }
        }
        registry.register(
            "lastmile_messages",
            "Messages received, by channel and what the rules made of them",
            messages.clone(),
        );
        let llm_requests = Family::<ResultLabels, Counter>::default();
        for result in ["ok", "error"] {
            let _ = llm_requests.get_or_create(&ResultLabels {
                result: result.into(),
            });
        }
        let llm_tokens = Family::<KindLabels, Counter>::default();
        for kind in ["input", "output"] {
            let _ = llm_tokens.get_or_create(&KindLabels { kind: kind.into() });
        }
        registry.register(
            "lastmile_llm_requests",
            "Requests to the model that drafts rules, by result",
            llm_requests.clone(),
        );
        registry.register(
            "lastmile_llm_tokens",
            "Tokens used by the model that drafts rules, by kind (input, output)",
            llm_tokens.clone(),
        );
        registry.register_collector(Box::new(PackageCollector { db }));
        std::sync::Arc::new(Metrics {
            registry,
            messages,
            llm_requests,
            llm_tokens,
        })
    }

    /// One request to the model.
    pub fn llm_call(&self, ok: bool, input_tokens: u64, output_tokens: u64) {
        self.llm_requests
            .get_or_create(&ResultLabels {
                result: if ok { "ok" } else { "error" }.into(),
            })
            .inc();
        for (kind, n) in [("input", input_tokens), ("output", output_tokens)] {
            self.llm_tokens
                .get_or_create(&KindLabels { kind: kind.into() })
                .inc_by(n);
        }
    }

    pub fn message(&self, channel: Channel, result: MessageResult) {
        self.messages
            .get_or_create(&MessageLabels {
                channel: channel.as_str().into(),
                result: result.as_str().into(),
            })
            .inc();
    }

    /// Runs the database queries: call it from a blocking context.
    pub fn encode(&self) -> String {
        let mut out = String::new();
        prometheus_client::encoding::text::encode(&mut out, &self.registry)
            .expect("writing to a String cannot fail");
        out
    }
}

struct PackageCollector {
    db: Db,
}

impl std::fmt::Debug for PackageCollector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackageCollector").finish_non_exhaustive()
    }
}

fn seconds(t: Timestamp) -> f64 {
    t.as_millisecond() as f64 / 1000.0
}

fn courier(p: &Package) -> String {
    p.courier.clone().unwrap_or_default()
}

impl prometheus_client::collector::Collector for PackageCollector {
    fn encode(&self, mut encoder: DescriptorEncoder) -> Result<(), std::fmt::Error> {
        let active = match self.db.blocking(|c| packages::list(c, Scope::Active)) {
            Ok(list) => list,
            Err(e) => {
                tracing::error!(error = %e, "metrics: reading packages failed");
                return Ok(());
            }
        };

        // Every status is exported, at 0 when empty, so alerts and graphs
        // see a series even before the first package of that kind.
        let mut counts: BTreeMap<(&'static str, String), u64> = BTreeMap::new();
        for p in &active {
            *counts.entry((p.status.as_str(), courier(p))).or_default() += 1;
        }
        for s in Status::ALL {
            if !counts.keys().any(|(status, _)| *status == s.as_str()) {
                counts.insert((s.as_str(), String::new()), 0);
            }
        }
        {
            let mut metric = encoder.encode_descriptor(
                "lastmile_packages",
                "Active (non-archived) packages by status and courier",
                None,
                MetricType::Gauge,
            )?;
            for ((status, courier), n) in &counts {
                let labels = [("status", status.to_string()), ("courier", courier.clone())];
                ConstGauge::new(*n as i64).encode(metric.encode_family(&labels)?)?;
            }
        }

        let ready: Vec<&Package> = active
            .iter()
            .filter(|p| p.status == Status::ReadyForPickup)
            .collect();
        {
            let mut metric = encoder.encode_descriptor(
                "lastmile_package_ready_since_timestamp_seconds",
                "When each package waiting at a pickup point got there",
                None,
                MetricType::Gauge,
            )?;
            for p in &ready {
                let labels = [("id", p.id.to_string()), ("courier", courier(p))];
                ConstGauge::new(seconds(p.status_at)).encode(metric.encode_family(&labels)?)?;
            }
        }
        let inbox = self
            .db
            .blocking(|c| messages::inbox_counts(c))
            .unwrap_or_default();
        {
            let mut metric = encoder.encode_descriptor(
                "lastmile_inbox_messages",
                "Messages waiting in the inbox, by result",
                None,
                MetricType::Gauge,
            )?;
            for result in ["unmatched", "error", "matched"] {
                let n = inbox
                    .iter()
                    .find(|(r, _)| r == result)
                    .map_or(0, |(_, n)| *n);
                let labels = [("result", result.to_string())];
                ConstGauge::new(n).encode(metric.encode_family(&labels)?)?;
            }
        }
        let rule_counts = self.db.blocking(|c| rules::counts(c)).unwrap_or_default();
        {
            let mut metric = encoder.encode_descriptor(
                "lastmile_rules",
                "Rules by origin and whether they are enabled",
                None,
                MetricType::Gauge,
            )?;
            for (origin, enabled, n) in &rule_counts {
                let labels = [("origin", origin.clone()), ("enabled", enabled.to_string())];
                ConstGauge::new(*n).encode(metric.encode_family(&labels)?)?;
            }
        }
        {
            let mut metric = encoder.encode_descriptor(
                "lastmile_package_code_expiry_timestamp_seconds",
                "When the pickup period of a waiting package ends, if known",
                None,
                MetricType::Gauge,
            )?;
            for p in &ready {
                if let Some(end) = p.code_expires_at {
                    let labels = [("id", p.id.to_string())];
                    ConstGauge::new(seconds(end)).encode(metric.encode_family(&labels)?)?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_package_gauges() {
        let db = Db::open_in_memory().unwrap();
        let now: Timestamp = "2026-10-18T12:00:00Z".parse().unwrap();
        db.blocking(|c| crate::demo::load(c, now)).unwrap();
        let text = Metrics::new(db).encode();
        assert!(
            text.contains(r#"lastmile_packages{status="ready_for_pickup",courier="CTT"} 1"#),
            "{text}"
        );
        assert!(
            text.contains(r#"lastmile_packages{status="exception",courier="GLS"} 1"#),
            "{text}"
        );
        // The archived lamp is not counted; delivered ink still is.
        assert!(
            text.contains(r#"lastmile_packages{status="delivered",courier="Paack"} 1"#),
            "{text}"
        );
        assert!(
            !text.contains(r#"status="delivered",courier="CTT Expresso""#),
            "{text}"
        );
        // 20h before now.
        assert!(
            text.contains(
                r#"lastmile_package_ready_since_timestamp_seconds{id="1",courier="CTT"} 1792252800"#
            ),
            "{text}"
        );
        assert!(
            text.contains(r#"lastmile_package_code_expiry_timestamp_seconds{id="1"} 1792800000"#),
            "{text}"
        );
    }

    #[test]
    fn empty_statuses_are_exported_at_zero() {
        let metrics = Metrics::new(Db::open_in_memory().unwrap());
        metrics.message(Channel::Share, MessageResult::Matched);
        let text = metrics.encode();
        assert!(
            text.contains(r#"lastmile_messages_total{channel="share",result="matched"} 1"#),
            "{text}"
        );
        assert!(
            text.contains(r#"lastmile_messages_total{channel="upload",result="error"} 0"#),
            "{text}"
        );
        assert!(
            text.contains(r#"lastmile_inbox_messages{result="unmatched"} 0"#),
            "{text}"
        );
        for s in Status::ALL {
            assert!(
                text.contains(&format!(
                    r#"lastmile_packages{{status="{}",courier=""}} 0"#,
                    s.as_str()
                )),
                "{text}"
            );
        }
    }
}
