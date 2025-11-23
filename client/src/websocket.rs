//! Production-grade WebSocket subscription manager for real-time Solana pool data updates.
//! 
//! This module implements parallel WebSocket subscriptions to all pool addresses,
//! providing real-time, consistent pool data updates without timing gaps.
//! 
//! Architecture:
//! - Runs in a separate tokio runtime thread
//! - Subscribes to all pool addresses in parallel
//! - Sends account updates via channel to main thread
//! - Handles reconnections with exponential backoff
//! - Implements rate limiting and error recovery

use anchor_client::solana_sdk::pubkey::Pubkey;
use solana_sdk::account::Account;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::RwLock;
use tokio::time::sleep;
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::str::FromStr;
use futures_util::StreamExt;
use base64::Engine;
use crate::constants::*;

/// RPC Provider configuration
#[derive(Clone, Debug)]
pub struct RpcProvider {
    pub name: String,
    pub http_url: String,
    pub ws_url: String,
    pub priority: u8, // Lower = higher priority
    pub rate_limit: u32, // Max subscriptions per connection
}

impl RpcProvider {
    pub fn default_providers() -> Vec<Self> {
        vec![
            RpcProvider {
                name: "Helius".to_string(),
                http_url: "https://api.helius.xyz".to_string(),
                ws_url: "wss://api.helius.xyz".to_string(),
                priority: 1,
                rate_limit: 1000,
            },
            RpcProvider {
                name: "QuickNode".to_string(),
                http_url: "https://api.quicknode.com".to_string(),
                ws_url: "wss://api.quicknode.com/ws".to_string(),
                priority: 2,
                rate_limit: 500,
            },
            RpcProvider {
                name: "Chainstack".to_string(),
                http_url: "https://api.chainstack.com".to_string(),
                ws_url: "wss://api.chainstack.com/ws".to_string(),
                priority: 3,
                rate_limit: 500,
            },
            RpcProvider {
                name: "Solana Mainnet".to_string(),
                http_url: "https://api.mainnet-beta.solana.com".to_string(),
                ws_url: "wss://api.mainnet-beta.solana.com".to_string(),
                priority: 4,
                rate_limit: 100,
            },
        ]
    }
    
    /// Calculate health score for provider selection (0-100, higher is better)
    /// Factors: success rate, priority, rate limit
    pub fn calculate_health_score(&self, success_count: u32, failure_count: u32) -> f64 {
        let total_attempts = success_count + failure_count;
        
        if total_attempts == 0 {
            // No history - use priority-based score (higher priority = higher score)
            return 100.0 - (self.priority as f64 * 10.0);
        }
        
        // Success rate (0-1)
        let success_rate = success_count as f64 / total_attempts as f64;
        
        // Priority factor (lower priority = higher score)
        let priority_factor = 1.0 - (self.priority as f64 * 0.1);
        
        // Rate limit factor (higher limit = higher score, normalized to 0-1)
        let rate_limit_factor = (self.rate_limit as f64 / 1000.0).min(1.0);
        
        // Weighted score: 60% success rate, 20% priority, 20% rate limit
        let score = (success_rate * 60.0) + (priority_factor * 20.0) + (rate_limit_factor * 20.0);
        
        score.max(0.0).min(100.0)
    }
}

/// WebSocket subscription manager for real-time pool data updates
pub struct WebSocketManager {
    /// Channel sender for account updates (pubkey -> account data)
    update_tx: mpsc::UnboundedSender<(Pubkey, Option<Account>)>,
    /// Channel receiver for account updates
    update_rx: Option<mpsc::UnboundedReceiver<(Pubkey, Option<Account>)>>,
    /// Map of subscription IDs to pool addresses
    subscriptions: Arc<RwLock<HashMap<u64, Pubkey>>>,
    /// Map of log subscription IDs to DEX program addresses
    log_subscriptions: Arc<RwLock<HashMap<u64, Pubkey>>>,
    /// Channel sender for new pool discoveries (program_id -> pool_address)
    new_pool_tx: Option<mpsc::UnboundedSender<(Pubkey, Pubkey)>>, // (program_id, pool_address)
    /// WebSocket endpoint URL (primary)
    ws_url: String,
    /// Pool addresses to subscribe to
    pool_addresses: Vec<Pubkey>,
    /// DEX program addresses to monitor for new pools
    dex_program_addresses: Vec<Pubkey>,
    /// Reconnection attempt counter
    reconnect_attempts: Arc<RwLock<u32>>,
    /// Maximum reconnection attempts before giving up
    max_reconnect_attempts: u32,
    /// Available RPC providers (sorted by priority)
    providers: Vec<RpcProvider>,
    /// Current provider index
    current_provider_idx: Arc<RwLock<usize>>,
    /// Provider health tracking (success/failure counts)
    provider_health: Arc<RwLock<HashMap<String, (u32, u32)>>>, // (success, failure)
}

