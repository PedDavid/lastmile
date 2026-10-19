//! CTT's tracking page (`https://www.ctt.pt/t/<number>`), read once a browser
//! has drawn it: the dated events of its timeline, and what each says about
//! where the object is.
//!
//! The page is read as the text a visitor sees, not by its markup, which CTT
//! changes freely: every line with a date starts an event, and the words
//! after it (on the same line or the next ones) are what happened. How CTT
//! words each event is mapped to the six states in [`classify`], which is
//! where new wordings are added as they are seen.

use std::sync::LazyLock;

use jiff::Timestamp;
use jiff::civil::{Date, DateTime, Time};
use regex::Regex;
use unicode_normalization::UnicodeNormalization;

use crate::model::Status;

/// The page to open for a tracking number.
pub fn tracking_url(tracking: &str) -> String {
    format!("https://www.ctt.pt/t/{}", tracking.trim().to_uppercase())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub at: Timestamp,
    pub description: String,
    /// `None` for wordings not in the table: shown on the timeline only.
    pub status: Option<Status>,
}

/// `dd-mm-yyyy`, `dd/mm/yyyy`, `dd.mm.yyyy` or `yyyy-mm-dd`, then an optional
/// `hh:mm`.
static DATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?x)
        (?:(?P<d>\d{1,2})[-/.](?P<m>\d{1,2})[-/.](?P<y>\d{4})|(?P<iy>\d{4})-(?P<im>\d{2})-(?P<id>\d{2}))
        (?:[\sT,]+(?P<h>\d{1,2}):(?P<min>\d{2}))?",
    )
    .unwrap()
});
static TIME_ONLY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{1,2}:\d{2}$").unwrap());

/// The events on the page, oldest first. Times are read as UTC, like every
/// date in the app.
pub fn parse(text: &str) -> Vec<Event> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let mut events = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let Some(m) = DATE.captures(lines[i]) else {
            i += 1;
            continue;
        };
        let whole = m.get(0).expect("a match has a whole");
        let Some(mut at) = timestamp(&m) else {
            i += 1;
            continue;
        };
        // What follows the date on its line, then the lines up to the next
        // dated one (the time, if the date was alone, comes first).
        let mut parts: Vec<String> = Vec::new();
        let rest = format!(
            "{} {}",
            &lines[i][..whole.start()],
            &lines[i][whole.end()..]
        );
        let rest = rest
            .trim()
            .trim_matches(|c: char| "-–—:|·".contains(c) || c.is_whitespace());
        if !rest.is_empty() {
            parts.push(rest.to_string());
        }
        let mut j = i + 1;
        while j < lines.len() && DATE.find(lines[j]).is_none() && parts.len() < 3 {
            if TIME_ONLY.is_match(lines[j]) {
                if m.name("h").is_none()
                    && let Some(t) = hm(lines[j])
                {
                    at += jiff::SignedDuration::from_secs(t);
                }
            } else {
                parts.push(lines[j].to_string());
            }
            j += 1;
        }
        let description = parts.join(" – ");
        if description.chars().filter(|c| c.is_alphabetic()).count() >= 4
            && !is_estimate(&description)
        {
            let status = classify(&description);
            events.push(Event {
                at,
                description,
                status,
            });
        }
        i = j.max(i + 1);
    }
    events.sort_by_key(|e| e.at);
    events.dedup();
    events
}

/// A line that gives the expected day, which has not happened yet.
fn is_estimate(description: &str) -> bool {
    let d = fold(description);
    [
        "entrega prevista",
        "data prevista",
        "previsao de entrega",
        "expected delivery",
        "estimated delivery",
    ]
    .iter()
    .any(|w| d.starts_with(w))
}

/// The day the page says delivery is expected, if it does.
pub fn eta(text: &str) -> Option<Timestamp> {
    static ETA: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)(?:entrega\s+prevista|data\s+prevista|previs[ãa]o\s+de\s+entrega|expected\s+delivery|estimated\s+delivery)\D{0,20}?(\d{1,2}[-/.]\d{1,2}[-/.]\d{4}|\d{4}-\d{2}-\d{2})").unwrap()
    });
    let day = ETA.captures(text)?.get(1)?.as_str();
    timestamp(&DATE.captures(day)?)
}

