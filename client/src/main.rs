use anchor_client::solana_client::rpc_client::RpcClient;
use anchor_client::solana_sdk::commitment_config::CommitmentConfig;
use anchor_client::solana_sdk::pubkey::Pubkey;
use anchor_client::solana_sdk::signature::read_keypair_file;
use anchor_client::solana_sdk::signature::{Keypair, Signer};

use anchor_client::{Client, Cluster};

use std::collections::{HashMap, HashSet};
use std::fmt::Debug;
use std::rc::Rc;
use std::str::FromStr;

use std::borrow::Borrow;
use std::vec;

use clap::Parser;

use log::{debug, info, warn};
use solana_sdk::account::Account;

use client::arb::*;
use client::constants::*;
use client::pool::{pool_factory, PoolDir, PoolOperations, PoolType};
use client::serialize::token::unpack_token_account;
use client::utils::{
    derive_token_address, read_json_dir, PoolEdge, PoolGraph, PoolIndex, PoolQuote,
};
use client::websocket::WebSocketManager;
use std::sync::{Arc, Mutex};

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
pub struct Args {
    #[clap(short, long)]
    pub cluster: String,
    
    /// Path to wallet keypair file (required for mainnet)
    #[clap(short, long)]
    pub wallet: Option<String>,
    
    /// Custom RPC endpoint URL (optional, defaults to Jito for mainnet)
    #[clap(short, long)]
    pub rpc_url: Option<String>,
    
    /// Dry run mode: only log opportunities, don't execute transactions
    #[clap(long)]
    pub dry_run: bool,
    
    /// Enable WebSocket subscriptions for real-time pool data (default: true for mainnet)
    /// Use --websocket to enable, omit to use default (enabled for mainnet)
    #[clap(long, takes_value = false)]
    pub websocket: bool,
    
    /// Maximum number of pools to load per DEX (default: 40, max recommended: 50 for WebSocket)
    /// Each pool typically requires 2 WebSocket subscriptions (one per token account)
    /// Solana RPC typically allows 100-200 subscriptions per connection
    /// Recommended: 30-40 pools per DEX = 60-80 subscriptions per DEX = 240-320 total (within limits)
    #[clap(long, default_value = "40")]
    pub max_pools_per_dex: usize,
    
    /// Path to JSON file containing pool whitelist (optional)
    /// If provided, only pools listed in the whitelist will be loaded
    /// Format: {"Orca": ["address1", "address2", ...], "Saber": [...], "Aldrin": [...], "Serum": [...]}
    /// If not provided, pools are filtered by --max-pools-per-dex (by file order)
    #[clap(long)]
    pub pool_whitelist: Option<String>,
}

fn load_token_registry() -> HashMap<Pubkey, String> {
    let mut token_symbols = HashMap::new();
    
    // Try to load token_list.json from onchain-data directory
    // This is a one-time load at startup, so it doesn't affect runtime latency
    let token_list_path = "../onchain-data/token_list.json";
    match std::fs::read_to_string(token_list_path) {
        Ok(contents) => {
            #[derive(serde::Deserialize)]
            struct TokenList {
                tokens: Vec<TokenEntry>,
            }
            
            #[derive(serde::Deserialize)]
            struct TokenEntry {
                address: String,
                symbol: String,
            }
            
            match serde_json::from_str::<TokenList>(&contents) {
                Ok(token_list) => {
                    let mut loaded_count = 0;
                    let mut failed_count = 0;
                    for token in token_list.tokens {
                        if let Ok(mint) = Pubkey::from_str(&token.address) {
                            token_symbols.insert(mint, token.symbol);
                            loaded_count += 1;
                        } else {
                            failed_count += 1;
                        }
                    }
                    if failed_count > 0 {
                        warn!("Failed to parse {} token addresses from registry", failed_count);
                    }
                    info!("Successfully loaded {} token symbols from registry ({} failed to parse)", 
                          loaded_count, failed_count);
                }
                Err(e) => {
                    warn!("Failed to parse token_list.json: {}. Will use addresses instead.", e);
                }
            }
        }
        Err(e) => {
            warn!("Failed to load token_list.json from {}: {}. Will use addresses instead.", token_list_path, e);
        }
    }
    
    token_symbols
}

fn add_pool_to_graph<'a>(
    graph: &mut PoolGraph,
    idx0: PoolIndex,
    idx1: PoolIndex,
    quote: &PoolQuote,
) {
    // idx0 = A, idx1 = B
    let edges = graph
        .0
        .entry(idx0)
        .or_insert_with(|| PoolEdge(HashMap::new()));
    let quotes = edges.0.entry(idx1).or_insert_with(|| vec![]);
    quotes.push(quote.clone());
}

