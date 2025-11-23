use crate::dex_sources::DexPriceSource;
use anyhow::Result;
use reqwest::Client;
use serde_json::Value;
use std::time::Duration;

pub struct Birdeye {
    client: Client,
    url: String,
}

impl Birdeye {
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .gzip(true)
            .brotli(true)
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            url: "https://public-api.birdeye.so/public/price?address=".to_string(),
        }
    }
}

#[async_trait::async_trait]
impl DexPriceSource for Birdeye {
    fn name(&self) -> &str {
        "Birdeye"
    }

    async fn get_price(&self, token_address: &str) -> Result<Option<f64>> {
        let url = format!("{}{}", self.url, token_address);
        
        let response = match self.client.get(&url).send().await {
            Ok(resp) => resp,
            Err(_) => return Ok(None),
        };

        if !response.status().is_success() {
            return Ok(None);
        }

        let json: Value = match response.json().await {
            Ok(json) => json,
            Err(_) => return Ok(None),
        };

        // Birdeye format: { "data": { "value": 100.5 } }
        if let Some(data) = json.get("data") {
            if let Some(value) = data.get("value") {
                if let Some(price) = value.as_f64() {
                    return Ok(Some(price));
                }
            }
        }

        Ok(None)
    }
}

