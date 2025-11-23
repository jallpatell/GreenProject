use anyhow::Result;
use colored::*;
use log::{debug, error, info, warn};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::time::Duration;

mod arbitrage;
mod arb_logger;
mod config;
mod dex_sources;
mod price_fetcher;
mod ws_manager;
mod saber_pools;

use arbitrage::{ArbitrageDetector, Opportunity};
use arb_logger::ArbitrageLogger;
use config::{load_config, load_dexes, load_tokens};
use price_fetcher::PriceFetcher;
use ws_manager::{PriceUpdate, WebSocketManager};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logger
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    info!("🚀 Starting Solana Arbitrage Scanner");
    info!("📊 High-performance token price tracking across DEXs");

    // Load configuration
    let config = load_config("config/config.toml")?;
    let tokens = load_tokens("tokens.json")?;
    let dexes = load_dexes("dexes.json")?;

    info!("✅ Loaded {} tokens", tokens.len());
    info!("✅ Loaded {} DEX endpoints", dexes.len());
    info!("📊 Spread threshold: {:.2}%", config.scanner.spread_threshold * 100.0);
    info!("📝 Log file: {}", config.scanner.log_file);

    // Initialize components
    let price_fetcher = PriceFetcher::new(dexes.clone());
    let detector = ArbitrageDetector::new(config.scanner.spread_threshold);
    let logger = ArbitrageLogger::new(&config.scanner.log_file)?;

    // ** PRODUCTION-GRADE: WebSocket-based real-time price streaming
    // ** REAL DATA: All prices from live DEX APIs (WebSocket or HTTP polling fallback)
    // ** Subscribes to each token from each DEX for real-time arbitrage detection
    
    info!("📡 Starting WebSocket connections for real-time price streaming");
    info!("🔄 Subscribing to {} tokens from {} DEXes", tokens.len(), dexes.len());
    
    // Initialize WebSocket manager with tokens and DEXes
    let (ws_manager, mut price_updates_rx) = WebSocketManager::new(tokens.clone(), dexes.clone());
    
    // Start WebSocket connections (with HTTP polling fallback for DEXes without WebSocket)
    ws_manager.start().await?;
    
    info!("✅ WebSocket connections started - listening for real-time price updates");
    
    // ** PRODUCTION-GRADE: Initialize Saber Pool Monitor
    // ** LOW-LATENCY: Fetches pool reserves and calculates prices in real-time
    // ** CACHED: Minimizes RPC calls with TTL-based caching
    let saber_pools = saber_pools::load_saber_pools("../pools/saber")?;
    if !saber_pools.is_empty() {
        info!("📊 Loaded {} Saber pools for monitoring", saber_pools.len());
        
        // Create Saber pool monitor
        let rpc_url = std::env::var("SOLANA_RPC_URL")
            .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
        let saber_monitor = saber_pools::SaberPoolMonitor::new(&rpc_url, saber_pools, 5)?; // 5s cache TTL
        
        // Set token mapping
        let mut token_mapping = HashMap::new();
        for token in &tokens {
            token_mapping.insert(token.address.clone(), token.symbol.clone());
        }
        saber_monitor.set_token_mapping(token_mapping).await;
        
        // Start Saber monitoring in background
        let tx = ws_manager.get_price_updates_tx();
        let price_matrix = ws_manager.price_matrix.clone();
        tokio::spawn(async move {
            saber_monitor.start_monitoring(tx, price_matrix, 2).await; // Poll every 2s
        });
        
        info!("✅ Saber pool monitoring started");
    } else {
        warn!("⚠️  No Saber pools found - skipping Saber integration");
    }

    // ** PRODUCTION-GRADE: Real-time arbitrage detection loop
    // ** LOW-LATENCY: Processes price updates as they arrive from WebSocket/HTTP polling
    // ** Optimized for minimal latency in spread calculation
    let mut cycle = 0;
    let mut last_analysis = std::time::Instant::now();
    let analysis_interval = Duration::from_millis(500); // ** LOW-LATENCY: Analyze spreads every 500ms

    // Main event loop - process real-time price updates
    loop {
        cycle += 1;
        
        // Process incoming price updates
        tokio::select! {
            // Receive price update from WebSocket/HTTP polling
            update = price_updates_rx.recv() => {
                if let Some(update) = update {
                    // Price update received - will be analyzed in next analysis cycle
                    debug!("📊 Price update: {} = ${:.4} on {}", 
                           update.token_symbol, update.price, update.dex_name);
                }
            }
            
            // ** LOW-LATENCY: Analyze spreads periodically (every 500ms)
            _ = tokio::time::sleep(analysis_interval) => {
                if last_analysis.elapsed() >= analysis_interval {
                    let analysis_start = std::time::Instant::now();
                    last_analysis = std::time::Instant::now();
                    
                    // ** LOW-LATENCY: Get current price matrix (single read)
                    let prices = ws_manager.get_price_matrix().await;

                    // ** LOW-LATENCY: Detect arbitrage opportunities (optimized calculation)
                    let opportunities = detector.detect_opportunities(&tokens, &prices);
                    
                    let analysis_duration = analysis_start.elapsed();
                    
                    // Log and print real opportunities (only when spread > threshold)
                    for opp in &opportunities {
                        info!("📝 Logging real arbitrage: {} | Spread: {:.2}% | Max: ${:.4} ({}) | Min: ${:.4} ({})", 
                               opp.token, opp.spread * 100.0, opp.max_price, opp.max_dex, opp.min_price, opp.min_dex);
                        
                        if let Err(e) = logger.log_opportunity(opp) {
                            error!("Failed to log opportunity: {}", e);
            }
                    }
                    
                    // Print results (top 5 only) when opportunities found
                    if !opportunities.is_empty() {
                        print_results(cycle, &opportunities, Duration::from_secs(0));
                    }
                    
                    // ** LOW-LATENCY: Log statistics every 10 cycles (with latency metrics)
                    if cycle % 10 == 0 {
                        let tokens_with_prices = prices.len();
                        let total_price_points: usize = prices.values().map(|dex_prices| dex_prices.len()).sum();
                        info!("📊 Cycle #{}: {} tokens with prices | {} total price points | {} opportunities | Analysis: {:?}", 
                              cycle, tokens_with_prices, total_price_points, opportunities.len(), analysis_duration);
                    }
                }
            }
        }
    }
}