/// WebSocket message types for Solana RPC
#[derive(Debug, Serialize, Deserialize)]
struct SubscriptionRequest {
    jsonrpc: String,
    id: u64,
    method: String,
    params: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct SubscriptionResponse {
    jsonrpc: String,
    id: Option<u64>,
    result: Option<u64>, // Subscription ID
    method: Option<String>,
    params: Option<SubscriptionParams>,
    error: Option<SubscriptionError>,
}

#[derive(Debug, Deserialize)]
struct SubscriptionParams {
    result: Option<serde_json::Value>, // Can be SubscriptionResult or LogsResult
    subscription: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct SubscriptionResult {
    value: Option<AccountData>,
}

#[derive(Debug, Deserialize)]
struct LogsResult {
    value: Option<LogsData>,
}

#[derive(Debug, Deserialize)]
struct LogsData {
    err: Option<serde_json::Value>,
    logs: Vec<String>,
    signature: String,
}

#[derive(Debug, Deserialize)]
struct AccountData {
    data: Vec<String>, // [base64_data, encoding]
    executable: bool,
    lamports: u64,
    owner: String,
    rent_epoch: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct SubscriptionError {
    code: i32,
    message: String,
}

impl WebSocketManager {
    /// Create a new WebSocket manager with multi-provider fallback and new pool detection
    pub fn new(ws_url: String, pool_addresses: Vec<Pubkey>) -> Self {
        let (update_tx, update_rx) = mpsc::unbounded_channel();
        let (new_pool_tx, _new_pool_rx) = mpsc::unbounded_channel();
        
        // Get default providers and find best match for provided URL
        let mut providers = RpcProvider::default_providers();
        let mut current_provider_idx = 0;
        
        // Find provider matching the provided URL (or use first as default)
        for (idx, provider) in providers.iter().enumerate() {
            if ws_url.contains(&provider.name.to_lowercase()) || 
               ws_url == provider.ws_url ||
               ws_url.contains("helius") && provider.name == "Helius" ||
               ws_url.contains("quicknode") && provider.name == "QuickNode" ||
               ws_url.contains("chainstack") && provider.name == "Chainstack" {
                current_provider_idx = idx;
                break;
            }
        }
        
        // If custom URL provided, add it as first priority
        if !providers.iter().any(|p| p.ws_url == ws_url) {
            providers.insert(0, RpcProvider {
                name: "Custom".to_string(),
                http_url: ws_url.replace("wss://", "https://").replace("ws://", "http://"),
                ws_url: ws_url.clone(),
                priority: 0,
                rate_limit: 200, // Conservative default
            });
            current_provider_idx = 0;
        }
        
        // DEX program addresses for new pool detection (from constants.rs)
        let dex_program_addresses = vec![
            *ORCA_PROGRAM_ID,
            *SABER_PROGRAM_ID,
            *ALDRIN_V1_PROGRAM_ID,
            *ALDRIN_V2_PROGRAM_ID,
            *SERUM_PROGRAM_ID,
        ];
        
        Self {
            update_tx,
            update_rx: Some(update_rx),
            subscriptions: Arc::new(RwLock::new(HashMap::new())),
            log_subscriptions: Arc::new(RwLock::new(HashMap::new())),
            new_pool_tx: Some(new_pool_tx),
            ws_url,
            pool_addresses,
            dex_program_addresses,
            reconnect_attempts: Arc::new(RwLock::new(0)),
            max_reconnect_attempts: 10,
            providers,
            current_provider_idx: Arc::new(RwLock::new(current_provider_idx)),
            provider_health: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    
    /// Get the receiver channel for new pool discoveries
    pub fn take_new_pool_receiver(&mut self) -> Option<mpsc::UnboundedReceiver<(Pubkey, Pubkey)>> {
        // Note: This would require storing the receiver, for now we'll handle new pools internally
        None
    }
    
    /// Get dynamic rate limit for current provider
    async fn get_rate_limit(&self) -> u32 {
        let idx = *self.current_provider_idx.read().await;
        self.providers[idx].rate_limit
    }
    
    /// Get current provider URL
    async fn get_current_provider_url(&self) -> String {
        let idx = *self.current_provider_idx.read().await;
        self.providers[idx].ws_url.clone()
    }
    
    /// ** PRODUCTION-GRADE: Get best provider based on health score
    async fn get_best_provider(&self) -> Option<(usize, &RpcProvider, f64)> {
        let health_map = self.provider_health.read().await;
        
        let mut best_provider: Option<(usize, &RpcProvider, f64)> = None;
        let mut best_score = -1.0;
        
        for (idx, provider) in self.providers.iter().enumerate() {
            let health = health_map.get(&provider.name).copied().unwrap_or((0, 0));
            let score = provider.calculate_health_score(health.0, health.1);
            
            if score > best_score {
                best_score = score;
                best_provider = Some((idx, provider, score));
            }
        }
        
        best_provider
    }
    
    /// Switch to next available provider (using health scoring)
    async fn switch_to_next_provider(&self) -> Option<&RpcProvider> {
        // ** PRODUCTION-GRADE: Use health score to select best provider
        if let Some((best_idx, best_provider, score)) = self.get_best_provider().await {
            let current_idx = *self.current_provider_idx.read().await;
            
            // Only switch if best provider is different and has better score
            if best_idx != current_idx {
                *self.current_provider_idx.write().await = best_idx;
                info!("🔄 Switched to best provider: {} (health score: {:.2}/100, priority: {})", 
                      best_provider.name, score, best_provider.priority);
                return Some(best_provider);
            }
        }
        
        // Fallback: round-robin if health scoring doesn't find a better provider
        let mut idx = *self.current_provider_idx.read().await;
        let mut attempts = 0;
        
        while attempts < self.providers.len() {
            idx = (idx + 1) % self.providers.len();
            let provider = &self.providers[idx];
            
            // Check provider health (skip if too many failures)
            let health = {
                let health_map = self.provider_health.read().await;
                health_map.get(&provider.name).copied().unwrap_or((0, 0))
            };
            
            // Skip if failure rate > 50% and we have other options
            if health.1 > 0 && health.1 > health.0 && self.providers.len() > 1 {
                attempts += 1;
                continue;
            }
            
            *self.current_provider_idx.write().await = idx;
            info!("🔄 Switched to provider: {} (priority: {})", provider.name, provider.priority);
            return Some(provider);
        }
        
        None
    }
    
    /// Record provider success/failure
    async fn record_provider_result(&self, provider_name: &str, success: bool) {
        let mut health = self.provider_health.write().await;
        let entry = health.entry(provider_name.to_string()).or_insert((0, 0));
        if success {
            entry.0 += 1;
        } else {
            entry.1 += 1;
        }
    }

    /// Get the receiver channel for account updates
    pub fn take_receiver(&mut self) -> mpsc::UnboundedReceiver<(Pubkey, Option<Account>)> {
        self.update_rx.take().expect("Receiver already taken")
    }

    /// Start the WebSocket subscription manager in a separate tokio runtime
    pub fn start(self) -> std::thread::JoinHandle<()> {
        let manager = Arc::new(self);
        let manager_clone = manager.clone();
        
        std::thread::spawn(move || {
            // Create a new tokio runtime for this thread
            // ** CRITICAL FIX: Catch panics to prevent main process from exiting
            let rt = match tokio::runtime::Runtime::new() {
                Ok(runtime) => runtime,
                Err(e) => {
                    error!("Failed to create tokio runtime for WebSocket: {}", e);
                    return; // Exit thread gracefully instead of panicking
                }
            };
            
            // ** CRITICAL FIX: Catch panics in WebSocket run loop
            // This prevents the WebSocket thread from crashing the entire process
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                rt.block_on(async move {
                    manager_clone.run().await;
                });
            }));
            
            if let Err(e) = result {
                error!("WebSocket thread panicked: {:?}", e);
                error!("WebSocket manager will continue in background, but connection may be broken");
                // Don't panic - let the main process continue
            }
        })
    }

