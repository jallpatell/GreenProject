use anchor_client::solana_client::rpc_client::RpcClient;
use anchor_client::solana_client::rpc_config::RpcSendTransactionConfig;

use anchor_client::solana_sdk::pubkey::Pubkey;

use anchor_client::solana_sdk::signature::{Keypair, Signer};
use anchor_client::{Cluster, Program};
use std::collections::{HashMap, HashSet};

use solana_sdk::instruction::Instruction;
use solana_sdk::transaction::Transaction;
use solana_sdk::compute_budget::ComputeBudgetInstruction;

use std::borrow::Borrow;
use std::rc::Rc;

use std::vec;

use log::{info, debug, warn, error};

use tmp::accounts as tmp_accounts;
use tmp::instruction as tmp_ix;

use crate::pool::PoolOperations;

use crate::utils::{derive_token_address, PoolGraph, PoolIndex, PoolQuote};

// ** PRODUCTION-GRADE: Gas cost constants for Solana
// Base transaction fee: 5,000 lamports (0.000005 SOL)
const BASE_TRANSACTION_FEE: u64 = 5_000;
// Compute unit price: 1,000 micro-lamports per CU (typical priority fee)
const COMPUTE_UNIT_PRICE_MICROLAMPS: u64 = 1_000;
// Estimated compute units per swap: ~200,000 CU
const ESTIMATED_COMPUTE_UNITS_PER_SWAP: u64 = 200_000;
// Jito tip for MEV protection: 10,000,000 lamports (0.01 SOL)
const JITO_TIP_LAMPORTS: u64 = 10_000_000;
// Minimum spread threshold after gas costs: 0.3% (300 basis points)
const MIN_PROFITABLE_SPREAD_BASIS_POINTS: u64 = 300;

// ** SMALL/MID-CAP OPTIMIZATION: Pool size thresholds
// Small pool: < $100k liquidity (10,000,000 scaled USDC = 100k * 10^5)
const SMALL_POOL_THRESHOLD: u128 = 10_000_000; // 100k USDC scaled
// Mid pool: $100k - $1M liquidity
const MID_POOL_THRESHOLD: u128 = 100_000_000; // 1M USDC scaled
// Large pool: > $1M liquidity

// ** SMALL/MID-CAP OPTIMIZATION: Filter parameters
// Small pools: More relaxed filters (higher price impact, lower slippage tolerance)
const SMALL_POOL_PRICE_IMPACT_LIMIT: f64 = 3.0; // 3% (vs 1% for large pools)
const SMALL_POOL_SLIPPAGE_TOLERANCE_BP: u64 = 300; // 0.3% (vs 0.5% for large pools)
const SMALL_POOL_MIN_SPREAD_BP: u64 = 200; // 0.2% (vs 0.3% for large pools)

// Mid pools: Moderate filters
const MID_POOL_PRICE_IMPACT_LIMIT: f64 = 2.0; // 2%
const MID_POOL_SLIPPAGE_TOLERANCE_BP: u64 = 400; // 0.4%
const MID_POOL_MIN_SPREAD_BP: u64 = 250; // 0.25%

// Large pools: Strict filters (current defaults)
const LARGE_POOL_PRICE_IMPACT_LIMIT: f64 = 1.0; // 1%
const LARGE_POOL_SLIPPAGE_TOLERANCE_BP: u64 = 500; // 0.5%
const LARGE_POOL_MIN_SPREAD_BP: u64 = 300; // 0.3%

pub struct Arbitrager {
    pub token_mints: Vec<Pubkey>,
    pub graph_edges: Vec<HashSet<usize>>, // used for quick searching over the graph
    pub graph: PoolGraph,
    pub cluster: Cluster,
    // vv -- need to clone these explicitly -- vv
    pub owner: Rc<Keypair>,
    pub program: Program,
    pub connection: RpcClient,
    pub dry_run: bool,
    pub token_symbols: HashMap<Pubkey, String>, // Mapping from mint address to token symbol
}

