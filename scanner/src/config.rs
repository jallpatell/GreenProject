use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;

#[derive(Debug, Clone, Deserialize)]
pub struct TokenConfig {
    pub symbol: String,
    pub address: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DexConfig {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub requires_address: bool,
    #[serde(default)]
    pub response_path: Option<String>, // JSON path to price (e.g., "data.price" or "price")
}

#[derive(Debug, Deserialize)]
pub struct ScannerConfig {
    pub scanner: ScannerSettings,
    pub dexes: DexSettings,
}

#[derive(Debug, Deserialize)]
pub struct ScannerSettings {
    pub spread_threshold: f64,
    pub cycle_delay_ms: u64,
    pub log_file: String,
}

#[derive(Debug, Deserialize)]
pub struct DexSettings {
    pub enabled: Vec<String>,
}

pub fn load_tokens(path: &str) -> Result<Vec<TokenConfig>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read tokens config: {}", path))?;
    
    let tokens: Vec<TokenConfig> = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse tokens config: {}", path))?;
    
    Ok(tokens)
}

pub fn load_dexes(path: &str) -> Result<Vec<DexConfig>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read DEX config: {}", path))?;
    
    let dexes: Vec<DexConfig> = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse DEX config: {}", path))?;
    
    Ok(dexes)
}

pub fn load_config(path: &str) -> Result<ScannerConfig> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read config file: {}", path))?;
    
    let config: ScannerConfig = toml::from_str(&content)
        .with_context(|| format!("Failed to parse config file: {}", path))?;
    
    Ok(config)
}