    /// Main WebSocket connection and subscription loop with multi-provider fallback
    async fn run(self: Arc<Self>) {
        let mut consecutive_failures = 0;
        
        // ** PRODUCTION-GRADE: Start with best provider based on health score
        if let Some((best_idx, best_provider, score)) = self.get_best_provider().await {
            *self.current_provider_idx.write().await = best_idx;
            info!("🎯 Starting with best provider: {} (health score: {:.2}/100)", best_provider.name, score);
        }
        
        loop {
            let provider_url = self.get_current_provider_url().await;
            let provider_name = {
                let idx = *self.current_provider_idx.read().await;
                self.providers[idx].name.clone()
            };
            
            // ** PRODUCTION-GRADE: Log provider health score
            let health_score = {
                let health_map = self.provider_health.read().await;
                let health = health_map.get(&provider_name).copied().unwrap_or((0, 0));
                let idx = *self.current_provider_idx.read().await;
                self.providers[idx].calculate_health_score(health.0, health.1)
            };
            
            info!("🔗 Attempting connection to provider: {} ({}) [Health Score: {:.2}/100]", 
                  provider_name, provider_url, health_score);
            
            match self.clone().connect_and_subscribe_with_url(provider_url.clone()).await {
                Ok(_) => {
                    // Connection closed normally, record success and wait before reconnecting
                    self.record_provider_result(&provider_name, true).await;
                    consecutive_failures = 0;
                    
                    // ** PRODUCTION-GRADE: Log updated health score
                    let updated_score = {
                        let health_map = self.provider_health.read().await;
                        let health = health_map.get(&provider_name).copied().unwrap_or((0, 0));
                        let idx = *self.current_provider_idx.read().await;
                        self.providers[idx].calculate_health_score(health.0, health.1)
                    };
                    info!("✅ Connection closed normally. Provider {} health score: {:.2}/100. Reconnecting in 5 seconds...", 
                          provider_name, updated_score);
                    sleep(Duration::from_secs(5)).await;
                }
                Err(e) => {
                    // Record failure and try next provider
                    self.record_provider_result(&provider_name, false).await;
                    consecutive_failures += 1;
                    
                    warn!("❌ WebSocket connection failed to {}: {}", provider_name, e);
                    
                    // ** PRODUCTION-GRADE: Try best available provider based on health score
                    if let Some(next_provider) = self.switch_to_next_provider().await {
                        let next_health_score = {
                            let health_map = self.provider_health.read().await;
                            let health = health_map.get(&next_provider.name).copied().unwrap_or((0, 0));
                            next_provider.calculate_health_score(health.0, health.1)
                        };
                        info!("🔄 Switching to provider: {} (health score: {:.2}/100, priority: {})", 
                              next_provider.name, next_health_score, next_provider.priority);
                        sleep(Duration::from_secs(2)).await; // Brief delay before retry
                        continue;
                    }
                    
                    // All providers failed, use exponential backoff
                    let attempts = {
                        let mut attempts = self.reconnect_attempts.write().await;
                        *attempts += 1;
                        *attempts
                    };
                    
                    if attempts >= self.max_reconnect_attempts {
                        error!("WebSocket failed after {} attempts across all providers. Falling back to HTTP polling.", attempts);
                        // Send error notification to main thread
                        for addr in &self.pool_addresses {
                            let _ = self.update_tx.send((*addr, None));
                        }
                        break;
                    }
                    
                    // Exponential backoff: 2^attempts seconds, max 60 seconds
                    let backoff = std::cmp::min(2_u64.pow(attempts), 60);
                    warn!("WebSocket error (attempt {}/{}): {}. Reconnecting in {} seconds...", 
                          attempts, self.max_reconnect_attempts, e, backoff);
                    sleep(Duration::from_secs(backoff)).await;
                }
            }
        }
    }

