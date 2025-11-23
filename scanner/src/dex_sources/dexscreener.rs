use crate::dex_sources::DexPriceSource;
use anyhow::Result;
use reqwest::Client;
use serde_json::Value;
use std::time::Duration;

pub struct DexScreener {
    client: Client,
    url: String,
}

impl DexScreener {
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .gzip(true)
            .brotli(true)
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            url: "https://api.dexscreener.com/latest/dex/tokens/".to_string(),
        }
    }
}

#[async_trait::async_trait]
impl DexPriceSource for DexScreener {
    fn name(&self) -> &str {
        "DexScreener"
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

        // DexScreener format: { "pairs": [{ "priceUsd": "100.5" }] }
        if let Some(pairs) = json.get("pairs").and_then(|p| p.as_array()) {
            if let Some(first_pair) = pairs.first() {
                if let Some(price_str) = first_pair.get("priceUsd").and_then(|p| p.as_str()) {
                    if let Ok(price) = price_str.parse::<f64>() {
                        return Ok(Some(price));
                    }
                }
                if let Some(price) = first_pair.get("priceUsd").and_then(|p| p.as_f64()) {
                    return Ok(Some(price));
                }
            }
        }

        Ok(None)
    }
}

