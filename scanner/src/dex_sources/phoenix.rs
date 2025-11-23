use crate::dex_sources::DexPriceSource;
use anyhow::Result;
use reqwest::Client;
use std::time::Duration;

pub struct Phoenix {
    client: Client,
    url: String,
}

impl Phoenix {
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .gzip(true)
            .brotli(true)
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            url: "https://phoenix.price-api.com/latest".to_string(),
        }
    }
}

#[async_trait::async_trait]
impl DexPriceSource for Phoenix {
    fn name(&self) -> &str {
        "Phoenix"
    }

    async fn get_price(&self, _token_address: &str) -> Result<Option<f64>> {
        // Phoenix API - would need to query market data
        // For now, return None - this can be enhanced with Phoenix's market API
        // TODO: Implement Phoenix market price calculation
        Ok(None)
    }
}

