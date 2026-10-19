//! The analyzer client against a real Presidio service. It needs one, so it
//! only runs when asked to:
//!
//! ```sh
//! docker run -d -p 5002:3000 mcr.microsoft.com/presidio-analyzer:2.2.362
//! LASTMILE_TEST_PRESIDIO_URL=http://localhost:5002 cargo test --test analyzer -- --ignored
//! ```

use clap::Parser;
use lastmile::analyzer::Client;
use lastmile::config::Config;
use lastmile::redact::{Household, Options, redact_with};

#[tokio::test]
#[ignore = "needs a Presidio analyzer: set LASTMILE_TEST_PRESIDIO_URL"]
async fn a_real_analyzer_finds_a_name_that_is_not_in_the_household() {
    let url = std::env::var("LASTMILE_TEST_PRESIDIO_URL").expect("LASTMILE_TEST_PRESIDIO_URL");
    let cfg = Config::try_parse_from(["lastmile", "--presidio-url", &url]).unwrap();
    let client = Client::from_config(&cfg.analyzer).unwrap().unwrap();
    let text = "Paack: Olá Ana Ferreira, a sua encomenda PCK7Q2W9D será entregue hoje. Levantar com Rui Costa. Código de entrega 5527.";
    let found = client.analyze(text).await.unwrap();
    let household = Household {
        names: vec!["Ana Ferreira".into()],
        ..Default::default()
    };
    let redacted = redact_with(text, &household, Options::default(), &found).text;
    assert!(
        !redacted.contains("Rui") && !redacted.contains("Costa"),
        "{redacted}"
    );
    assert!(!redacted.contains("Ferreira"), "{redacted}");
    // What a rule needs is still there.
    assert!(
        redacted.contains("PCK7Q2W9D") && redacted.contains("5527"),
        "{redacted}"
    );
}