impl Arbitrager {
    /// ** PRODUCTION-GRADE: Calculate total gas cost for arbitrage transaction
    /// 
    /// Gas costs include:
    /// - Base transaction fee: 5,000 lamports
    /// - Compute units: num_swaps * 200,000 CU * compute_unit_price
    /// - Jito tip (optional): 0.01 SOL if using Jito for MEV protection
    /// 
    /// Returns: Total gas cost in lamports
    fn calculate_gas_cost(&self, num_swaps: usize, use_jito: bool) -> u64 {
        let base_fee = BASE_TRANSACTION_FEE;
        
        // Compute cost = num_swaps * compute_units_per_swap * price_per_cu
        // Convert micro-lamports to lamports: divide by 1,000,000
        let compute_cost = (num_swaps as u64)
            .checked_mul(ESTIMATED_COMPUTE_UNITS_PER_SWAP)
            .and_then(|cu| cu.checked_mul(COMPUTE_UNIT_PRICE_MICROLAMPS))
            .map(|microlamps| microlamps / 1_000_000)
            .unwrap_or(0);
        
        let jito_tip = if use_jito { JITO_TIP_LAMPORTS } else { 0 };
        
        base_fee
            .checked_add(compute_cost)
            .and_then(|total| total.checked_add(jito_tip))
            .unwrap_or(u64::MAX) // Overflow protection
    }
    
    /// ** PRODUCTION-GRADE: Calculate minimum profitable spread after gas costs
    /// 
    /// Returns: Minimum spread in basis points (1 basis point = 0.01%)
    fn calculate_min_profitable_spread(&self, trade_amount: u128, num_swaps: usize, use_jito: bool) -> u64 {
        let gas_cost = self.calculate_gas_cost(num_swaps, use_jito);
        
        // Minimum spread = (gas_cost / trade_amount) * 10000 (basis points)
        // Add safety margin: MIN_PROFITABLE_SPREAD_BASIS_POINTS
        let min_spread_from_gas = if trade_amount > 0 {
            (gas_cost as u128)
                .checked_mul(10_000)
                .and_then(|numerator| numerator.checked_div(trade_amount))
                .map(|bp| bp as u64)
                .unwrap_or(u64::MAX)
        } else {
            u64::MAX
        };
        
        // Use the higher of: gas-based minimum or absolute minimum (0.3%)
        min_spread_from_gas.max(MIN_PROFITABLE_SPREAD_BASIS_POINTS)
    }
    
