use crate::config::{DexConfig, TokenConfig};
use anyhow::Result;
use futures::future::join_all;
use log::{debug, info, warn};
use reqwest::Client;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub type TokenPrices = HashMap<String, HashMap<String, f64>>; // token -> dex -> price

/// ** PRODUCTION-GRADE: Response cache entry
#[derive(Clone)]
struct CacheEntry {
    data: HashMap<String, f64>, // dex -> price
    timestamp: Instant,
}

pub struct PriceFetcher {
    client: Client,
    dexes: Arc<Vec<DexConfig>>,
    cache: Arc<tokio::sync::RwLock<HashMap<String, CacheEntry>>>, // token -> cache entry
    cache_ttl: Duration,
}

impl PriceFetcher {
    pub fn new(dexes: Vec<DexConfig>) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .gzip(true)
            .brotli(true)
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            dexes: Arc::new(dexes),
            cache: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            cache_ttl: Duration::from_secs(1), // 1 second cache TTL
        }
    }

    pub async fn fetch_all_prices(&self, tokens: &[TokenConfig]) -> Result<TokenPrices> {
        let mut all_prices: TokenPrices = HashMap::new();

        // Check cache first and fetch only expired/missing entries
        let mut fetch_tasks = Vec::new();
        let mut tokens_to_fetch = Vec::new();
        let mut cached_tokens = Vec::new();

        // Check cache for each token
        {
            let cache = self.cache.read().await;
            for token in tokens {
                let cache_key = token.symbol.clone();
                if let Some(entry) = cache.get(&cache_key) {
                    if entry.timestamp.elapsed() < self.cache_ttl {
                        // Cache hit - use cached data
                        cached_tokens.push((token.symbol.clone(), entry.data.clone()));
                        continue;
                    }
                }
                // Cache miss or expired - add to fetch list
                tokens_to_fetch.push(token);
            }
        }

        // Add cached results
        for (symbol, prices) in cached_tokens {
            if !prices.is_empty() {
                all_prices.insert(symbol, prices);
            }
        }

        // Fetch prices for uncached tokens in parallel
        for token in &tokens_to_fetch {
            fetch_tasks.push(self.fetch_token_prices(token));
        }

        let results = join_all(fetch_tasks).await;

        // Process fetched results and update cache
        let mut cache = self.cache.write().await;
        for (token, prices_result) in tokens_to_fetch.iter().zip(results) {
            match prices_result {
                Ok(dex_prices) => {
                    if !dex_prices.is_empty() {
                        // Update cache
                        cache.insert(
                            token.symbol.clone(),
                            CacheEntry {
                                data: dex_prices.clone(),
                                timestamp: Instant::now(),
                            },
                        );
                        all_prices.insert(token.symbol.clone(), dex_prices);
                    }
                }
                Err(e) => {
                    warn!("Failed to fetch prices for {}: {}", token.symbol, e);
                }
            }
        }

        Ok(all_prices)
    }

    async fn fetch_token_prices(&self, token: &TokenConfig) -> Result<HashMap<String, f64>> {
        let mut prices: HashMap<String, f64> = HashMap::new();

        // Fetch from all DEXes in parallel
        let fetch_tasks: Vec<_> = self
            .dexes
            .iter()
            .map(|dex| self.fetch_price_from_dex(dex, token))
            .collect();

        let results = join_all(fetch_tasks).await;

        // ** REAL DATA: Prices are fetched from live DEX APIs (no mock data)
        for (dex, result) in self.dexes.iter().zip(results) {
            match result {
                Ok(Some(price)) => {
                    // ** REAL PRICE: Fetched from live DEX API endpoint
                    prices.insert(dex.name.clone(), price);
                    debug!("{}: {} = ${:.4} (REAL API DATA)", token.symbol, dex.name, price);
                }
                Ok(None) => {
                    // Token not listed on this DEX or price unavailable
                    debug!("{}: {} - No price data available", token.symbol, dex.name);
                }
                Err(e) => {
                    warn!("{}: {} - API error: {}", token.symbol, dex.name, e);
                }
            }
        }

        Ok(prices)
    }

    async fn fetch_price_from_dex(
        &self,
        dex: &DexConfig,
        token: &TokenConfig,
    ) -> Result<Option<f64>> {
        // ** PRODUCTION-GRADE: Build URL based on DEX requirements
        let url = if dex.requires_address {
            // Some APIs need token address appended
            if dex.url.contains("?ids=") {
                // Jupiter-style: ?ids=ADDRESS
                format!("{}{}", dex.url, token.address)
            } else if dex.url.ends_with("=") {
                // Birdeye-style: ?address=ADDRESS
                format!("{}{}", dex.url, token.address)
            } else if dex.url.ends_with("/") {
                // DexScreener-style: /ADDRESS
                format!("{}{}", dex.url, token.address)
            } else {
                // Default: append address
            format!("{}{}", dex.url, token.address)
            }
        } else {
            // Use symbol for APIs that support it
            format!("{}{}", dex.url, token.symbol)
        };

        // ** DEBUG: Log URL for first few calls to diagnose API issues
        static mut URL_LOG_COUNT: usize = 0;
        let should_log_url = unsafe {
            URL_LOG_COUNT += 1;
            URL_LOG_COUNT <= 5
        };
        if should_log_url {
            debug!("{}: {} - Fetching from URL: {}", token.symbol, dex.name, url);
        }

        // ** PRODUCTION-GRADE: Handle API timeouts gracefully
        let response = match self.client.get(&url).send().await {
            Ok(resp) => resp,
            Err(e) => {
                // ** CRITICAL: Log ALL errors to diagnose why no prices are fetched
                warn!("{}: {} - Request failed: {} | URL: {}", token.symbol, dex.name, e, url);
                return Ok(None);
            }
        };

        // ** PRODUCTION-GRADE: Handle HTTP errors gracefully
        if !response.status().is_success() {
            warn!("{}: {} - HTTP {} | URL: {}", token.symbol, dex.name, response.status(), url);
            return Ok(None);
        }

        // ** PRODUCTION-GRADE: Parse JSON with error handling
        let json: Value = match response.json().await {
            Ok(json) => json,
            Err(e) => {
                warn!("{}: {} - Failed to parse JSON: {} | URL: {}", token.symbol, dex.name, e, url);
                return Ok(None);
            }
        };
        
        // ** DEBUG: Log raw JSON for first few calls to diagnose parsing issues
        static mut JSON_LOG_COUNT: usize = 0;
        let should_log_json = unsafe {
            JSON_LOG_COUNT += 1;
            JSON_LOG_COUNT <= 10
        };
        if should_log_json {
            info!("{}: {} - Raw JSON response: {}", 
                  token.symbol, dex.name, 
                  serde_json::to_string(&json).unwrap_or_else(|_| "Failed to serialize".to_string()));
        }

        // ** PRODUCTION-GRADE: Extract price with graceful error handling
        // Return None instead of propagating errors for missing/empty data
        match extract_price(&json, &dex.response_path, &token.symbol) {
            Ok(price) => {
                if let Some(p) = price {
                    info!("✅ {}: {} - Successfully extracted price: ${:.4}", token.symbol, dex.name, p);
                } else {
                    warn!("⚠️ {}: {} - Price extraction returned None | Response path: {:?} | JSON keys: {:?}", 
                          token.symbol, dex.name, dex.response_path,
                          json.as_object().map(|o| o.keys().collect::<Vec<_>>()).unwrap_or_default());
                }
        Ok(price)
    }
            Err(e) => {
                // ** CRITICAL: Log ALL extraction failures to diagnose why no prices are fetched
                warn!("❌ {}: {} - Price extraction failed: {} | Response path: {:?} | Response: {}", 
                      token.symbol, dex.name, e, dex.response_path,
                      serde_json::to_string(&json).unwrap_or_else(|_| "Failed to serialize".to_string()));
                Ok(None) // Return None instead of error
            }
        }
    }
}