fn print_results(cycle: u64, opportunities: &[Opportunity], duration: Duration) {
    // ** PRODUCTION-GRADE: Only print when opportunities exist (spread > threshold)
    if opportunities.is_empty() {
        // Don't print anything when no opportunities found
        return;
    }
    
    println!("\n{}", "=".repeat(80).bright_black());
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let time_str = format!("{:02}:{:02}:{:02}", 
        (timestamp / 3600) % 24,
        (timestamp / 60) % 60,
        timestamp % 60
    );
    
    println!(
        "{} Cycle #{} | {} | Duration: {:?}",
        "📊".bright_cyan(),
        cycle.to_string().bright_white().bold(),
        time_str.bright_black(),
        duration
    );
    println!("{}", "=".repeat(80).bright_black());

    // Only print if opportunities exist
    if !opportunities.is_empty() {
        // ** PRODUCTION-GRADE: Show only top 5 spreads
        let top_5: Vec<_> = opportunities.iter().take(5).collect();
        
        println!(
            "\n{} {} {}",
            "🎯".green(),
            "ARBITRAGE OPPORTUNITIES".bright_green().bold(),
            format!("(showing top {} of {})", top_5.len(), opportunities.len()).bright_black()
        );
        println!("{}", "-".repeat(80).bright_black());

        for (idx, opp) in top_5.iter().enumerate() {
            // ** PRODUCTION-GRADE: Colorize spreads based on magnitude
            // High spread (>0.5%): bright green
            // Medium spread (0.3-0.5%): green
            // Low spread (0.1-0.3%): yellow
            let spread_color = if opp.spread > 0.005 {
                "bright_green"
            } else if opp.spread > 0.003 {
                "green"
            } else {
                "yellow"
            };

            let spread_emoji = if opp.spread > 0.005 {
                "🟢"
            } else if opp.spread > 0.003 {
                "🟡"
            } else {
                "🟠"
            };

            // ** PRODUCTION-GRADE: Format output as specified
            // 🟢 [ARB] BONK 0.42% | Max: 0.0000125 (Jupiter) | Min: 0.00001245 (Orca)
            println!(
                "{}. {} [ARB] {} {:.2}% | Max: {} ({}) | Min: {} ({})",
                (idx + 1).to_string().bright_white(),
                spread_emoji,
                opp.token.bright_cyan().bold(),
                format!("{:.2}", opp.spread * 100.0)
                    .color(spread_color)
                    .bold(),
                format!("${:.4}", opp.max_price).bright_green(),
                opp.max_dex.bright_green(),
                format!("${:.4}", opp.min_price).bright_red(),
                opp.min_dex.bright_red(),
            );
        }
        
        if opportunities.len() > 5 {
            println!(
                "\n{} {} more opportunities logged to {}",
                "ℹ️".bright_black(),
                opportunities.len() - 5,
                "logs/arbitrage_log.jsonl".bright_black()
            );
        }
    }

    println!();
}