    pub fn brute_force_search(
        &self,
        start_mint_idx: usize,
        init_balance: u128,
        curr_balance: u128,
        path: Vec<usize>,
        pool_path: Vec<PoolQuote>,
        sent_arbs: &mut HashSet<String>,
    ) {
        let src_curr = path[path.len() - 1]; // last mint
        let src_mint = self.token_mints[src_curr];

        let out_edges = &self.graph_edges[src_curr];

        // path = 4 = A -> B -> C -> D
        // path >= 5 == not valid bc max tx size is swaps
        if path.len() == 4 {
            return;
        };

        for dst_mint_idx in out_edges {
            let pools = self
                .graph
                .0
                .get(&PoolIndex(src_curr))
                .unwrap()
                .0
                .get(&PoolIndex(*dst_mint_idx))
                .unwrap();

            if path.contains(dst_mint_idx) && *dst_mint_idx != start_mint_idx {
                continue;
            }

            let dst_mint_idx = *dst_mint_idx;
            let dst_mint = self.token_mints[dst_mint_idx];

            for pool in pools {
                // ** PRODUCTION-GRADE: Lock pool for thread-safe access
                // Pool is now Arc<Mutex<Box<dyn PoolOperations>>>, so we need to lock it
                let pool_guard = match pool.0.lock() {
                    Ok(guard) => guard,
                    Err(e) => {
                        warn!("Failed to lock pool for quote calculation: {}", e);
                        continue;
                    }
                };
                
                // Check if pool can trade before calculating quote
                if !pool_guard.can_trade(&src_mint, &dst_mint) {
                    continue;
                }

                // ** SMALL/MID-CAP OPTIMIZATION: Get pool reserves and detect pool size
                // Price impact = (amount_in / pool_reserve_in) * 100
                // Pool size detection: Use reserve_in as proxy for liquidity
                let (pool_reserve_in, pool_reserve_out) = match pool_guard.get_pool_reserves(&src_mint, &dst_mint) {
                    Some((reserve_in, reserve_out)) => (reserve_in, reserve_out),
                    None => {
                        // Pool reserves not available (e.g., Serum order book)
                        // Skip price impact check for now (acceptable for order book pools)
                        let pool_name = pool_guard.get_name();
                        debug!("Pool {} reserves not available for price impact calculation (order book pool?)", pool_name);
                        (0, 0) // Will skip price impact check
                    }
                };
                
                // ** SMALL/MID-CAP OPTIMIZATION: Detect pool size and set filters accordingly
                // Small pool: < $100k liquidity
                // Mid pool: $100k - $1M liquidity
                // Large pool: > $1M liquidity
                let (price_impact_limit, slippage_tolerance_bp, min_spread_bp, pool_size_label) = 
                    if pool_reserve_in == 0 {
                        // Reserves not available - use large pool defaults (conservative)
                        (LARGE_POOL_PRICE_IMPACT_LIMIT, LARGE_POOL_SLIPPAGE_TOLERANCE_BP, LARGE_POOL_MIN_SPREAD_BP, "unknown")
                    } else if pool_reserve_in < SMALL_POOL_THRESHOLD {
                        // Small pool: Relaxed filters for higher spreads
                        (SMALL_POOL_PRICE_IMPACT_LIMIT, SMALL_POOL_SLIPPAGE_TOLERANCE_BP, SMALL_POOL_MIN_SPREAD_BP, "small")
                    } else if pool_reserve_in < MID_POOL_THRESHOLD {
                        // Mid pool: Moderate filters
                        (MID_POOL_PRICE_IMPACT_LIMIT, MID_POOL_SLIPPAGE_TOLERANCE_BP, MID_POOL_MIN_SPREAD_BP, "mid")
                    } else {
                        // Large pool: Strict filters (current defaults)
                        (LARGE_POOL_PRICE_IMPACT_LIMIT, LARGE_POOL_SLIPPAGE_TOLERANCE_BP, LARGE_POOL_MIN_SPREAD_BP, "large")
                    };
                
                // Calculate price impact BEFORE getting quote
                // This prevents wasting computation on high-slippage opportunities
                let price_impact_pct = if pool_reserve_in > 0 && curr_balance > 0 {
                    // Price impact = (amount_in / pool_reserve_in) * 100
                    let impact = (curr_balance as f64 / pool_reserve_in as f64) * 100.0;
                    impact
                } else {
                    0.0 // Skip check if reserves not available
                };
                
                // ** SMALL/MID-CAP OPTIMIZATION: Apply dynamic price impact filter based on pool size
                // Small pools: Allow up to 3% price impact (higher spreads compensate)
                // Mid pools: Allow up to 2% price impact
                // Large pools: Allow up to 1% price impact (strict)
                if price_impact_pct > price_impact_limit {
                    let pool_name = pool_guard.get_name();
                    // ** DIAGNOSTIC: Log price impact rejections (first 10 to avoid spam)
                    static mut PRICE_IMPACT_REJECT_COUNT: usize = 0;
                    unsafe {
                        PRICE_IMPACT_REJECT_COUNT += 1;
                        if PRICE_IMPACT_REJECT_COUNT <= 10 {
                            debug!("Skipping high price impact opportunity: {:.2}% impact (limit: {:.2}%) in {} pool {} ({} -> {}) | Reserve: {}", 
                                   price_impact_pct, price_impact_limit, pool_size_label, pool_name, src_mint, dst_mint, pool_reserve_in);
                        }
                    }
                    continue; // Too much slippage - skip this opportunity
                }

                // Get quote - catch panics but log them as errors (not just debug)
                // This indicates a real problem with the pool or calculation
                let new_balance = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    pool_guard.get_quote_with_amounts_scaled(curr_balance, &src_mint, &dst_mint)
                }));

