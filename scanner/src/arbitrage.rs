use crate::config::TokenConfig;
use crate::price_fetcher::TokenPrices;
use std::cmp::Ordering;
use log;

pub struct ArbitrageDetector {
    threshold: f64, // Minimum spread to consider (e.g., 0.001 = 0.1%)
}

#[derive(Debug, Clone)]
pub struct Opportunity {
    pub token: String,
    pub max_price: f64,
    pub max_dex: String,
    pub min_price: f64,
    pub min_dex: String,
    pub spread: f64, // As decimal (e.g., 0.0027 = 0.27%)
}

impl ArbitrageDetector {
    pub fn new(threshold: f64) -> Self {
        Self { threshold }
    }

    /// ** PRODUCTION-GRADE: Detect arbitrage opportunities
    /// ** LOW-LATENCY: Optimized spread calculation with minimal allocations
    /// ** All prices are in USD (USDC equivalent) from Jupiter V3 API
    pub fn detect_opportunities(
        &self,
        tokens: &[TokenConfig],
        prices: &TokenPrices,
    ) -> Vec<Opportunity> {
        let mut opportunities = Vec::new();

        // ** LOW-LATENCY: Pre-allocate capacity for better performance
        opportunities.reserve(tokens.len() / 4); // Estimate ~25% of tokens will have opportunities

        // ** PRODUCTION-GRADE: Analyze prices across ALL DEXes for each coin
        // ** For each coin, compare prices from ALL available DEXes (Jupiter, Birdeye, DexScreener, etc.)
        // ** Find max and min prices across ALL DEXes, not just 2
        // ** All prices are in USD (USDC equivalent)

        for token in tokens {
            if let Some(dex_prices) = prices.get(&token.symbol) {
                // ** CRITICAL: Need at least 2 DEXes to compare prices for arbitrage
                if dex_prices.len() < 2 {
                    // Skip tokens with insufficient DEX coverage
                    continue;
                }

                // ** LOW-LATENCY: Single pass to find max and min prices
                // ** This compares prices from ALL DEXes (not just 2) to find the best arbitrage opportunity
                let (max_dex, max_price) = dex_prices
                    .iter()
                    .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(Ordering::Equal))
                    .unwrap();

                let (min_dex, min_price) = dex_prices
                    .iter()
                    .min_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(Ordering::Equal))
                    .unwrap();

                // ** LOW-LATENCY: Fast spread calculation
                // ** Formula: spread = (max_price - min_price) / min_price
                // ** All prices are in USD (USDC equivalent)
                let spread = (max_price - min_price) / min_price;

                // ** LOW-LATENCY: Early threshold check (avoid unnecessary allocations)
                if spread >= self.threshold {
                    // ** REAL OPPORTUNITY: Based on actual market prices from ALL DEX APIs
                    // ** All prices are in USD (USDC equivalent)
                    opportunities.push(Opportunity {
                        token: token.symbol.clone(),  // Coin name (e.g., "SOL", "BONK")
                        max_price: *max_price,        // Real USD price from DEX API (highest across ALL DEXes)
                        max_dex: max_dex.clone(),     // Real DEX name (highest price)
                        min_price: *min_price,        // Real USD price from DEX API (lowest across ALL DEXes)
                        min_dex: min_dex.clone(),     // Real DEX name (lowest price)
                        spread,                       // Real spread percentage (as decimal) across ALL DEXes
                    });
                }
            }
        }

        // ** LOW-LATENCY: Sort by spread descending (in-place sort)
        opportunities.sort_by(|a, b| b.spread.partial_cmp(&a.spread).unwrap_or(Ordering::Equal));

        opportunities
    }
}