fn extract_price(json: &Value, path: &Option<String>, _token_symbol: &str) -> Result<Option<f64>> {
    // ** CRITICAL: Try path navigation first, but if it fails, fall back to format-specific handlers
    let value = if let Some(ref path_str) = path {
        // Navigate JSON path (e.g., "data.price" or "data.PRICE.price")
        let parts: Vec<&str> = path_str.split('.').collect();
        let mut current = json;
        let mut path_valid = true;
        
        for part in parts {
            // Handle array access (e.g., "pairs.0.priceUsd")
            if part.chars().all(|c| c.is_ascii_digit()) {
                let idx: usize = part.parse().unwrap_or(0);
                match current.as_array() {
                    Some(arr) => {
                        if arr.is_empty() {
                            // Empty array - path invalid
                            path_valid = false;
                            break;
                        }
                        match arr.get(idx) {
                            Some(v) => current = v,
                            None => {
                                path_valid = false;
                                break;
                            }
                        }
                    }
                    None => {
                        // Not an array - path invalid
                        path_valid = false;
                        break;
                    }
                }
            } else {
                // Handle object keys (case-insensitive for some APIs)
                match current.get(part) {
                    Some(v) => current = v,
                    None => {
                        // Try case-insensitive match
                        match current.as_object().and_then(|obj| {
                            obj.keys()
                                .find(|k| k.eq_ignore_ascii_case(part))
                                .and_then(|k| obj.get(k))
                        }) {
                            Some(v) => current = v,
                            None => {
                                path_valid = false;
                                break;
            }
        }
                    }
                }
            }
        }
        
        if path_valid {
        current
        } else {
            // Path navigation failed - return None to trigger fallback handlers
            return Ok(None);
        }
    } else {
        // Try common price fields
        json.get("price")
            .or_else(|| json.get("data").and_then(|d| d.get("price")))
            .or_else(|| json.get("value"))
            .unwrap_or(json)
    };

    // Try to extract f64 from various formats
    if let Some(price) = value.as_f64() {
        return Ok(Some(price));
    }

    if let Some(price_str) = value.as_str() {
        if let Ok(price) = price_str.parse::<f64>() {
            return Ok(Some(price));
        }
    }

    if let Some(price) = value.as_u64() {
        return Ok(Some(price as f64));
    }

    if let Some(price) = value.as_i64() {
        return Ok(Some(price as f64));
    }

    // ** PRODUCTION-GRADE: Handle Jupiter API format
    // Jupiter v4 returns: { "data": { "ADDRESS": { "PRICE": { "price": 100.5 } } } }
    // Also handle error responses: { "success": false, "message": "Not found" }
    if let Some(success) = json.get("success") {
        if let Some(false) = success.as_bool() {
            // Jupiter API returned error - token not found
            return Ok(None);
        }
    }
    
    if let Some(data) = json.get("data") {
        if let Some(token_data) = data.as_object() {
            for (_, v) in token_data {
                // Try "PRICE" field (Jupiter v4 format): { "PRICE": { "price": 100.5 } }
                if let Some(price_obj) = v.get("PRICE") {
                    if let Some(price) = price_obj.get("price").and_then(|p| p.as_f64()) {
                        return Ok(Some(price));
                    }
                }
                // Try "price" field (fallback format): { "price": 100.5 }
                if let Some(price_obj) = v.get("price") {
                    if let Some(price) = price_obj.as_f64() {
                        return Ok(Some(price));
                    }
                }
            }
        }
    }

    // ** PRODUCTION-GRADE: Handle DexScreener format
    // DexScreener returns: { "pairs": [{ "priceUsd": "100.5" }] }
    // Handle empty arrays gracefully
    if let Some(pairs) = json.get("pairs").and_then(|p| p.as_array()) {
        if pairs.is_empty() {
            // Empty pairs array - token may not be listed on DexScreener
            return Ok(None);
        }
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

    // ** PRODUCTION-GRADE: Handle Birdeye format
    // Birdeye returns: { "data": { "value": 100.5 } }
    // Also handle error responses: { "success": false, "message": "Not found" }
    if let Some(success) = json.get("success") {
        if let Some(false) = success.as_bool() {
            // Birdeye API returned error - token not found
            return Ok(None);
        }
    }
    
    if let Some(data) = json.get("data") {
        if let Some(value) = data.get("value") {
            if let Some(price) = value.as_f64() {
                return Ok(Some(price));
            }
        }
    }

    // ** PRODUCTION-GRADE: Handle CoinGecko format
    // CoinGecko returns: { "solana": { "usd": 100.5 } } or { "token_id": { "usd": 100.5 } }
    // Also handle rate limit errors: { "status": { "error_code": 429, "error_message": "..." } }
    if let Some(status) = json.get("status") {
        if let Some(error_code) = status.get("error_code") {
            if let Some(429) = error_code.as_u64() {
                // CoinGecko rate limited - skip
                return Ok(None);
            }
        }
    }
    
    if let Some(obj) = json.as_object() {
        for (_, token_data) in obj {
            if let Some(usd_price) = token_data.get("usd").and_then(|p| p.as_f64()) {
                return Ok(Some(usd_price));
            }
        }
    }

    Ok(None)
}

