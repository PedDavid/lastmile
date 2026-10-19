//! A client for a Presidio analyzer service: it reads a message and says
//! where the names and places are, which the household values and the
//! patterns of [`crate::redact`] cannot know. What it finds is replaced with
//! the same placeholders. It only ever suggests redactions on the draft page,
//! which the admin reads before anything is sent.
//!
//! The analyzer is sent the original message, so it belongs in the same
//! cluster; the chart can start one (`presidio.enabled`).

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::AnalyzerConfig;
use crate::redact::{Found, Kind};

#[derive(Debug, thiserror::Error)]
pub enum AnalyzeError {
    #[error("could not reach the analyzer: {0}")]
    Request(String),
    #[error("the analyzer answered {0}")]
    Status(u16),
    #[error("unexpected reply from the analyzer: {0}")]
    Reply(String),
}

pub struct Client {
    http: reqwest::Client,
    url: String,
    language: String,
    entities: Vec<String>,
    score: f32,
}

#[derive(Serialize)]
struct Request<'a> {
    text: &'a str,
    language: &'a str,
    entities: &'a [String],
    score_threshold: f32,
}

#[derive(Deserialize)]
struct Finding {
    entity_type: String,
    /// Positions in characters, not bytes.
    start: usize,
    end: usize,
}

impl Client {
    /// `None` when no analyzer is configured.
    pub fn from_config(cfg: &AnalyzerConfig) -> anyhow::Result<Option<Client>> {
        let Some(url) = &cfg.presidio_url else {
            return Ok(None);
        };
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(
                cfg.presidio_timeout.as_secs().max(1) as u64
            ))
            .build()?;
        Ok(Some(Client {
            http,
            url: format!("{}/analyze", url.as_str().trim_end_matches('/')),
            language: cfg.presidio_language.clone(),
            entities: cfg.presidio_entities.clone(),
            score: cfg.presidio_score,
        }))
    }

    /// Where requests go, without the path: shown to the admin.
    pub fn host(&self) -> String {
        url::Url::parse(&self.url)
            .map(|u| u.origin().ascii_serialization())
            .unwrap_or_else(|_| self.url.clone())
    }

    /// What the analyzer finds in `text`, as byte ranges of it.
    pub async fn analyze(&self, text: &str) -> Result<Vec<Found>, AnalyzeError> {
        let res = self
            .http
            .post(&self.url)
            .json(&Request {
                text,
                language: &self.language,
                entities: &self.entities,
                score_threshold: self.score,
            })
            .send()
            .await
            .map_err(|e| AnalyzeError::Request(e.without_url().to_string()))?;
        if !res.status().is_success() {
            return Err(AnalyzeError::Status(res.status().as_u16()));
        }
        let findings: Vec<Finding> = res
            .json()
            .await
            .map_err(|e| AnalyzeError::Reply(e.without_url().to_string()))?;
        Ok(to_found(text, &findings))
    }
}

/// What a Presidio entity type becomes here. Types that are already found
/// by the patterns, or that a rule needs (organisations, dates), are left.
fn kind(entity: &str) -> Option<Kind> {
    Some(match entity {
        "PERSON" => Kind::Name,
        "LOCATION" => Kind::Address,
        "PHONE_NUMBER" => Kind::Phone,
        "EMAIL_ADDRESS" => Kind::Email,
        "IBAN_CODE" => Kind::Iban,
        "CREDIT_CARD" => Kind::Card,
        _ => return None,
    })
}

fn to_found(text: &str, findings: &[Finding]) -> Vec<Found> {
    // Character index → byte offset, with one past the end for a span that
    // stops at the end of the text.
    let offsets: Vec<usize> = text
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()))
        .collect();
    findings
        .iter()
        .filter_map(|f| {
            let kind = kind(&f.entity_type)?;
            let start = *offsets.get(f.start)?;
            let end = *offsets.get(f.end)?;
            (start < end).then_some(Found { start, end, kind })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(entity: &str, start: usize, end: usize) -> Finding {
        Finding {
            entity_type: entity.into(),
            start,
            end,
        }
    }

    #[test]
    fn positions_in_characters_become_byte_ranges() {
        // "á" and "ç" are two bytes each.
        let text = "Olá Joana Conceição, aqui";
        let found = to_found(text, &[finding("PERSON", 4, 19)]);
        assert_eq!(found.len(), 1);
        assert_eq!(&text[found[0].start..found[0].end], "Joana Conceição");
        assert_eq!(found[0].kind, Kind::Name);
    }

    #[test]
    fn a_span_may_end_at_the_end_of_the_text() {
        let text = "Entregue a Rui";
        let found = to_found(text, &[finding("PERSON", 11, 14)]);
        assert_eq!(&text[found[0].start..found[0].end], "Rui");
    }

    #[test]
    fn nonsense_and_unwanted_types_are_dropped() {
        let text = "Olá Rui";
        let found = to_found(
            text,
            &[
                finding("PERSON", 4, 99),
                finding("PERSON", 5, 5),
                finding("PERSON", 6, 4),
                finding("ORGANIZATION", 0, 3),
                finding("DATE_TIME", 0, 3),
            ],
        );
        assert!(found.is_empty());
    }

    #[test]
    fn locations_are_addresses() {
        let found = to_found("em Lisboa", &[finding("LOCATION", 3, 9)]);
        assert_eq!(found[0].kind, Kind::Address);
    }
}