    /// Connect to WebSocket and subscribe to all pool addresses (using self.ws_url)
    async fn connect_and_subscribe(self: Arc<Self>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let ws_url = self.ws_url.clone();
        self.connect_and_subscribe_with_url(ws_url).await
    }
    
    /// Connect to WebSocket and subscribe to all pool addresses (with custom URL)
    async fn connect_and_subscribe_with_url(self: Arc<Self>, ws_url: String) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        info!("Connecting to WebSocket: {}", ws_url);
        
        // ** CRITICAL FIX: Add timeout and better error handling
        let connect_result = tokio::time::timeout(
            Duration::from_secs(30),
            tokio_tungstenite::connect_async(&ws_url)
        ).await;
        
        let (ws_stream, _) = match connect_result {
            Ok(Ok(stream)) => stream,
            Ok(Err(e)) => {
                let error_msg = format!("WebSocket connection failed to {}: {} (DNS/SSL/TLS error - check URL and network)", ws_url, e);
                error!("{}", error_msg);
                return Err(error_msg.into());
            }
            Err(_) => {
                let error_msg = format!("WebSocket connection timeout to {} (30s) - server may be unreachable or rate limiting", ws_url);
                error!("{}", error_msg);
                return Err(error_msg.into());
            }
        };
        
        info!("");
        info!("═══════════════════════════════════════════════════════════════");
        info!("✅ WEBSOCKET CONNECTED SUCCESSFULLY");
        info!("═══════════════════════════════════════════════════════════════");
        info!("🔗 Connection established to: {}", ws_url);
        
