use crate::dex_sources::DexPriceSource;
use anyhow::Result;
use reqwest::Client;
use std::time::Duration;

pub struct Orca {
    client: Client,
    url: String,
}

impl Orca {
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .gzip(true)
            .brotli(true)
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            url: "https://api.mainnet.orca.so/v1/whirlpool/list".to_string(),
        }
    }
}

#[async_trait::async_trait]
impl DexPriceSource for Orca {
    fn name(&self) -> &str {
        "Orca"
    }

    async fn get_price(&self, _token_address: &str) -> Result<Option<f64>> {
        // Orca API doesn't have a direct price endpoint, so we'll use Birdeye as fallback
        // For now, return None - this can be enhanced with Orca's pool data API
        // TODO: Implement Orca pool price calculation
        Ok(None)
    }
}