fn hm(s: &str) -> Option<i64> {
    let (h, m) = s.split_once(':')?;
    Some(h.parse::<i64>().ok()? * 3600 + m.parse::<i64>().ok()? * 60)
}

fn timestamp(m: &regex::Captures) -> Option<Timestamp> {
    let n = |name: &str| m.name(name).and_then(|v| v.as_str().parse::<i16>().ok());
    let (year, month, day) = match (n("y"), n("iy")) {
        (Some(y), _) => (y, n("m")?, n("d")?),
        (_, Some(y)) => (y, n("im")?, n("id")?),
        _ => return None,
    };
    let date = Date::new(year, i8::try_from(month).ok()?, i8::try_from(day).ok()?).ok()?;
    let time = match (n("h"), n("min")) {
        (Some(h), Some(min)) => {
            Time::new(i8::try_from(h).ok()?, i8::try_from(min).ok()?, 0, 0).ok()?
        }
        _ => Time::midnight(),
    };
    DateTime::from_parts(date, time)
        .to_zoned(jiff::tz::TimeZone::UTC)
        .ok()
        .map(|z| z.timestamp())
}

/// Lowercase, without accents.
fn fold(s: &str) -> String {
    s.nfd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .collect::<String>()
        .to_lowercase()
}

/// What a wording of CTT's says about where the object is. The first row
/// that matches wins, so the more specific wordings come first ("not
/// delivered" before "delivered").
const WORDINGS: &[(Status, &[&str])] = &[
    (
        Status::Exception,
        &[
            "nao entregue",
            "nao foi possivel entregar",
            "tentativa de entrega",
            "entrega nao efetuada",
            "entrega sem sucesso",
            "devolvid",
            "devolucao",
            "aduana",
            "alfandega",
            "retido",
            "extraviad",
            "morada incorreta",
            "endereco incorreto",
            "destinatario ausente",
            "not delivered",
            "delivery attempt",
            "returned",
            "customs",
        ],
    ),
    (
        Status::ReadyForPickup,
        &[
            "disponivel para levantamento",
            "disponivel para recolha",
            "aguarda levantamento",
            "para levantamento",
            "pronto para levantar",
            "pronto para levantamento",
            "available for pickup",
            "ready for pickup",
        ],
    ),
    (
        Status::OutForDelivery,
        &[
            "em distribuicao",
            "saiu para entrega",
            "em entrega",
            "em curso de entrega",
            "out for delivery",
        ],
    ),
    (
        Status::Delivered,
        &[
            "entregue",
            "entrega efetuada",
            "entrega realizada",
            "delivered",
        ],
    ),
    (
        Status::Announced,
        &[
            "pre-aviso",
            "pre aviso",
            "aguarda entrada",
            "informacao recebida",
            "objeto registado",
            "registado",
            "criado",
            "shipment information received",
        ],
    ),
    (
        Status::InTransit,
        &[
            "em transito",
            "em tratamento",
            "em processamento",
            "chegada a",
            "chegou a",
            "saida de",
            "expedid",
            "rececao",
            "recebido",
            "aceite",
            "aceitacao",
            "triagem",
            "centro de distribuicao",
            "centro operacional",
            "armazem",
            "in transit",
        ],
    ),
];

pub fn classify(description: &str) -> Option<Status> {
    let d = fold(description);
    WORDINGS
        .iter()
        .find(|(_, words)| words.iter().any(|w| d.contains(w)))
        .map(|(status, _)| *status)
}