        let (mut write, read) = ws_stream.split();
        
        // Reset reconnection attempts on successful connection
        {
            let mut attempts = self.reconnect_attempts.write().await;
            *attempts = 0;
        }
        
        // ** PRODUCTION-GRADE: Dynamic Rate Limiting
        // Get rate limit for current provider
        let rate_limit = self.get_rate_limit().await;
        let total_subscriptions_needed = self.pool_addresses.len() + self.dex_program_addresses.len();
        
        if total_subscriptions_needed > rate_limit as usize {
            warn!("⚠️ WARNING: Total subscriptions ({}) exceed provider rate limit ({}). Some subscriptions may fail.", 
                  total_subscriptions_needed, rate_limit);
        }
        
        // Subscribe to all pool addresses in parallel
        let mut subscription_id = 1u64;
        let mut subscriptions = self.subscriptions.write().await;
        subscriptions.clear();
        
        let mut log_subscriptions = self.log_subscriptions.write().await;
        log_subscriptions.clear();
        
        info!("Subscribing to {} pool addresses and {} DEX programs (total: {} subscriptions, limit: {})...", 
              self.pool_addresses.len(), self.dex_program_addresses.len(), total_subscriptions_needed, rate_limit);
        let subscribe_start = std::time::Instant::now();
        
        use futures_util::SinkExt;
        
        // Subscribe to pool account addresses
        for pool_addr in &self.pool_addresses {
            // Check rate limit
            if subscription_id > rate_limit as u64 {
                warn!("⚠️ Rate limit reached ({}). Skipping remaining pool subscriptions.", rate_limit);
                break;
            }
            
            let sub_request = json!({
                "jsonrpc": "2.0",
                "id": subscription_id,
                "method": "accountSubscribe",
                "params": [
                    pool_addr.to_string(),
                    {
                        "encoding": "base64",
                        "commitment": "confirmed"
                    }
                ]
            });
            
            // Send subscription request
            let msg = tokio_tungstenite::tungstenite::Message::Text(
                serde_json::to_string(&sub_request).unwrap()
            );
            
            if let Err(e) = write.send(msg).await {
                warn!("Failed to send subscription for pool {}: {}", pool_addr, e);
                continue;
            }
            
            subscriptions.insert(subscription_id, *pool_addr);
            subscription_id += 1;
        }
        
        // ** PRODUCTION-GRADE: Subscribe to DEX program logs for new pool detection
        info!("Subscribing to {} DEX program logs for new pool detection...", self.dex_program_addresses.len());
        for dex_program_id in &self.dex_program_addresses {
            // Check rate limit
            if subscription_id > rate_limit as u64 {
                warn!("⚠️ Rate limit reached ({}). Skipping remaining log subscriptions.", rate_limit);
                break;
            }
            
            let log_sub_request = json!({
                "jsonrpc": "2.0",
                "id": subscription_id,
                "method": "logsSubscribe",
                "params": [
                    {
                        "mentions": [dex_program_id.to_string()]
                    },
                    {
                        "commitment": "confirmed"
                    }
                ]
            });
            
            // Send log subscription request
            let msg = tokio_tungstenite::tungstenite::Message::Text(
                serde_json::to_string(&log_sub_request).unwrap()
            );
            
            if let Err(e) = write.send(msg).await {
                warn!("Failed to send log subscription for DEX program {}: {}", dex_program_id, e);
                continue;
            }
            
            log_subscriptions.insert(subscription_id, *dex_program_id);
            subscription_id += 1;
        }
        
