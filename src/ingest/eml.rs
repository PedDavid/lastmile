//! Exported emails (`.eml`): the sender, subject, date and text. Attachments
//! are ignored and HTML is only ever turned into text.

use jiff::Timestamp;
use mail_parser::MessageParser;

#[derive(Debug, PartialEq, Eq)]
pub struct Email {
    pub sender: Option<String>,
    pub subject: Option<String>,
    pub date: Option<Timestamp>,
    pub message_id: Option<String>,
    pub text: String,
}

pub fn parse(raw: &[u8]) -> Result<Email, String> {
    let message = MessageParser::default()
        .parse(raw)
        .ok_or("not an email (no headers found)")?;
    if message.from().is_none() && message.subject().is_none() && message.date().is_none() {
        return Err("not an email (no sender, subject or date)".into());
    }
    let text = message
        .body_text(0)
        .map(|t| t.into_owned())
        .unwrap_or_default();
    let subject = message.subject().map(str::to_string);
    if text.trim().is_empty() && subject.as_deref().is_none_or(|s| s.trim().is_empty()) {
        return Err("the email has no text".into());
    }
    Ok(Email {
        sender: message
            .from()
            .and_then(|a| a.first())
            .and_then(|a| a.address())
            .map(str::to_string),
        subject,
        date: message
            .date()
            .and_then(|d| Timestamp::from_second(d.to_timestamp()).ok()),
        message_id: message.message_id().map(str::to_string),
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_quoted_printable_html_email() {
        let raw = b"From: CTT <noreply@ctt.pt>\r\n\
Subject: =?UTF-8?Q?A_sua_encomenda_est=C3=A1_a_caminho?=\r\n\
Date: Fri, 16 Oct 2026 09:30:00 +0100\r\n\
Message-ID: <abc@ctt.pt>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/html; charset=utf-8\r\n\
Content-Transfer-Encoding: quoted-printable\r\n\
\r\n\
<p>Ol=C3=A1,</p><p>Objeto <b>RR123456789PT</b> em tr=C3=A2nsito.</p>\r\n";
        let email = parse(raw).unwrap();
        assert_eq!(email.sender.as_deref(), Some("noreply@ctt.pt"));
        assert_eq!(
            email.subject.as_deref(),
            Some("A sua encomenda está a caminho")
        );
        assert_eq!(email.message_id.as_deref(), Some("abc@ctt.pt"));
        assert_eq!(email.date, Some("2026-10-16T08:30:00Z".parse().unwrap()));
        assert!(
            email.text.contains("Objeto RR123456789PT em trânsito."),
            "{}",
            email.text
        );
        assert!(!email.text.contains('<'), "{}", email.text);
    }

    #[test]
    fn rejects_files_that_are_not_emails() {
        assert!(parse(b"").is_err());
        assert!(parse(b"\x89PNG\r\n\x1a\n").is_err());
    }
}
