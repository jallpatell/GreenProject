use crate::arbitrage::Opportunity;
use anyhow::{Context, Result};
use chrono::Utc;
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

/// ** PRODUCTION-GRADE: Arbitrage log entry for JSONL serialization
/// ** REAL DATA ONLY: This logs actual prices fetched from live DEX APIs
#[derive(Debug, Serialize)]
pub struct ArbitrageLogEntry {
    pub timestamp: String,
    pub token_symbol: String,  // Coin name (e.g., "SOL", "BONK", "USDC")
    pub spread_percent: f64,   // Spread percentage (e.g., 0.27 = 0.27%)
    pub max_price: f64,         // Maximum price found across DEXes
    pub max_dex: String,        // DEX with maximum price
    pub min_price: f64,         // Minimum price found across DEXes
    pub min_dex: String,        // DEX with minimum price
    pub price_difference: f64,  // Absolute price difference (max - min)
}

impl From<&Opportunity> for ArbitrageLogEntry {
    fn from(opp: &Opportunity) -> Self {
        // ** REAL DATA: All prices are fetched from live DEX APIs (Jupiter, Birdeye, DexScreener, etc.)
        // ** NO MOCK DATA: These are actual market prices at the time of logging
        let price_difference = opp.max_price - opp.min_price;
        Self {
            timestamp: Utc::now().to_rfc3339(),
            token_symbol: opp.token.clone(),  // Coin name (e.g., "SOL", "BONK")
            spread_percent: opp.spread * 100.0, // Spread as percentage (e.g., 0.27 = 0.27%)
            max_price: opp.max_price,          // Real price from DEX API
            max_dex: opp.max_dex.clone(),      // Real DEX name
            min_price: opp.min_price,          // Real price from DEX API
            min_dex: opp.min_dex.clone(),      // Real DEX name
            price_difference,                  // Absolute difference in USD
        }
    }
}

pub struct ArbitrageLogger {
    log_file: String,
}

impl ArbitrageLogger {
    pub fn new(log_file: &str) -> Result<Self> {
        // Ensure log directory exists
        if let Some(parent) = Path::new(log_file).parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create log directory: {:?}", parent))?;
        }

        Ok(Self {
            log_file: log_file.to_string(),
        })
    }

    /// ** PRODUCTION-GRADE: Log arbitrage opportunity to JSONL file
    /// Uses atomic append operation to prevent corruption
    pub fn log_opportunity(&self, opportunity: &Opportunity) -> Result<()> {
        let entry = ArbitrageLogEntry::from(opportunity);
        let json_line = serde_json::to_string(&entry)
            .with_context(|| "Failed to serialize arbitrage entry")?;

        // ** PRODUCTION-GRADE: Atomic append with error handling
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_file)
            .with_context(|| format!("Failed to open log file: {}", self.log_file))?;

        writeln!(file, "{}", json_line)
            .with_context(|| format!("Failed to write to log file: {}", self.log_file))?;

        file.sync_all()
            .with_context(|| format!("Failed to sync log file: {}", self.log_file))?;

        Ok(())
    }
}

