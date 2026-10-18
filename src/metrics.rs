//! Prometheus metrics, computed from the database on every scrape (a
//! household's packages are a handful of rows).

use std::collections::BTreeMap;

use jiff::Timestamp;
use prometheus_client::encoding::{DescriptorEncoder, EncodeMetric};
use prometheus_client::metrics::MetricType;
use prometheus_client::metrics::gauge::ConstGauge;
use prometheus_client::registry::Registry;

use crate::db::Db;
use crate::db::packages::{self, Scope};
use crate::model::{Package, Status};

pub struct Metrics {
    registry: Registry,
}

impl Metrics {
    pub fn new(db: Db) -> std::sync::Arc<Self> {
        let mut registry = Registry::default();
        registry.register_collector(Box::new(PackageCollector { db }));
        std::sync::Arc::new(Metrics { registry })
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
        let text = Metrics::new(Db::open_in_memory().unwrap()).encode();
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
