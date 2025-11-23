use crate::dex_sources::DexPriceSource;
use anyhow::Result;
use reqwest::Client;
use serde_json::Value;
use std::time::Duration;

pub struct FluxBeam {
    client: Client,
    url: String,
}

impl FluxBeam {
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .gzip(true)
            .brotli(true)
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            url: "https://api.fluxbeam.xyz/price?token=".to_string(),
        }
    }
}

#[async_trait::async_trait]
impl DexPriceSource for FluxBeam {
    fn name(&self) -> &str {
        "FluxBeam"
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

        // Try common price fields
        if let Some(price) = json.get("price").and_then(|p| p.as_f64()) {
            return Ok(Some(price));
        }
        if let Some(price) = json.get("value").and_then(|p| p.as_f64()) {
            return Ok(Some(price));
        }
        if let Some(data) = json.get("data") {
            if let Some(price) = data.get("price").and_then(|p| p.as_f64()) {
                return Ok(Some(price));
            }
        }

        Ok(None)
    }
}