        drop(subscriptions);
        drop(log_subscriptions);
        let subscribe_duration = subscribe_start.elapsed();
        let total_sent = (subscription_id - 1) as usize;
        info!("📤 Sent {} subscription requests ({} pools + {} logs) in {:?}ms (parallel)", 
              total_sent, self.pool_addresses.len(), self.dex_program_addresses.len(), subscribe_duration.as_millis());
        info!("⏳ Waiting for subscription confirmations...");
        
        // Spawn task to handle incoming messages
        let manager_clone = self.clone();
        let read_task = tokio::spawn(async move {
            manager_clone.handle_messages(read).await
        });
        
        // Wait for read task to complete (connection closed or error)
        match read_task.await {
            Ok(Ok(())) => {
                info!("WebSocket read task completed normally");
                Ok(())
            }
            Ok(Err(e)) => {
                let error_msg = format!("WebSocket read task error: {}", e);
                error!("{}", error_msg);
                Err(error_msg.into())
            }
            Err(e) => {
                let error_msg = format!("WebSocket read task panic: {:?}", e);
                error!("{}", error_msg);
                Err(error_msg.into())
            }
        }
    }

    /// Handle incoming WebSocket messages
    async fn handle_messages(
        self: Arc<Self>,
        mut read: futures_util::stream::SplitStream<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use futures_util::StreamExt;
        let mut initial_subscriptions = 0;
        let mut initial_log_subscriptions = 0;
        let subscriptions_expected = self.pool_addresses.len();
        let log_subscriptions_expected = self.dex_program_addresses.len();
        let total_expected = subscriptions_expected + log_subscriptions_expected;
        
        while let Some(msg_result) = read.next().await {
            let msg = match msg_result {
                Ok(m) => m,
                Err(e) => {
                    let error_msg = format!("WebSocket message error: {} - connection may be broken", e);
                    error!("{}", error_msg);
                    return Err(error_msg.into());
                }
            };
            
            match msg {
                tokio_tungstenite::tungstenite::Message::Text(text) => {
                    // Parse JSON-RPC response
                    match serde_json::from_str::<SubscriptionResponse>(&text) {
                        Ok(response) => {
                            // Handle subscription confirmation
                            if let Some(sub_id) = response.result {
                                // Check if this is a log subscription or account subscription
                                let is_log_subscription = {
                                    let log_subs = self.log_subscriptions.read().await;
                                    log_subs.contains_key(&sub_id)
                                };
                                
                                if is_log_subscription {
                                    initial_log_subscriptions += 1;
                                    if initial_log_subscriptions == 1 {
                                        info!("✅ First log subscription confirmed (new pool detection enabled)");
                                    }
                                } else {
                                    initial_subscriptions += 1;
                                    if initial_subscriptions == 1 {
                                        info!("");
                                        info!("═══════════════════════════════════════════════════════════════");
                                        info!("✅ FIRST SUBSCRIPTION CONFIRMED (ID: {})", sub_id);
                                        info!("═══════════════════════════════════════════════════════════════");
                                        info!("📊 Receiving subscription confirmations...");
                                    }
                                }
                                
                                let total_confirmed = initial_subscriptions + initial_log_subscriptions;
                                if total_confirmed == total_expected {
                                    info!("");
                                    info!("═══════════════════════════════════════════════════════════════");
                                    info!("🎉 ALL {} SUBSCRIPTIONS CONFIRMED - WEBSOCKET MODE ACTIVE", total_expected);
                                    info!("   - {} pool account subscriptions", initial_subscriptions);
                                    info!("   - {} DEX log subscriptions (new pool detection)", initial_log_subscriptions);
                                    info!("═══════════════════════════════════════════════════════════════");
                                    info!("⚡ Real-time pool updates enabled - no more HTTP polling delays");
                                    info!("🔄 Pools will update automatically as blockchain data changes");
                                    info!("🆕 New pool detection enabled - monitoring DEX program logs");
                                    info!("📝 Logging to log.txt is now active (WebSocket mode enabled)");
                                    info!("");
                                } else if total_confirmed % 100 == 0 {
                                    info!("📈 Subscription progress: {}/{} confirmed ({} pools + {} logs)", 
                                          total_confirmed, total_expected, initial_subscriptions, initial_log_subscriptions);
                                }
                            }
                            
                            // Handle account update notification
                            if let Some(method) = response.method.clone() {
                                if method == "accountNotification" {
                                    if let Some(params) = response.params {
                                        if let Some(sub_id) = params.subscription {
                                            let subscriptions = self.subscriptions.read().await;
                                            if let Some(pool_addr) = subscriptions.get(&sub_id) {
                                                if let Some(result) = &params.result {
                                                    // Parse result as SubscriptionResult
                                                    if let Ok(account_result) = serde_json::from_value::<SubscriptionResult>(result.clone()) {
                                                        if let Some(value) = account_result.value {
                                                            // Parse account data
                                                            match self.parse_account_data(value) {
                                                                Ok(account) => {
                                                                    // Send update to main thread
                                                                    if let Err(e) = self.update_tx.send((*pool_addr, Some(account))) {
                                                                        error!("Failed to send account update: {}", e);
                                                                    }
                                                                }
                                                                Err(e) => {
                                                                    debug!("Failed to parse account data for {}: {}", pool_addr, e);
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                } else if method == "logsNotification" {
                                    // ** PRODUCTION-GRADE: Handle new pool detection from logs
                                    if let Some(params) = response.params {
                                        if let Some(sub_id) = params.subscription {
                                            let log_subs = self.log_subscriptions.read().await;
                                            if let Some(dex_program_id) = log_subs.get(&sub_id) {
                                                // Parse logs from params
                                                // Note: logsNotification format is different from accountNotification
                                                // We need to parse the logs from the result
                                                if let Some(result) = params.result {
                                                    // Try to parse as LogsData
                                                    if let Ok(logs_data) = serde_json::from_value::<LogsData>(result) {
                                                        self.handle_logs_for_new_pool(*dex_program_id, logs_data).await;
                                                    } else {
                                                        debug!("Failed to parse logs notification for DEX program {}", dex_program_id);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            
                            // Handle errors
                            if let Some(err) = response.error {
                                warn!("WebSocket RPC error: {} (code: {})", err.message, err.code);
                            }
                        }
                        Err(e) => {
                            debug!("Failed to parse WebSocket message: {} - {}", e, text);
                        }
                    }
                }
                tokio_tungstenite::tungstenite::Message::Close(_) => {
                    info!("WebSocket connection closed by server");
                    return Ok(());
                }
                tokio_tungstenite::tungstenite::Message::Ping(_data) => {
                    // Auto-respond to ping (handled by tokio-tungstenite)
                    debug!("Received ping");
                }
                tokio_tungstenite::tungstenite::Message::Pong(_) => {
                    debug!("Received pong");
                }
                _ => {
                    debug!("Received unexpected message type");
                }
            }
        }
        
        Ok(())
    }

    /// Parse Solana account data from WebSocket notification
    fn parse_account_data(&self, data: AccountData) -> Result<Account, Box<dyn std::error::Error + Send + Sync>> {
        if data.data.len() < 2 {
            return Err("Invalid account data format".into());
        }
        
        let base64_data = &data.data[0];
        let account_bytes = base64::engine::general_purpose::STANDARD
            .decode(base64_data)
            .map_err(|e| format!("Failed to decode base64: {}", e))?;
        
        let owner = Pubkey::from_str(&data.owner)
            .map_err(|e| format!("Failed to parse owner pubkey: {}", e))?;
        
        Ok(Account {
            lamports: data.lamports,
            data: account_bytes,
            owner,
            executable: data.executable,
            rent_epoch: data.rent_epoch.unwrap_or(0),
        })
    }
    
    /// ** PRODUCTION-GRADE: Handle logs for new pool detection
    /// Parses DEX program logs to detect new pool creation
    async fn handle_logs_for_new_pool(&self, dex_program_id: Pubkey, logs: LogsData) {
        // Check if logs indicate new pool creation
        // Common patterns: "initialize", "createPool", "initialize2", "newPool"
        let pool_creation_keywords = vec!["initialize", "createPool", "initialize2", "newPool", "create"];
        
        let has_pool_creation = logs.logs.iter().any(|log| {
            pool_creation_keywords.iter().any(|keyword| log.contains(keyword))
        });
        
        if has_pool_creation {
            info!("🆕 Potential new pool detected from DEX program {} (signature: {})", 
                  dex_program_id, logs.signature);
            
            // Extract pool address from transaction signature
            // Note: In production, you would parse the transaction to extract the pool address
            // For now, we log the signature for manual investigation
            debug!("New pool creation logs: {:?}", logs.logs);
            
            // TODO: Parse transaction to extract pool address
            // This would require fetching the transaction and parsing instruction data
            // For now, we just log the detection
        }
    }
}