/// Whether the page has drawn what was asked for: dated events, or CTT's
/// answer that the object is not known. Until then the browser keeps waiting.
pub fn drawn(text: &str) -> bool {
    !parse(text).is_empty() || {
        let t = fold(text);
        t.contains("nao foi encontrad")
            || t.contains("nao encontrad")
            || t.contains("objeto inexistente")
            || t.contains("not found")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn reads_a_timeline_with_the_description_on_the_date_line() {
        let text = "Rastreio de objetos\nDW000000001PT\nEstado atual\n\
                    16-10-2026 09:30 Objeto aceite\n\
                    17-10-2026 14:02 - Em trânsito\n\
                    19-10-2026 08:15 Saiu para entrega\n";
        let events = parse(text);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].at, ts("2026-10-16T09:30:00Z"));
        assert_eq!(events[0].description, "Objeto aceite");
        assert_eq!(events[0].status, Some(Status::InTransit));
        assert_eq!(events[1].description, "Em trânsito");
        assert_eq!(events[2].status, Some(Status::OutForDelivery));
    }

    #[test]
    fn reads_a_timeline_with_the_date_the_time_and_the_description_on_lines_of_their_own() {
        let text = "19/10/2026\n08:15\nEntrega efetuada\nRecebido por: o destinatário\n\
                    18/10/2026\n17:40\nObjeto em distribuição\n";
        let events = parse(text);
        assert_eq!(events.len(), 2);
        // Oldest first.
        assert_eq!(events[0].at, ts("2026-10-18T17:40:00Z"));
        assert_eq!(events[0].status, Some(Status::OutForDelivery));
        assert_eq!(events[1].at, ts("2026-10-19T08:15:00Z"));
        assert_eq!(
            events[1].description,
            "Entrega efetuada – Recebido por: o destinatário"
        );
        assert_eq!(events[1].status, Some(Status::Delivered));
    }

    #[test]
    fn reads_iso_dates() {
        let events = parse("2026-10-16 09:30 Objeto aceite");
        assert_eq!(events[0].at, ts("2026-10-16T09:30:00Z"));
    }

    #[test]
    fn a_date_with_nothing_said_about_it_is_not_an_event() {
        // The expected day is not something that happened.
        assert!(parse("Entrega prevista a 19-10-2026").is_empty());
        assert!(parse("19-10-2026").is_empty());
        assert!(parse("31-02-2026 09:30 Objeto aceite").is_empty());
        assert!(parse("sem datas aqui").is_empty());
    }

    #[test]
    fn classifies_wordings_and_leaves_unknown_ones_out() {
        let c = |s: &str| classify(s);
        assert_eq!(c("Objeto entregue"), Some(Status::Delivered));
        assert_eq!(c("Entrega efetuada"), Some(Status::Delivered));
        // Not delivered is not delivered.
        assert_eq!(
            c("Objeto não entregue - destinatário ausente"),
            Some(Status::Exception)
        );
        assert_eq!(
            c("Tentativa de entrega sem sucesso"),
            Some(Status::Exception)
        );
        assert_eq!(c("Objeto devolvido ao remetente"), Some(Status::Exception));
        assert_eq!(
            c("Disponível para levantamento na Loja CTT"),
            Some(Status::ReadyForPickup)
        );
        assert_eq!(c("Objeto em distribuição"), Some(Status::OutForDelivery));
        assert_eq!(
            c("Chegada ao Centro de Distribuição"),
            Some(Status::InTransit)
        );
        assert_eq!(c("Pré-aviso recebido"), Some(Status::Announced));
        assert_eq!(c("Algo que nunca vimos"), None);
    }

    #[test]
    fn knows_when_the_page_has_been_drawn() {
        assert!(!drawn("A carregar…"));
        assert!(!drawn("Rastreio de objetos\nDW000000001PT"));
        assert!(drawn("DW000000001PT\n16-10-2026 09:30 Objeto aceite"));
        assert!(drawn("O objeto não foi encontrado"));
    }

    #[test]
    fn reads_the_expected_day() {
        assert_eq!(
            eta("Entrega prevista a 19-10-2026\nHora prevista: até às 19:00"),
            Some(ts("2026-10-19T00:00:00Z"))
        );
        assert_eq!(
            eta("Expected delivery: 2026-10-19"),
            Some(ts("2026-10-19T00:00:00Z"))
        );
        assert_eq!(eta("sem previsão"), None);
    }

    #[test]
    fn the_page_to_open() {
        assert_eq!(
            tracking_url(" dw669167148pt "),
            "https://www.ctt.pt/t/DW669167148PT"
        );
    }
}
