use crate::dex_sources::DexPriceSource;
use anyhow::Result;
use reqwest::Client;
use serde_json::Value;
use std::time::Duration;

pub struct Jupiter {
    client: Client,
    url: String,
}

impl Jupiter {
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .gzip(true)
            .brotli(true)
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            url: "https://price.jup.ag/v4/price?ids=".to_string(),
        }
    }
}

#[async_trait::async_trait]
impl DexPriceSource for Jupiter {
    fn name(&self) -> &str {
        "Jupiter"
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

        // Jupiter v4 format: { "data": { "ADDRESS": { "price": 100.5 } } }
        if let Some(data) = json.get("data") {
            if let Some(token_data) = data.as_object() {
                for (_, v) in token_data {
                    if let Some(price_obj) = v.get("price") {
                        if let Some(price) = price_obj.as_f64() {
                            return Ok(Some(price));
                        }
                    }
                    // Try "PRICE" field (Jupiter v4 format)
                    if let Some(price_obj) = v.get("PRICE") {
                        if let Some(price) = price_obj.get("price").and_then(|p| p.as_f64()) {
                            return Ok(Some(price));
                        }
                    }
                }
            }
        }

        Ok(None)
    }
}

