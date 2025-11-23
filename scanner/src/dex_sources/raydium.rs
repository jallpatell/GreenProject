use crate::dex_sources::DexPriceSource;
use anyhow::Result;
use reqwest::Client;
use std::time::Duration;

pub struct Raydium {
    client: Client,
    url: String,
}

impl Raydium {
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .gzip(true)
            .brotli(true)
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            url: "https://api.raydium.io/v2/main/info".to_string(),
        }
    }
}

#[async_trait::async_trait]
impl DexPriceSource for Raydium {
    fn name(&self) -> &str {
        "Raydium"
    }

    async fn get_price(&self, _token_address: &str) -> Result<Option<f64>> {
        // Raydium doesn't have a direct price endpoint
        // Fallback to Birdeye or use Raydium pool data
        // For now, return None - this can be enhanced with Raydium's pool API
        // TODO: Implement Raydium pool price calculation
        Ok(None)
    }
}