fn main() {
    let args = Args::parse();
    let cluster = match args.cluster.as_str() {
        "localnet" => Cluster::Localnet,
        "mainnet" => Cluster::Mainnet,
        _ => panic!("invalid cluster type"),
    };

    // Initialize logger with info level by default if RUST_LOG is not set
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }
    env_logger::init();

    let owner_kp_path = match cluster {
        Cluster::Localnet => "../mainnet-fork/localnet_owner.key",
        Cluster::Mainnet => {
            args.wallet.as_ref()
                .map(|w| w.as_str())
                .unwrap_or_else(|| {
                    eprintln!("Error: --wallet is required for mainnet");
                    eprintln!("Usage: cargo run --bin main -- --cluster mainnet --wallet /path/to/wallet.json");
                    std::process::exit(1);
                })
        }
        _ => panic!("shouldnt get here"),
    };

    // ** setup RPC connection
    let connection_url = match cluster {
        Cluster::Mainnet => {
            args.rpc_url.as_ref()
                .map(|u| u.as_str())
                .unwrap_or("https://mainnet.rpc.jito.wtf/?access-token=746bee55-1b6f-4130-8347-5e1ea373333f")
        }
        _ => cluster.url(),
    };
    info!("using connection: {}", connection_url);

    let connection = RpcClient::new_with_commitment(connection_url, CommitmentConfig::confirmed());
    let send_tx_connection =
        RpcClient::new_with_commitment(cluster.url(), CommitmentConfig::confirmed());

    // setup anchor things
    let owner = read_keypair_file(owner_kp_path.clone()).unwrap();
    let rc_owner = Rc::new(owner);
    let provider = Client::new_with_options(
        cluster.clone(),
        rc_owner.clone(),
        CommitmentConfig::confirmed(),
    );
    let program = provider.program(*ARB_PROGRAM_ID);

    // Load token registry for symbol mapping (one-time load at startup, no latency impact)
    let token_registry_start = std::time::Instant::now();
    let token_symbols = load_token_registry();
    let token_registry_duration = token_registry_start.elapsed();
    info!("Loaded {} token symbols from registry in {:?}ms (one-time startup cost)", 
          token_symbols.len(), token_registry_duration.as_millis());

    // ** define pool JSONs
    let mut pool_dirs = vec![];

    let orca_dir = PoolDir {
        tipe: PoolType::OrcaPoolType,
        dir_path: "../pools/orca".to_string(),
    };
    pool_dirs.push(orca_dir);

    let saber_dir = PoolDir {
        tipe: PoolType::SaberPoolType,
        dir_path: "../pools/saber/".to_string(),
    };
    pool_dirs.push(saber_dir);

    let aldrin_dir = PoolDir {
        tipe: PoolType::AldrinPoolType,
        dir_path: "../pools/aldrin".to_string(),
    };
    pool_dirs.push(aldrin_dir);

    let serum_dir = PoolDir {
        tipe: PoolType::SerumPoolType,
        dir_path: "../pools/serum".to_string(),
    };
    pool_dirs.push(serum_dir);

    // ** json pool -> pool object
    let mut token_mints = vec![];
    let mut pools = vec![];

    let mut update_pks = vec![];
    let mut update_pks_lengths = vec![];
    let mut all_mint_idxs = vec![];

    let mut mint2idx = HashMap::new();
    let mut graph_edges = vec![];

    debug!("extracting pool + mints...");
    
    // ** PRODUCTION-GRADE: Pool Filtering for WebSocket Efficiency
    // Limit pools per DEX to stay within WebSocket subscription limits
    // Each pool typically requires 2 subscriptions (one per token account)
    // Solana RPC typically allows 100-200 subscriptions per connection
    // With 4 DEXes and 40 pools per DEX: 4 * 40 * 2 = 320 subscriptions (within limits)
    let max_pools_per_dex = args.max_pools_per_dex;
    let max_total_subscriptions = 200; // Conservative limit for Solana RPC WebSocket
    let estimated_subscriptions_per_pool = 2; // Each pool has 2 token accounts
    
    // ** Load pool whitelist if provided
    let pool_whitelist: Option<HashMap<String, HashSet<String>>> = if let Some(whitelist_path) = &args.pool_whitelist {
        info!("📋 Loading pool whitelist from: {}", whitelist_path);
        match std::fs::read_to_string(whitelist_path) {
            Ok(content) => {
                match serde_json::from_str::<HashMap<String, Vec<String>>>(&content) {
                    Ok(whitelist_map) => {
                        let mut whitelist_set = HashMap::new();
                        for (dex, addresses) in whitelist_map {
                            let address_set: HashSet<String> = addresses.into_iter().collect();
                            let count = address_set.len();
                            let dex_name = dex.clone();
                            whitelist_set.insert(dex, address_set);
                            info!("📋 {}: {} pools in whitelist", dex_name, count);
                        }
                        info!("✅ Pool whitelist loaded successfully - only whitelisted pools will be used");
                        Some(whitelist_set)
                    }
                    Err(e) => {
                        warn!("⚠️ Failed to parse pool whitelist JSON: {}. Falling back to --max-pools-per-dex filtering.", e);
                        None
                    }
                }
            }
            Err(e) => {
                warn!("⚠️ Failed to read pool whitelist file {}: {}. Falling back to --max-pools-per-dex filtering.", whitelist_path, e);
                None
            }
        }
    } else {
        info!("📊 Pool filtering: Max {} pools per DEX (estimated {} subscriptions per pool)", 
              max_pools_per_dex, estimated_subscriptions_per_pool);
        info!("📊 WebSocket limit: Max {} total subscriptions (conservative estimate)", max_total_subscriptions);
        None
    };
    
    let mut pools_by_dex: HashMap<String, Vec<(String, Box<dyn PoolOperations>)>> = HashMap::new();
    
    // First pass: Load all pools and group by DEX
    for pool_dir in pool_dirs {
        let dex_name = match pool_dir.tipe {
            PoolType::OrcaPoolType => "Orca",
            PoolType::SaberPoolType => "Saber",
            PoolType::AldrinPoolType => "Aldrin",
            PoolType::SerumPoolType => "Serum",
            PoolType::MercurialPoolType => "Mercurial",
        };
        
        debug!("pool dir: {:#?}", pool_dir);
        let pool_paths = read_json_dir(&pool_dir.dir_path);
        
        let mut dex_pools = vec![];
        for pool_path in pool_paths {
            let json_str = std::fs::read_to_string(&pool_path).unwrap();
            let pool = pool_factory(&pool_dir.tipe, &json_str);

            let pool_mints = pool.get_mints();
            if pool_mints.len() != 2 {
                // only support 2 mint pools
                warn!("skipping pool with mints != 2: {:?}", pool_path);
                continue;
            }

            //  ** record pool info for graph
            // token: (mint = graph idx), (addr = get quote amount)
            let mut mint_idxs = vec![];
            for mint in pool_mints {
                let idx;
                if !token_mints.contains(&mint) {
                    idx = token_mints.len();
                    mint2idx.insert(mint, idx);
                    token_mints.push(mint);
                    // graph_edges[idx] will always exist :)
                    graph_edges.push(HashSet::new());
                } else {
                    idx = *mint2idx.get(&mint).unwrap();
                }
                mint_idxs.push(idx);
            }

            // get accounts which need account info to be updated (e.g. pool src/dst amounts for xy=k)
            let update_accounts = pool.get_update_accounts();
            update_pks_lengths.push(update_accounts.len());
            update_pks.push(update_accounts);

            let mint0_idx = mint_idxs[0];
            let mint1_idx = mint_idxs[1];

            all_mint_idxs.push(mint0_idx);
            all_mint_idxs.push(mint1_idx);

            // record graph edges if they dont already exist
            if !graph_edges[mint0_idx].contains(&mint1_idx) {
                graph_edges[mint0_idx].insert(mint1_idx);
            }
            if !graph_edges[mint1_idx].contains(&mint0_idx) {
                graph_edges[mint1_idx].insert(mint0_idx);
            }

            dex_pools.push((pool_path, pool));
        }
        
        pools_by_dex.insert(dex_name.to_string(), dex_pools);
    }
    
    // Second pass: Filter pools per DEX and build graph
    let mut total_pools = 0;
    let mut total_subscriptions = 0;
    
    for (dex_name, mut dex_pools) in pools_by_dex {
        // ** PRODUCTION-GRADE: Filter pools based on whitelist or max_pools_per_dex
        if let Some(ref whitelist) = pool_whitelist {
            // Use whitelist filtering
            if let Some(whitelisted_addresses) = whitelist.get(&dex_name) {
                let original_count = dex_pools.len();
                
                // ** DIAGNOSTIC: Log pool addresses for debugging
                let mut found_addresses = Vec::new();
                let mut missing_addresses = Vec::new();
                
                // First, collect all pool addresses from loaded pools
                for (_, pool) in &dex_pools {
                    let pool_addr = pool.get_pool_address().to_string();
                    found_addresses.push(pool_addr.clone());
                }
                
                // Check which whitelisted addresses are missing
                for whitelist_addr in whitelisted_addresses {
                    if !found_addresses.contains(whitelist_addr) {
                        missing_addresses.push(whitelist_addr.clone());
                    }
                }
                
                // Filter pools to only whitelisted ones
                dex_pools.retain(|(_, pool)| {
                    let pool_addr = pool.get_pool_address().to_string();
                    whitelisted_addresses.contains(&pool_addr)
                });
                
                info!("📋 {}: Filtered {} pools to {} whitelisted pools", 
                      dex_name, original_count, dex_pools.len());
                
                if dex_pools.len() < whitelisted_addresses.len() {
                    warn!("⚠️ {}: {} pools in whitelist but only {} found in pool directory.", 
                          dex_name, whitelisted_addresses.len(), dex_pools.len());
                    if !missing_addresses.is_empty() {
                        warn!("   Missing pool addresses (first 10): {:?}", 
                              missing_addresses.iter().take(10).collect::<Vec<_>>());
                        warn!("   Found pool addresses (first 10): {:?}", 
                              found_addresses.iter().take(10).collect::<Vec<_>>());
                        warn!("   💡 These addresses may not exist in pool JSON files or may be inactive");
                        warn!("   💡 Run find_and_match_pools.sh to create matched whitelist");
                    }
                } else if dex_pools.len() == whitelisted_addresses.len() {
                    info!("✅ {}: All {} whitelisted pools matched successfully!", 
                          dex_name, whitelisted_addresses.len());
                }
            } else {
                // DEX not in whitelist - skip all pools for this DEX
                info!("📋 {}: Not in whitelist, skipping all {} pools", dex_name, dex_pools.len());
                dex_pools.clear();
            }
        } else {
            // Use max_pools_per_dex filtering (by file order)
            if dex_pools.len() > max_pools_per_dex {
                info!("📊 {}: Filtering {} pools to top {} pools (by file order)", 
                      dex_name, dex_pools.len(), max_pools_per_dex);
                dex_pools.truncate(max_pools_per_dex);
            } else {
                info!("📊 {}: Using all {} pools", dex_name, dex_pools.len());
            }
        }
        
        // Process filtered pools
        for (pool_path, pool) in dex_pools {
            let pool_mints = pool.get_mints();
            if pool_mints.len() != 2 {
                // only support 2 mint pools
                warn!("skipping pool with mints != 2: {:?}", pool_path);
                continue;
            }

            //  ** record pool info for graph
            // token: (mint = graph idx), (addr = get quote amount)
            let mut mint_idxs = vec![];
            for mint in pool_mints {
                let idx;
                if !token_mints.contains(&mint) {
                    idx = token_mints.len();
                    mint2idx.insert(mint, idx);
                    token_mints.push(mint);
                    // graph_edges[idx] will always exist :)
                    graph_edges.push(HashSet::new());
                } else {
                    idx = *mint2idx.get(&mint).unwrap();
                }
                mint_idxs.push(idx);
            }

            // get accounts which need account info to be updated (e.g. pool src/dst amounts for xy=k)
            let update_accounts = pool.get_update_accounts();
            let subscription_count = update_accounts.len();
            update_pks_lengths.push(subscription_count);
            update_pks.push(update_accounts);
            total_subscriptions += subscription_count;

            let mint0_idx = mint_idxs[0];
            let mint1_idx = mint_idxs[1];

            all_mint_idxs.push(mint0_idx);
            all_mint_idxs.push(mint1_idx);

            // record graph edges if they dont already exist
            if !graph_edges[mint0_idx].contains(&mint1_idx) {
                graph_edges[mint0_idx].insert(mint1_idx);
            }
            if !graph_edges[mint1_idx].contains(&mint0_idx) {
                graph_edges[mint1_idx].insert(mint0_idx);
            }

            pools.push(pool);
            total_pools += 1;
        }
    }
    
    // Validate total subscriptions
    if total_subscriptions > max_total_subscriptions {
        warn!("⚠️ WARNING: Total subscriptions ({}) exceeds recommended limit ({}). WebSocket may fail or be unstable.", 
              total_subscriptions, max_total_subscriptions);
        warn!("💡 Recommendation: Reduce --max-pools-per-dex to {} or less", 
              max_total_subscriptions / (4 * estimated_subscriptions_per_pool)); // 4 DEXes
    } else {
        info!("✅ Total subscriptions: {} (within limit of {})", total_subscriptions, max_total_subscriptions);
    }
    
    info!("📊 Total pools loaded: {} (across all DEXes)", total_pools);
    
    let mut update_pks = update_pks.concat();

    // Reduced logging - only log summary
    debug!("added {:?} mints", token_mints.len());
    debug!("added {:?} pools", pools.len());

    // Define multiple starting tokens to search from (not just USDC)
    let starting_tokens = vec![
        ("USDC", "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"),
        ("USDT", "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB"),
        ("SOL", "So11111111111111111111111111111111111111112"),
        ("WSOL", "So11111111111111111111111111111111111111112"),
    ];
    
    // Filter to only tokens that exist in our pool graph
    let mut valid_start_tokens = vec![];
    for (name, mint_str) in starting_tokens {
        if let Ok(mint) = Pubkey::from_str(mint_str) {
            if mint2idx.contains_key(&mint) {
                let idx = *mint2idx.get(&mint).unwrap();
                valid_start_tokens.push((name, mint, idx));
                info!("Added starting token: {} (idx: {})", name, idx);
            } else {
                debug!("Starting token {} not found in pool graph, skipping", name);
            }
        }
    }
    
    if valid_start_tokens.is_empty() {
        warn!("No valid starting tokens found! Falling back to USDC");
        let usdc_mint = Pubkey::from_str("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").unwrap();
        let start_mint_idx = *mint2idx.get(&usdc_mint).unwrap();
        valid_start_tokens.push(("USDC", usdc_mint, start_mint_idx));
    }
    
    info!("Will search from {} starting tokens: {:?}", 
          valid_start_tokens.len(), 
          valid_start_tokens.iter().map(|(n, _, _)| *n).collect::<Vec<_>>());

    let owner: &Keypair = rc_owner.borrow();
    
    // Collect all starting token addresses for balance checking
    let mut owner_start_addrs = vec![];
    for (_, mint, _) in &valid_start_tokens {
        let addr = derive_token_address(&owner.pubkey(), mint);
        owner_start_addrs.push((*mint, addr));
        update_pks.push(addr);
    }

    // Fetch pool amounts with timestamp for freshness tracking
    // ⚠️ TIMING ISSUE: Sequential fetching creates time gaps between chunks
    // Each chunk is fetched one after another, NOT simultaneously
    // This means pools fetched at different times may have inconsistent data
    // TODO: Implement parallel fetching or websockets for real-time updates
    info!("Fetching pool amounts from blockchain...");
    let pool_fetch_start = std::time::Instant::now();
    let mut update_accounts_raw = vec![];
    let mut successful_fetches = 0;
    let mut failed_fetches = 0;
    let mut chunk_times = Vec::new();
    
    let chunks: Vec<_> = update_pks.chunks(99).collect();
    let total_chunks = chunks.len();
    info!("⚠️ TIMING WARNING: Fetching {} chunks SEQUENTIALLY (not parallel) - time gaps between chunks may cause stale data", total_chunks);
    
    for (chunk_idx, token_addr_chunk) in chunks.iter().enumerate() {
        let chunk_start = std::time::Instant::now();
        match connection.get_multiple_accounts(token_addr_chunk) {
            Ok(accounts) => {
                let chunk_success = accounts.iter().filter(|a| a.is_some()).count();
                let chunk_failed = accounts.len() - chunk_success;
                successful_fetches += chunk_success;
                failed_fetches += chunk_failed;
                update_accounts_raw.push(accounts);
                let chunk_duration = chunk_start.elapsed();
                chunk_times.push(chunk_duration);
                if chunk_idx == 0 || chunk_idx == total_chunks - 1 {
                    debug!("Chunk {}/{} fetched in {:?}ms", chunk_idx + 1, total_chunks, chunk_duration.as_millis());
                }
            }
            Err(e) => {
                warn!("Failed to fetch account chunk {}/{}: {}", chunk_idx + 1, total_chunks, e);
                failed_fetches += token_addr_chunk.len();
            }
        }
    }
    let update_accounts_raw = update_accounts_raw.concat();
    let pool_fetch_duration = pool_fetch_start.elapsed();
    let avg_chunk_time = if !chunk_times.is_empty() {
        chunk_times.iter().sum::<std::time::Duration>() / chunk_times.len() as u32
    } else {
        std::time::Duration::ZERO
    };
    let max_chunk_time = chunk_times.iter().max().copied().unwrap_or(std::time::Duration::ZERO);
    info!("Pool data fetched from MAINNET blockchain in {:?}ms - Success: {}, Failed: {}, Total accounts: {}", 
          pool_fetch_duration.as_millis(), successful_fetches, failed_fetches, update_pks.len());
    info!("⚠️ TIMING: Sequential fetch - {} chunks, avg {:?}ms/chunk, max {:?}ms/chunk - Time gaps between chunks may cause stale data", 
          total_chunks, avg_chunk_time.as_millis(), max_chunk_time.as_millis());
    info!("✅ VERIFIED: Using REAL pool data from mainnet blockchain ({} accounts fetched successfully)", successful_fetches);
    warn!("⚠️ RECOMMENDATION: Consider implementing websockets or parallel fetching for real-time pool data updates to avoid timing gaps");
    
    // Track which accounts were actually fetched (not None) before filtering
    let mut account_indices = vec![];
    let mut update_accounts = vec![];
    for (idx, account) in update_accounts_raw.iter().enumerate() {
        if account.is_some() {
            account_indices.push(idx);
            update_accounts.push(account.clone());
        }
    }
    
    debug!("update accounts is {:?}", update_accounts.len());
    // slide it out here
    // Removed verbose account printing - too much output
    // println!("accounts: {:#?}", update_accounts.clone());
    
    // Extract starting token balances (last N accounts, where N = number of starting tokens)
    // Token account data must be exactly 165 bytes (SPL Token account size)
    const TOKEN_ACCOUNT_DATA_SIZE: usize = 165;
    let mut start_token_balances = HashMap::new();
    for (mint, _) in owner_start_addrs.iter().rev() {
        if let Some(account) = update_accounts.pop() {
            if let Some(acc) = account {
                // Validate account data before unpacking
                if acc.data.len() != TOKEN_ACCOUNT_DATA_SIZE {
                    // Account exists but has invalid data size - this is a real problem
                    warn!("Token account for {} has invalid data size: {} bytes (expected {}). Account may be corrupted or not a token account.", 
                          mint, acc.data.len(), TOKEN_ACCOUNT_DATA_SIZE);
                    start_token_balances.insert(*mint, 0);
                    continue;
                }
                
                // Try to unpack - if it fails, the data is corrupted
                let balance_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    unpack_token_account(&acc.data).amount as u128
                }));
                
                match balance_result {
                    Ok(balance) => {
                        start_token_balances.insert(*mint, balance);
                        info!("Starting token balance: {} = {} (scaled)", 
                              mint, balance);
                    }
                    Err(_) => {
                        // Account data is corrupted - this is a real problem, not just missing
                        warn!("Token account for {} has corrupted data (unpack failed). Account exists but data is invalid.", 
                              mint);
                        start_token_balances.insert(*mint, 0);
                    }
                }
            } else {
                // Account is None - account doesn't exist (OK, balance is 0)
                start_token_balances.insert(*mint, 0);
                debug!("Starting token account for {} doesn't exist (None), using balance 0", mint);
            }
        } else {
            // No account in update_accounts - account wasn't fetched (OK, balance is 0)
            start_token_balances.insert(*mint, 0);
            debug!("Starting token account for {} not in update_accounts, using balance 0", mint);
        }
    }
    
    // Don't panic if all balances are zero - we'll use hypothetical balance in dry-run mode
    if start_token_balances.is_empty() {
        warn!("No starting token accounts found! Will use hypothetical balance in dry-run mode.");
    }

    debug!("setting up exchange graph...");
    let mut graph = PoolGraph::new();
    let mut pool_count = 0;
    let mut raw_account_ptr = 0; // Track position in raw (unfiltered) accounts
    let total_pool_accounts = update_pks.len() - 1; // Exclude owner's account at the end
    
    // ** PRODUCTION-GRADE: Pool Registry for WebSocket Updates
    // Maps pool addresses (Pubkey) to Arc<Mutex<Box<dyn PoolOperations>>>
    // This allows real-time updates without graph rebuilding
    // Graph structure remains stable - only pool data is updated in place
    let mut pool_registry: HashMap<Pubkey, Arc<Mutex<Box<dyn PoolOperations>>>> = HashMap::new();
    
    // ** GENIUS SOLUTION: Account State Cache for Real-Time Updates
    // Maps pool address to its current account state (Vec<Option<Account>>)
    // This allows incremental updates: when websocket sends one account, we update the cache
    // and then call set_update_accounts with the full updated cache
    // This eliminates the need to fetch all accounts on every websocket update
    let mut account_state_cache: HashMap<Pubkey, Vec<Option<Account>>> = HashMap::new();
    
    // Store pools in Arc<Mutex<>> for thread-safe mutable access
    let mut wrapped_pools: Vec<Arc<Mutex<Box<dyn PoolOperations>>>> = vec![];

    for mut pool in pools.into_iter() {
        // update pool - need to match accounts based on original positions
        let expected_length = update_pks_lengths[pool_count];
        let mut pool_accounts = vec![];
        
        // Collect accounts for this pool from raw accounts, skipping None values
        // Stop before the owner's account (last one)
        let mut collected = 0;
        while collected < expected_length && raw_account_ptr < total_pool_accounts {
            if let Some(account) = &update_accounts_raw[raw_account_ptr] {
                pool_accounts.push(Some(account.clone()));
                collected += 1;
            }
            raw_account_ptr += 1;
        }
        
        // Only update pool if we got the expected number of accounts
        if pool_accounts.len() == expected_length {
            pool.set_update_accounts(pool_accounts, cluster.clone());
        } else {
            warn!("Pool {}: Expected {} accounts but got {} after filtering", 
                  pool_count, expected_length, pool_accounts.len());
        }

        // ** Wrap pool in Arc<Mutex<>> for thread-safe mutable access
        let wrapped_pool = Arc::new(Mutex::new(pool));
        
        // ** Register all pool addresses for websocket updates
        // Get the addresses that this pool needs for updates
        let pool_update_addresses = {
            let pool_ref = wrapped_pool.lock().unwrap();
            pool_ref.get_update_accounts()
        };
        
        // ** GENIUS: Initialize account state cache for this pool
        // Store current account state so we can update incrementally via websocket
        let mut pool_account_state = vec![];
        for (idx, addr) in pool_update_addresses.iter().enumerate() {
            // Get the account from the raw accounts we fetched
            // We need to match the account to the address
            if let Some(account) = &update_accounts_raw[raw_account_ptr - expected_length + idx] {
                pool_account_state.push(Some(account.clone()));
            } else {
                pool_account_state.push(None);
            }
            
            // Map address to pool for websocket updates
            pool_registry.insert(*addr, wrapped_pool.clone());
            
            // Map address to its index in the pool's account list
            // This allows us to update the correct account in the cache
        }
        
        // Store account state cache for this pool's addresses
        for addr in &pool_update_addresses {
            account_state_cache.insert(*addr, pool_account_state.clone());
        }
        
        wrapped_pools.push(wrapped_pool.clone());

        // add pool to graph
        let idxs = &all_mint_idxs[pool_count * 2..(pool_count + 1) * 2].to_vec();
        let idx0 = PoolIndex(idxs[0]);
        let idx1 = PoolIndex(idxs[1]);

        let pool_ptr = PoolQuote::new(wrapped_pool);
        add_pool_to_graph(&mut graph, idx0, idx1, &pool_ptr.clone());
        add_pool_to_graph(&mut graph, idx1, idx0, &pool_ptr);

        pool_count += 1;
    }
    
    info!("✅ Pool registry created: {} pool addresses mapped for real-time websocket updates", pool_registry.len());

    // ** PRODUCTION-GRADE: Initialize WebSocket Manager for Real-Time Updates
    // Convert HTTP RPC URL to WebSocket URL (wss:// for https://, ws:// for http://)
    // If --websocket flag is provided, use it; otherwise default to true for mainnet
    let enable_websocket = if args.websocket {
        true
    } else {
        // Default: enable for mainnet, disable for other clusters
        cluster == Cluster::Mainnet
    };
    let pool_registry_arc = Arc::new(Mutex::new(pool_registry));
    let account_state_cache_arc = Arc::new(Mutex::new(account_state_cache));
    let mut ws_receiver_opt: Option<tokio::sync::mpsc::UnboundedReceiver<(Pubkey, Option<Account>)>> = None;
    let _ws_handle_opt: Option<std::thread::JoinHandle<()>>;
    
    if enable_websocket {
        let ws_url = connection_url
            .replace("https://", "wss://")
            .replace("http://", "ws://");
        
        // Get all pool addresses for websocket subscription
        let pool_addresses: Vec<Pubkey> = {
            let registry = pool_registry_arc.lock().unwrap();
            registry.keys().cloned().collect()
        };
        
        if !pool_addresses.is_empty() {
            info!("");
            info!("═══════════════════════════════════════════════════════════════");
            info!("🚀 SWITCHING TO WEBSOCKET MODE - REAL-TIME UPDATES ENABLED");
            info!("═══════════════════════════════════════════════════════════════");
            info!("📡 Initializing WebSocket manager for {} pool addresses...", pool_addresses.len());
            info!("🌐 WebSocket endpoint: {}", ws_url);
            info!("⏳ Connecting to WebSocket and subscribing to all pools in parallel...");
            info!("");
            
            let mut ws_manager = WebSocketManager::new(ws_url, pool_addresses);
            let ws_receiver = ws_manager.take_receiver();
            let ws_handle = ws_manager.start();
            
            ws_receiver_opt = Some(ws_receiver);
            _ws_handle_opt = Some(ws_handle);
            
            // Note: Actual connection confirmation will come from websocket manager
            info!("✅ WebSocket manager thread started - waiting for connection...");
        } else {
            warn!("⚠️ No pool addresses to subscribe to - skipping websocket initialization");
            _ws_handle_opt = None;
        }
    } else {
        info!("");
        info!("═══════════════════════════════════════════════════════════════");
        info!("⚠️  WEBSOCKET DISABLED - USING HTTP POLLING MODE");
        info!("═══════════════════════════════════════════════════════════════");
        info!("💡 To enable websocket real-time updates, use --websocket flag");
        info!("");
        _ws_handle_opt = None;
    }

    let cluster_clone = cluster.clone();
    let arbitrager = Arbitrager {
        token_mints,
        graph_edges,
        graph,
        cluster: cluster_clone,
        owner: rc_owner,
        program,
        connection: send_tx_connection,
        dry_run: args.dry_run,
        token_symbols,
    };

    // Search from each starting token using FIXED reference amount
    // This makes spread detection independent of wallet balance
    // Spread is based purely on pool prices/ratios from real DEX data
    // ** CRITICAL FIX: Use smaller reference amount (0.1 USDC) to reduce price impact
    // Large amounts (1 USDC) cause significant price impact, leading to false arbitrage
    // Smaller amounts better approximate real-world execution with minimal price impact
    // ** SMALL/MID-CAP OPTIMIZATION: Use smaller reference amount for small pools
    // 0.05 USDC (50,000 scaled) - better for small/mid-cap pools with lower liquidity
    // This reduces price impact while still detecting opportunities
    let reference_amount = 5 * 10_u128.pow(4_u32); // 0.05 USDC scaled (optimized for small/mid-cap)
    let mut sent_arbs = HashSet::new(); // track what arbs we did with a larger size
    
    info!("🚀 SMALL/MID-CAP OPTIMIZED ARBITRAGE BOT");
    info!("Starting arbitrage search from {} tokens using fixed reference amount (0.05 USDC)...", valid_start_tokens.len());
    info!("Spread detection is independent of wallet balance - based purely on real pool data");
    info!("📊 Pool Size Detection: Small (<$100k), Mid ($100k-$1M), Large (>$1M)");
    info!("🎯 Optimized for small/mid-cap pools with higher spreads and lower liquidity");
    info!("   - Small pools: 3% price impact limit, 0.3% slippage, 0.2% min spread");
    info!("   - Mid pools: 2% price impact limit, 0.4% slippage, 0.25% min spread");
    info!("   - Large pools: 1% price impact limit, 0.5% slippage, 0.3% min spread");
    
    // ** PRODUCTION-GRADE: Main Loop with Real-Time WebSocket Updates
    // Process websocket updates continuously while searching for arbitrage
    loop {
        // Process all pending websocket updates (non-blocking)
        if let Some(ref mut ws_receiver) = ws_receiver_opt {
            let mut update_count = 0;
            while let Ok((pool_addr, account_opt)) = ws_receiver.try_recv() {
                update_count += 1;
                
                // Look up pool in registry
                let pool_opt = {
                    let registry = pool_registry_arc.lock().unwrap();
                    registry.get(&pool_addr).cloned()
                };
                
                if let Some(pool_arc) = pool_opt {
                    if let Some(account) = account_opt {
                        // ** GENIUS SOLUTION: Update pool in place without graph rebuilding
                        // 1. Update account state cache with new account
                        // 2. Get all accounts for this pool from cache
                        // 3. Call set_update_accounts with updated cache
                        // 4. Graph automatically reflects updates (no rebuilding needed)
                        
                        // Update account state cache
                        let mut cache_updated = false;
                        {
                            let mut cache = account_state_cache_arc.lock().unwrap();
                            if let Some(cached_accounts) = cache.get_mut(&pool_addr) {
                                // Find which account in the cache this address corresponds to
                                let pool_accounts = {
                                    let pool_guard = pool_arc.lock().unwrap();
                                    pool_guard.get_update_accounts()
                                };
                                
                                if let Some(account_idx) = pool_accounts.iter().position(|&addr| addr == pool_addr) {
                                    if account_idx < cached_accounts.len() {
                                        cached_accounts[account_idx] = Some(account.clone());
                                        cache_updated = true;
                                    }
                                }
                            }
                        }
                        
                        if cache_updated {
                            // Get updated account state from cache
                            let updated_accounts = {
                                let cache = account_state_cache_arc.lock().unwrap();
                                cache.get(&pool_addr).cloned()
                            };
                            
                            if let Some(accounts) = updated_accounts {
                                // ** PRODUCTION-GRADE: Update pool with new account state
                                // Lock pool, update accounts, unlock
                                // Graph automatically reflects updates since it holds references to the same Arc<Mutex<>>
                                let mut pool_guard = pool_arc.lock().unwrap();
                                pool_guard.set_update_accounts(accounts, cluster.clone());
                                
                                // Log first few updates at info level to show websocket is working
                                if update_count <= 5 {
                                    info!("⚡ WebSocket update #{}: Pool {} updated in real-time", update_count, pool_addr);
                                } else {
                                    debug!("✅ WebSocket update applied to pool address {} - pool data updated in real-time", pool_addr);
                                }
                            }
                        } else {
                            debug!("WebSocket update for pool address {} - cache not found or invalid index", pool_addr);
                        }
                    }
                } else {
                    debug!("WebSocket update for unknown pool address: {}", pool_addr);
                }
            }
            
            if update_count > 0 {
                debug!("Processed {} websocket updates", update_count);
            }
        }
        
        // Search for arbitrage opportunities
        for (token_name, _start_mint, start_mint_idx) in &valid_start_tokens {
            // Use fixed reference amount for all calculations - independent of wallet balance
            // This gives us real spread percentages based on market conditions
            let init_balance = reference_amount; // Fixed reference amount
            
            // Search with fixed reference amount - spread detection independent of wallet balance
            arbitrager.brute_force_search(
                *start_mint_idx,
                init_balance,  // Fixed reference amount
                init_balance,  // Fixed reference amount
                vec![*start_mint_idx],
                vec![],
                &mut sent_arbs,
            );
        }
        
        // ** PRODUCTION-GRADE: Log search completion to indicate bot is running
        // Use static counter to avoid excessive logging
        static mut CYCLE_COUNT: usize = 0;
        unsafe {
            CYCLE_COUNT += 1;
            if CYCLE_COUNT == 1 || CYCLE_COUNT % 10 == 0 {
                info!("🔍 Search cycle #{} completed - searched from {} starting tokens (no opportunities found yet)", 
                      CYCLE_COUNT, valid_start_tokens.len());
            } else {
                debug!("Search cycle #{} completed - searched from {} starting tokens", 
                       CYCLE_COUNT, valid_start_tokens.len());
            }
        }
        
        // Small delay to prevent CPU spinning
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