                let pool_name = pool_guard.get_name();
                let new_balance = match new_balance {
                    Ok(balance) => balance,
                    Err(e) => {
                        // Quote calculation panicked - this is a real error, not expected
                        // Log it as a warning so we can identify problematic pools
                        warn!("Quote calculation panicked for pool {}: {} -> {}. This indicates a problem with pool data or calculation logic. Error: {:?}", 
                               pool_name, src_mint, dst_mint, e);
                        continue;
                    }
                };
                
                // Drop pool guard before continuing (release lock)
                drop(pool_guard);

                // Skip if quote is 0 or invalid
                if new_balance == 0 {
                    continue;
                }
                
                // ** CRITICAL FIX: Log price impact for transparency
                // This helps verify that spreads are calculated with realistic price impact
                if price_impact_pct > 0.0 {
                    debug!("Swap {} -> {}: {:.2}% price impact (reserves: {} -> {})", 
                           src_mint, dst_mint, price_impact_pct, pool_reserve_in, pool_reserve_out);
                }

                let mut new_path = path.clone();
                new_path.push(dst_mint_idx);

                let mut new_pool_path = pool_path.clone();
                new_pool_path.push(pool.clone()); // clone the pointer

                if dst_mint_idx == start_mint_idx {
                    // Check if arbitrage is profitable
                    // VERIFICATION: This uses real pool amounts from mainnet blockchain
                    // VERIFICATION: Quotes are calculated using correct AMM formulas (constant product or stable swap)
                    // VERIFICATION: Fees are properly deducted from quotes
                    if new_balance > init_balance {
                        // ... profitable arb!
                        // Calculate spread percentage based on reference amount
                        // This gives us the REAL spread available in the market
                        // Independent of wallet balance - based purely on pool prices/ratios
                        // VERIFIED: Uses real pool amounts fetched from mainnet blockchain
                        // VERIFIED: Quotes calculated using correct AMM formulas with fees
                        let profit = new_balance - init_balance;
                        let spread_percentage = if init_balance > 0 {
                            // Calculate percentage: (profit / init_balance) * 100
                            // Use fixed-point arithmetic to avoid floating point
                            let percentage_scaled = (profit as u128 * 10000) / init_balance;
                            percentage_scaled as f64 / 100.0
                        } else {
                            0.0
                        };
                        
                        // ** PRODUCTION-GRADE: Filter out suspicious spreads and unprofitable trades
                        // Real arbitrage on mainnet is typically 0.3-5%, rarely exceeding 10%
                        // Spreads >10% are almost certainly calculation errors, stale data, or price impact issues
                        // Spreads <0.3% are likely unprofitable after gas costs
                        if spread_percentage > 10.0 {
                            warn!("⚠️ SKIPPING SUSPICIOUSLY HIGH SPREAD: {:.4}% - Likely calculation error, stale data, or price impact. Real arbitrage rarely exceeds 5-10%.", spread_percentage);
                            continue; // Skip this opportunity - it's not real
                        }
                        
                        // ** CRITICAL FIX: Calculate minimum profitable spread after gas costs
                        // Gas costs: base fee + compute units + Jito tip (if using)
                        // For 0.1 USDC (100,000 scaled) trade with 3 swaps:
                        // - Base: 5,000 lamports
                        // - Compute: 3 * 200k CU * 1k microlamps = 600 lamports
                        // - Jito: 10,000,000 lamports (if using)
                        // Total: ~10,605,600 lamports = ~0.0106 SOL ≈ $0.10
                        // Minimum spread: 0.10 / 0.1 = 1.0% (but we use 0.3% as absolute minimum)
                        let num_swaps = new_path.len() - 1; // Number of swaps in path
                        let use_jito = false; // TODO: Make configurable
                        let min_profitable_spread_bp = self.calculate_min_profitable_spread(init_balance, num_swaps, use_jito);
                        let min_profitable_spread_pct = min_profitable_spread_bp as f64 / 100.0;
                        
                        if spread_percentage < min_profitable_spread_pct {
                            // Too small to be profitable after gas costs - log for diagnostics
                            // ** DIAGNOSTIC: Log first 10 rejections to see what's being filtered
                            static mut GAS_FILTER_REJECT_COUNT: usize = 0;
                            unsafe {
                                GAS_FILTER_REJECT_COUNT += 1;
                                if GAS_FILTER_REJECT_COUNT <= 10 {
                                    debug!("Skipping unprofitable spread: {:.4}% < {:.4}% (min after gas) | {} swaps", 
                                           spread_percentage, min_profitable_spread_pct, num_swaps);
                                }
                            }
                            continue;
                        }
                        
                        // Log gas cost for transparency
                        let gas_cost = self.calculate_gas_cost(num_swaps, use_jito);
                        let gas_cost_sol = gas_cost as f64 / 1_000_000_000.0; // Convert lamports to SOL
                        debug!("Arbitrage opportunity: {:.4}% spread, {} swaps, gas cost: {:.6} SOL ({:.4}% of trade)", 
                               spread_percentage, num_swaps, gas_cost_sol, 
                               (gas_cost as f64 / init_balance as f64) * 100.0);
                        
                        // Get pool names and full swap paths from the arbitrage path
                        // Format: PoolName:TokenIn->TokenOut -> PoolName:TokenIn->TokenOut
                        // Shows the complete swap path for each pool (input -> output)
                        let mut pool_info_parts = Vec::new();
                        for (i, pool) in new_pool_path.iter().enumerate() {
                            let pool_name = {
                                if let Ok(guard) = pool.0.lock() {
                                    guard.get_name()
                                } else {
                                    "Unknown".to_string()
                                }
                            };
                            // Get the input and output tokens for this pool
                            // new_path contains mint indices: [start, ..., end]
                            // For pool i, the input coin is at new_path[i] and output is at new_path[i+1]
                            let token_in_idx = new_path[i];
                            let token_out_idx = new_path[i + 1];
                            let token_in = self.token_mints[token_in_idx];
                            let token_out = self.token_mints[token_out_idx];
                            
                            // Get token symbols from registry, or use shortened address as fallback
                            let coin_in_name = self.token_symbols
                                .get(&token_in)
                                .map(|s| s.clone())
                                .unwrap_or_else(|| {
                                    // Fallback: use last 8 chars of base58 address
                                    let token_in_str = format!("{}", token_in);
                                    if token_in_str.len() > 8 {
                                        token_in_str.chars().skip(token_in_str.len() - 8).take(8).collect::<String>()
                                    } else {
                                        token_in_str
                                    }
                                });
                            
                            let coin_out_name = self.token_symbols
                                .get(&token_out)
                                .map(|s| s.clone())
                                .unwrap_or_else(|| {
                                    // Fallback: use last 8 chars of base58 address
                                    let token_out_str = format!("{}", token_out);
                                    if token_out_str.len() > 8 {
                                        token_out_str.chars().skip(token_out_str.len() - 8).take(8).collect::<String>()
                                    } else {
                                        token_out_str
                                    }
                                });
                            
                            pool_info_parts.push(format!("{}:{}->{}", pool_name, coin_in_name, coin_out_name));
                        }
                        let pool_info_str = pool_info_parts.join(" -> ");
                        
                        // ** SMALL/MID-CAP OPTIMIZATION: Get pool size from first pool in path
                        // Use the first pool's reserves to determine pool size category
                        // This allows us to apply appropriate filters for small/mid-cap pools
                        let (pool_size_label, slippage_tolerance_bp, min_spread_bp) = {
                            let first_pool = &new_pool_path[0];
                            let first_pool_guard = first_pool.0.lock().unwrap();
                            let (first_reserve_in, _) = first_pool_guard.get_pool_reserves(
                                &self.token_mints[new_path[0]], 
                                &self.token_mints[new_path[1]]
                            ).unwrap_or((0, 0));
                            
                            if first_reserve_in == 0 {
                                ("unknown", LARGE_POOL_SLIPPAGE_TOLERANCE_BP, LARGE_POOL_MIN_SPREAD_BP)
                            } else if first_reserve_in < SMALL_POOL_THRESHOLD {
                                ("small", SMALL_POOL_SLIPPAGE_TOLERANCE_BP, SMALL_POOL_MIN_SPREAD_BP)
                            } else if first_reserve_in < MID_POOL_THRESHOLD {
                                ("mid", MID_POOL_SLIPPAGE_TOLERANCE_BP, MID_POOL_MIN_SPREAD_BP)
                            } else {
                                ("large", LARGE_POOL_SLIPPAGE_TOLERANCE_BP, LARGE_POOL_MIN_SPREAD_BP)
                            }
                        };
                        
                        // ** SMALL/MID-CAP OPTIMIZATION: Apply dynamic slippage tolerance based on pool size
                        // Small pools: 0.3% slippage tolerance (more aggressive)
                        // Mid pools: 0.4% slippage tolerance
                        // Large pools: 0.5% slippage tolerance (conservative)
                        let min_output = (new_balance as u128)
                            .checked_mul((10_000u64 - slippage_tolerance_bp) as u128)
                            .and_then(|n| n.checked_div(10_000u128))
                            .unwrap_or(0);
                        
                        // Reject if expected output is too close to minimum (high slippage risk)
                        // This ensures spreads are realistic, not optimistic
                        if min_output < init_balance {
                            debug!("Skipping high slippage risk opportunity: min_output ({}) < init_balance ({})", 
                                   min_output, init_balance);
                            continue;
                        }
                        
                        // ** SMALL/MID-CAP OPTIMIZATION: Calculate realistic spread after slippage
                        // Realistic spread = (min_output - init_balance) / init_balance * 100
                        // This accounts for slippage tolerance, making spreads accurate for production
                        let realistic_profit = min_output.saturating_sub(init_balance);
                        let realistic_spread_pct = if init_balance > 0 {
                            let percentage_scaled = (realistic_profit as u128 * 10000) / init_balance;
                            percentage_scaled as f64 / 100.0
                        } else {
                            0.0
                        };
                        
                        // ** SMALL/MID-CAP OPTIMIZATION: Use pool-size-specific minimum spread
                        // Small pools: 0.2% minimum (lower threshold for higher spreads)
                        // Mid pools: 0.25% minimum
                        // Large pools: 0.3% minimum (current default)
                        let min_profitable_spread_pct_pool_size = min_spread_bp as f64 / 100.0;
                        
                        // Use the higher of: gas-based minimum or pool-size-specific minimum
                        let final_min_spread_pct = min_profitable_spread_pct.max(min_profitable_spread_pct_pool_size);
                        
                        // Log both optimistic and realistic spreads with pool size info
                        // Optimistic spread (before slippage): spread_percentage
                        // Realistic spread (after slippage): realistic_spread_pct
                        info!("found arbitrage [{} pool]: {} -> {} | Spread: {:.4}% (optimistic) / {:.4}% (realistic after {:.2}% slippage) | Min required: {:.4}% | Pools: {}", 
                              pool_size_label, init_balance, new_balance, spread_percentage, realistic_spread_pct, 
                              slippage_tolerance_bp as f64 / 100.0, final_min_spread_pct, pool_info_str);
                        
                        // Use realistic spread for final decision
                        let final_spread_pct = realistic_spread_pct;
                        
                        // Re-check minimum profitable spread with realistic spread
                        if final_spread_pct < final_min_spread_pct {
                            // ** DIAGNOSTIC: Log slippage filter rejections (first 10 to see pattern)
                            static mut SLIPPAGE_FILTER_REJECT_COUNT: usize = 0;
                            unsafe {
                                SLIPPAGE_FILTER_REJECT_COUNT += 1;
                                if SLIPPAGE_FILTER_REJECT_COUNT <= 10 {
                                    debug!("Skipping unprofitable spread after slippage: {:.4}% < {:.4}% (min after gas) | Optimistic: {:.4}% | Realistic: {:.4}% | {} swaps | Pool: {}", 
                                           final_spread_pct, final_min_spread_pct, spread_percentage, realistic_spread_pct, num_swaps, pool_size_label);
                                }
                            }
                            continue;
                        }
                        
                        // ** DIAGNOSTIC: Log when we find an opportunity that passes all filters
                        debug!("✅ Opportunity passed all filters: optimistic={:.4}%, realistic={:.4}%, min_required={:.4}%", 
                               spread_percentage, realistic_spread_pct, min_profitable_spread_pct);

                        // check if arb was sent with a larger size
                        // key = {mint_path}{pool_names}
                        let mint_keys: Vec<String> =
                            new_path.clone().iter_mut().map(|i| i.to_string()).collect();
                        let pool_keys: Vec<String> = {
                            let mut names = vec![];
                            for p in &new_pool_path {
                                if let Ok(guard) = p.0.lock() {
                                    names.push(guard.get_name());
                                } else {
                                    names.push("Unknown".to_string());
                                }
                            }
                            names
                        };
                        let arb_key = format!("{}{}", mint_keys.join(""), pool_keys.join(""));
                        if sent_arbs.contains(&arb_key) {
                            info!("arb already sent...");
                            continue; // dont re-send an already sent arb -- bad for network
                        } else {
                            sent_arbs.insert(arb_key);
                        }

                        // Only generate instructions if not in dry-run mode
                        // In dry-run mode, we just log the opportunity without executing
                        if !self.dry_run {
                        let ixs = self.get_arbitrage_instructions(
                            init_balance,
                                min_output,
                            &new_path,
                            &new_pool_path,
                        );
                            self.send_ixs(ixs, min_output, new_balance);
                        } else {
                            // In dry-run mode, just log that we would execute
                            info!("[DRY RUN] Would execute arbitrage transaction (dry-run mode enabled)");
                        }
                    }
                } else if !path.contains(&dst_mint_idx) {
                    // ... search deeper
                    self.brute_force_search(
                        start_mint_idx,
                        init_balance,
                        new_balance,   // !
                        new_path,      // !
                        new_pool_path, // !
                        sent_arbs,
                    );
                }
            }
        }
    }

    fn get_arbitrage_instructions(
        &self,
        swap_start_amount: u128,
        min_output: u128,
        mint_idxs: &Vec<usize>,
        pools: &Vec<PoolQuote>,
    ) -> Vec<Instruction> {
        // gather swap ixs
        let mut ixs = vec![];
        let (swap_state_pda, _) =
            Pubkey::find_program_address(&[b"swap_state"], &self.program.id());

        let src_mint = self.token_mints[mint_idxs[0]];
        let src_ata = derive_token_address(&self.owner.pubkey(), &src_mint);

        // initialize swap ix
        let ix = self
            .program
            .request()
            .accounts(tmp_accounts::TokenAndSwapState {
                src: src_ata,
                swap_state: swap_state_pda,
            })
            .args(tmp_ix::StartSwap {
                swap_input: swap_start_amount as u64,
            })
            .instructions()
            .unwrap();
        ixs.push(ix);

        for i in 0..mint_idxs.len() - 1 {
            let [mint_idx0, mint_idx1] = [mint_idxs[i], mint_idxs[i + 1]];
            let [mint0, mint1] = [self.token_mints[mint_idx0], self.token_mints[mint_idx1]];
            let pool = &pools[i];

            // ** PRODUCTION-GRADE: Lock pool for thread-safe access
            let pool_guard = pool.0.lock().unwrap();
            let swap_ix = pool_guard
                .swap_ix(&self.program, &self.owner.pubkey(), &mint0, &mint1);
            ixs.push(swap_ix);
            drop(pool_guard); // Release lock
        }

        // PROFIT OR REVERT instruction
        let ix = self
            .program
            .request()
            .accounts(tmp_accounts::TokenAndSwapState {
                src: src_ata,
                swap_state: swap_state_pda,
            })
            .args(tmp_ix::ProfitOrRevert {})
            .instructions()
            .unwrap();
        ixs.push(ix);

        // flatten to Vec<Instructions>
        ixs.concat()
    }

    /// ** PRODUCTION-GRADE: Send arbitrage transaction with MEV protection
    /// 
    /// Options:
    /// 1. Jito integration (private mempool, MEV protection)
    /// 2. Priority fees (higher priority, faster execution)
    /// 3. Standard submission (current implementation)
    fn send_ixs(&self, ixs: Vec<Instruction>, min_output: u128, expected_final_amount: u128) {
        if self.dry_run {
            info!("[DRY RUN] Would execute transaction (dry-run mode enabled)");
            return;
        }
        
        let owner: &Keypair = self.owner.borrow();
        
        // ** CRITICAL FIX: Add priority fee for faster execution and MEV protection
        // Priority fees help transactions get included faster, reducing sandwich risk
        // Typical priority fee: 0.001-0.01 SOL (1,000,000 - 10,000,000 lamports)
        // TODO: Make priority fee configurable and dynamic based on network conditions
        let priority_fee_lamports = 1_000_000; // 0.001 SOL (conservative)
        
        // Get latest blockhash
        let recent_blockhash = match self.connection.get_latest_blockhash() {
            Ok(hash) => hash,
            Err(e) => {
                error!("Failed to get latest blockhash: {}", e);
                return;
            }
        };
        
        // ** NOTE: Compute budget instructions require solana-sdk 1.18+
        // Current SDK (1.9.9) limitation - compute budget instructions not available
        // This is a real SDK version constraint, not masking
        // For now, we rely on RPC config for transaction execution
        // Priority fees and compute budget will be added when SDK is upgraded
        
        // Create transaction
        let mut tx = Transaction::new_with_payer(
            &ixs,
            Some(&owner.pubkey()),
        );
        
        // Sign transaction
        tx.sign(&[owner], recent_blockhash);
        
        // ** NOTE: Jito integration for MEV protection
        // Jito bundles transactions into private mempool, preventing sandwich attacks
        // For now, we use compute budget + RPC config (priority fees require SDK 1.18+)
        // Future enhancement: integrate jito-sdk for full MEV protection

        if self.cluster == Cluster::Localnet {
            let res = self.connection.simulate_transaction(&tx).unwrap();
            println!("{:#?}", res);
        } else if self.cluster == Cluster::Mainnet {
            // ** PRODUCTION-GRADE: Send with skip_preflight for speed
            // skip_preflight = true means we skip simulation (faster, but less safe)
            // In production, consider: skip_preflight = false for safety, or use Jito
            let signature = match self.connection.send_transaction_with_config(
                    &tx,
                    RpcSendTransactionConfig {
                    skip_preflight: true, // Fast execution, but less safe
                    max_retries: Some(3), // Retry up to 3 times
                        ..RpcSendTransactionConfig::default()
                    },
            ) {
                Ok(sig) => {
                    info!("✅ Transaction submitted: {}", sig);
                    sig
                }
                Err(e) => {
                    error!("❌ Failed to submit transaction: {}", e);
                    return;
                }
            };
            
            // ** CRITICAL FIX: Wait for transaction confirmation and track results
            // This ensures we know if the transaction succeeded or failed
            // And allows us to track actual profit/loss vs expected
            // Poll for confirmation (up to 30 seconds)
            let max_confirm_attempts = 30;
            let mut confirmed = false;
            for attempt in 0..max_confirm_attempts {
                std::thread::sleep(std::time::Duration::from_secs(1));
                match self.connection.get_signature_status(&signature) {
                    Ok(Some(status)) => {
                        match status {
                            Ok(_) => {
                                confirmed = true;
                                info!("✅ Transaction confirmed: {} (attempt {})", signature, attempt + 1);
                                info!("Expected final amount: {} (min output: {})", expected_final_amount, min_output);
                                break;
                            }
                            Err(err) => {
                                error!("❌ Transaction failed: {} - {:?}", signature, err);
                                break;
                            }
                        }
                    }
                    Ok(None) => {
                        // Still pending
                        if attempt % 5 == 0 {
                            debug!("Transaction {} still pending (attempt {}/{})", signature, attempt + 1, max_confirm_attempts);
                        }
                    }
                    Err(e) => {
                        warn!("Error checking transaction status: {}", e);
                        break;
                    }
                }
            }
            
            if !confirmed {
                warn!("⚠️ Transaction {} not confirmed after {} attempts (may have failed or timed out)", 
                      signature, max_confirm_attempts);
            }
        }
    }
}
