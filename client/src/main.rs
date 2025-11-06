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
    for pool_dir in pool_dirs {
        debug!("pool dir: {:#?}", pool_dir);
        let pool_paths = read_json_dir(&pool_dir.dir_path);

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

            pools.push(pool);
        }
    }
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
    info!("Fetching pool amounts from blockchain...");
    let pool_fetch_start = std::time::Instant::now();
    let mut update_accounts_raw = vec![];
    let mut successful_fetches = 0;
    let mut failed_fetches = 0;
    
    for token_addr_chunk in update_pks.chunks(99) {
        match connection.get_multiple_accounts(token_addr_chunk) {
            Ok(accounts) => {
                let chunk_success = accounts.iter().filter(|a| a.is_some()).count();
                let chunk_failed = accounts.len() - chunk_success;
                successful_fetches += chunk_success;
                failed_fetches += chunk_failed;
                update_accounts_raw.push(accounts);
            }
            Err(e) => {
                warn!("Failed to fetch account chunk: {}", e);
                failed_fetches += token_addr_chunk.len();
            }
        }
    }
    let update_accounts_raw = update_accounts_raw.concat();
    let pool_fetch_duration = pool_fetch_start.elapsed();
    info!("Pool data fetched in {:?}ms - Success: {}, Failed: {}, Total accounts: {}", 
          pool_fetch_duration.as_millis(), successful_fetches, failed_fetches, update_pks.len());
    
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
    let mut start_token_balances = HashMap::new();
    for (mint, _) in owner_start_addrs.iter().rev() {
        if let Some(account) = update_accounts.pop() {
            if let Some(acc) = account {
                let balance = unpack_token_account(&acc.data).amount as u128;
                start_token_balances.insert(*mint, balance);
                info!("Starting token balance: {} = {} (scaled)", 
                      mint, balance);
            }
        }
    }
    
    if start_token_balances.is_empty() {
        panic!("No starting token accounts found!");
    }

    debug!("setting up exchange graph...");
    let mut graph = PoolGraph::new();
    let mut pool_count = 0;
    let mut raw_account_ptr = 0; // Track position in raw (unfiltered) accounts
    let total_pool_accounts = update_pks.len() - 1; // Exclude owner's account at the end

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

        // add pool to graph
        let idxs = &all_mint_idxs[pool_count * 2..(pool_count + 1) * 2].to_vec();
        let idx0 = PoolIndex(idxs[0]);
        let idx1 = PoolIndex(idxs[1]);

        let mut pool_ptr = PoolQuote::new(Rc::new(pool));
        add_pool_to_graph(&mut graph, idx0, idx1, &mut pool_ptr.clone());
        add_pool_to_graph(&mut graph, idx1, idx0, &mut pool_ptr);

        pool_count += 1;
    }

    let arbitrager = Arbitrager {
        token_mints,
        graph_edges,
        graph,
        cluster,
        owner: rc_owner,
        program,
        connection: send_tx_connection,
    };

    // Search from each starting token
    let min_swap_amount = 10_u128.pow(6_u32); // scaled! -- 1 USDC (or equivalent)
    let mut sent_arbs = HashSet::new(); // track what arbs we did with a larger size
    
    info!("Starting arbitrage search from {} tokens...", valid_start_tokens.len());
    
    for (token_name, start_mint, start_mint_idx) in &valid_start_tokens {
        let init_token_balance = *start_token_balances.get(start_mint)
            .unwrap_or(&0);
        
        if init_token_balance == 0 {
            debug!("Skipping {} - zero balance", token_name);
            continue;
        }
        
        info!("Searching from {} (balance: {} scaled, idx: {})", 
              token_name, init_token_balance, start_mint_idx);
        
        let mut swap_start_amount = init_token_balance;
        
        for iteration in 0..4 {
            debug!("  {} iteration {}: swap amount {} (scaled)", 
                  token_name, iteration + 1, swap_start_amount);
            
            arbitrager.brute_force_search(
                *start_mint_idx,
                init_token_balance,
                swap_start_amount,
                vec![*start_mint_idx],
                vec![],
                &mut sent_arbs,
            );
            
            swap_start_amount /= 2; // half input amount and search again
            if swap_start_amount < min_swap_amount {
                debug!("  {} swap amount too small, stopping", token_name);
                break;
            }
        }
        
        info!("Completed search from {}", token_name);
    }
    
    // Log search completion to indicate bot is running
    info!("Search cycle completed - searched from {} starting tokens", valid_start_tokens.len());
}
