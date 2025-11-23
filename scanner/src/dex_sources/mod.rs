pub mod jupiter;
pub mod birdeye;
pub mod dexscreener;
pub mod orca;
pub mod raydium;
pub mod meteora;
pub mod phoenix;
pub mod openbook;
pub mod fluxbeam;

use anyhow::Result;
use async_trait::async_trait;

/// ** PRODUCTION-GRADE: Trait for DEX price sources
/// Each DEX implements this trait to provide a unified interface for price fetching
#[async_trait]
pub trait DexPriceSource: Send + Sync {
    /// Get the name of the DEX
    fn name(&self) -> &str;
    
    /// Fetch price for a token (by address)
    /// Returns None if price is not available or request fails
    async fn get_price(&self, token_address: &str) -> Result<Option<f64>>;
}

pub use jupiter::Jupiter;
pub use birdeye::Birdeye;
pub use dexscreener::DexScreener;
pub use orca::Orca;
pub use raydium::Raydium;
pub use meteora::Meteora;
pub use phoenix::Phoenix;
pub use openbook::OpenBook;
pub use fluxbeam::FluxBeam;

