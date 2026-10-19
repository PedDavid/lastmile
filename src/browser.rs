//! A client for a headless Chromium's DevTools endpoint, to read pages that
//! are web apps (a courier's tracking page has no content in its HTML: the
//! page's own scripts fetch and draw it). It loads a page, waits until the
//! text on it is what the caller wants, and returns that text.
//!
//! Chromium answers its DevTools HTTP endpoint only when the `Host` header
//! is `localhost` or an IP address, so a service name is resolved here and the
//! address is used instead.

use std::net::IpAddr;
use std::time::{Duration, Instant};

use chromiumoxide::browser::Browser;
use futures::StreamExt;
use url::{Host, Url};

use crate::config::BrowserConfig;

/// What the page announces itself as: a fixed name for the project, so a
/// courier can see who is asking.
pub const USER_AGENT: &str =
    "lastmile (+https://github.com/PedDavid/lastmile; checks a household's own parcels)";

#[derive(Debug, thiserror::Error)]
pub enum BrowserError {
    #[error("could not reach the browser: {0}")]
    Connect(String),
    #[error("the browser failed: {0}")]
    Browser(String),
    #[error("the page did not show what was expected within {0:?}")]
    Timeout(Duration),
}

pub struct Client {
    endpoint: Url,
    timeout: Duration,
}

impl Client {
    /// `None` when no browser is configured.
    pub fn from_config(cfg: &BrowserConfig) -> Option<Client> {
        let endpoint = cfg.browser_url.clone()?;
        Some(Client {
            endpoint,
            timeout: Duration::from_secs(cfg.browser_timeout.as_secs().max(1) as u64),
        })
    }

    /// Where the browser is, for the log.
    pub fn host(&self) -> String {
        self.endpoint.origin().ascii_serialization()
    }

    /// The endpoint with its host as an address, which is what Chromium
    /// accepts.
    async fn endpoint_by_address(&self) -> Result<String, BrowserError> {
        let mut url = self.endpoint.clone();
        let port = url.port_or_known_default().unwrap_or(9222);
        if let Some(Host::Domain(name)) = url.host()
            && name != "localhost"
        {
            let addr: IpAddr = tokio::net::lookup_host((name, port))
                .await
                .map_err(|e| BrowserError::Connect(format!("{name}: {e}")))?
                .map(|a| a.ip())
                // IPv4 first: the endpoint is often listening on that only.
                .min_by_key(|ip| ip.is_ipv6())
                .ok_or_else(|| BrowserError::Connect(format!("{name}: no address")))?;
            url.set_ip_host(addr)
                .map_err(|()| BrowserError::Connect("not a usable address".into()))?;
        }
        Ok(url.as_str().trim_end_matches('/').to_string())
    }

    /// Loads `url` and returns the text of the page once `ready` says it is
    /// the text wanted and has stopped changing, or an error at the timeout.
    /// The page is closed afterwards, and nothing else in the browser is
    /// touched.
    pub async fn render(
        &self,
        url: &str,
        ready: impl Fn(&str) -> bool,
    ) -> Result<String, BrowserError> {
        let deadline = Instant::now() + self.timeout;
        let endpoint = self.endpoint_by_address().await?;
        let (browser, mut handler) = tokio::time::timeout(self.timeout, Browser::connect(endpoint))
            .await
            .map_err(|_| BrowserError::Timeout(self.timeout))?
            .map_err(|e| BrowserError::Connect(e.to_string()))?;
        let driver = tokio::spawn(async move { while handler.next().await.is_some() {} });
        let result = self.read(&browser, url, deadline, ready).await;
        driver.abort();
        result
    }

    async fn read(
        &self,
        browser: &Browser,
        url: &str,
        deadline: Instant,
        ready: impl Fn(&str) -> bool,
    ) -> Result<String, BrowserError> {
        let fail = |e: chromiumoxide::error::CdpError| BrowserError::Browser(e.to_string());
        let page = browser.new_page("about:blank").await.map_err(fail)?;
        let outcome = async {
            page.set_user_agent(USER_AGENT).await.map_err(fail)?;
            page.goto(url).await.map_err(fail)?;
            // The page's scripts draw it after the load: poll its text until
            // it is what is wanted and has stopped changing.
            let mut last = String::new();
            let mut steady = 0;
            loop {
                let text: String = page
                    .evaluate("document.body ? document.body.innerText : ''")
                    .await
                    .map_err(fail)?
                    .into_value()
                    .map_err(|e| BrowserError::Browser(e.to_string()))?;
                if !text.trim().is_empty() && text == last {
                    steady += 1;
                } else {
                    steady = 0;
                }
                if steady >= 2 && ready(&text) {
                    return Ok(text);
                }
                last = text;
                if Instant::now() >= deadline {
                    return Err(BrowserError::Timeout(self.timeout));
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
        .await;
        let _ = page.close().await;
        outcome
    }
}
