use std::collections::HashMap;
use std::fmt::Debug;
use serde;
use serde::{Deserialize, Serialize};
use solana_sdk::account::Account;
use crate::serialize::token::{Token, WrappedPubkey, unpack_token_account};
use crate::serialize::pool::JSONFeeStructure; 
use crate::pool::PoolOperations;

use anchor_client::solana_sdk::pubkey::Pubkey;
use anchor_client::Cluster;
use anchor_client::Program;

use solana_sdk::instruction::Instruction;
use log::{warn, debug};

use tmp::accounts as tmp_accounts;
use tmp::instruction as tmp_ix;

use crate::pool_utils::base::CurveType;
use crate::utils::{str2pubkey, derive_token_address};
use crate::pool_utils::{
    orca::{get_pool_quote_with_amounts},
    fees::Fees,
};
use crate::constants::*;

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OrcaPool {
    pub address: WrappedPubkey,
    pub nonce: u64,
    pub authority: WrappedPubkey,
    pub pool_token_mint: WrappedPubkey,
    pub pool_token_decimals: u64,
    pub fee_account: WrappedPubkey,
    pub token_ids: Vec<String>,
    pub tokens: HashMap<String, Token>,
    pub fee_structure: JSONFeeStructure,
    pub curve_type: u8,
    #[serde(default)]
    pub amp: u64,
    // to set later 
    #[serde(skip)]
    pub pool_amounts: HashMap<String, u128>
}

impl PoolOperations for OrcaPool {
    fn swap_ix(&self, 
        program: &Program,
        owner: &Pubkey,
        mint_in: &Pubkey, 
        mint_out: &Pubkey
    ) -> Vec<Instruction> {
        let (swap_state, _) = Pubkey::find_program_address(
            &[b"swap_state"], 
            &program.id()
        );
        let user_src = derive_token_address(owner, mint_in);
        let user_dst = derive_token_address(owner, mint_out); 

        let (authority_pda, _) = Pubkey::find_program_address(
            &[&self.address.to_bytes()],
            &ORCA_PROGRAM_ID 
        );

        let pool_src = self.mint_2_addr(mint_in);
        let pool_dst = self.mint_2_addr(mint_out);

        let swap_ix = program
            .request()
            .accounts(tmp_accounts::OrcaSwap {
                token_swap: self.address.0, 
                authority: authority_pda,
                user_transfer_authority: *owner,
                user_src,
                pool_src,
                user_dst,
                pool_dst,
                pool_mint: self.pool_token_mint.0,
                fee_account: self.fee_account.0,
                token_program: *TOKEN_PROGRAM_ID,
                token_swap_program: *ORCA_PROGRAM_ID,
                swap_state,
            })
            .args(tmp_ix::OrcaSwap { })
            .instructions()
            .unwrap();

        swap_ix
    }

    fn get_quote_with_amounts_scaled(
        &self, 
        scaled_amount_in: u128, 
        mint_in: &Pubkey,
        mint_out: &Pubkey,
    ) -> u128 {
        
        // Handle missing pool amounts properly - this indicates pool wasn't updated correctly
        let pool_src_amount = match self.pool_amounts.get(&mint_in.to_string()) {
            Some(amount) => *amount,
            None => {
                warn!("Orca pool missing source amount for mint {}. Pool may not have been updated correctly.", mint_in);
                return 0;
            }
        };
        let pool_dst_amount = match self.pool_amounts.get(&mint_out.to_string()) {
            Some(amount) => *amount,
            None => {
                warn!("Orca pool missing destination amount for mint {}. Pool may not have been updated correctly.", mint_out);
                return 0;
            }
        };

        // compute fees 
        let trader_fee = &self.fee_structure.trader_fee;
        let owner_fee = &self.fee_structure.owner_fee;
        let fees = Fees {
            trade_fee_numerator: trader_fee.numerator,
            trade_fee_denominator: trader_fee.denominator,
            owner_trade_fee_numerator: owner_fee.numerator,
            owner_trade_fee_denominator: owner_fee.denominator,
            owner_withdraw_fee_numerator: 0,
            owner_withdraw_fee_denominator: 0,
            host_fee_numerator: 0,
            host_fee_denominator: 0,
        };
        let ctype = if self.curve_type == 0 { 
            CurveType::ConstantProduct 
        } else if self.curve_type == 2 {
            CurveType::Stable
        } else { 
            panic!("invalid self curve type: {:?}", self.curve_type);
        };

        // get quote -- works for either constant product or stable swap 
        // Handle quote calculation errors properly
        match get_pool_quote_with_amounts(
            scaled_amount_in,
            ctype,
            self.amp, 
            &fees, 
            pool_src_amount, 
            pool_dst_amount, 
            None,
        ) {
            Ok(quote) => quote,
            Err(e) => {
                warn!("Orca pool quote calculation failed for {} -> {}: {}. Pool may have invalid parameters.", 
                      mint_in, mint_out, e);
                0
            }
        }
    }

    fn get_update_accounts(&self) -> Vec<Pubkey> {
        // pool vault amount 
        let accounts = self
            .get_mints()
            .iter()
            .map(|mint| self.mint_2_addr(mint))
            .collect();        
        accounts 
    }

    fn can_trade(&self, 
        _mint_in: &Pubkey,
        _mint_out: &Pubkey
    ) -> bool {
        for amount in self.pool_amounts.values() {
            if *amount == 0 { return false; }
        }
        true
    }

    fn set_update_accounts(&mut self, accounts: Vec<Option<Account>>, _cluster: Cluster) { 
        let ids: Vec<String> = self
            .get_mints()
            .iter()
            .map(|mint| mint.to_string())
            .collect();
        let id0 = &ids[0];
        let id1 = &ids[1];
        
        // Handle missing accounts properly
        if accounts.len() < 2 {
            warn!("Orca pool set_update_accounts: Expected 2 accounts but got {}. Pool may not have been fetched correctly.", accounts.len());
            return;
        }
        
        // Handle None accounts properly
        let acc_data0 = match &accounts[0] {
            Some(acc) => &acc.data,
            None => {
                warn!("Orca pool set_update_accounts: Account 0 is None. Pool account may not exist.");
                return;
            }
        };
        let acc_data1 = match &accounts[1] {
            Some(acc) => &acc.data,
            None => {
                warn!("Orca pool set_update_accounts: Account 1 is None. Pool account may not exist.");
                return;
            }
        };

        // Validate account data before unpacking (165 bytes for SPL Token account)
        const TOKEN_ACCOUNT_DATA_SIZE: usize = 165;
        if acc_data0.len() != TOKEN_ACCOUNT_DATA_SIZE {
            warn!("Orca pool set_update_accounts: Account 0 has invalid data size: {} bytes (expected {}).", acc_data0.len(), TOKEN_ACCOUNT_DATA_SIZE);
            return;
        }
        if acc_data1.len() != TOKEN_ACCOUNT_DATA_SIZE {
            warn!("Orca pool set_update_accounts: Account 1 has invalid data size: {} bytes (expected {}).", acc_data1.len(), TOKEN_ACCOUNT_DATA_SIZE);
            return;
        }

        // Try to unpack - if it fails, the data is corrupted
        let amount0_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            unpack_token_account(acc_data0).amount as u128
        }));
        let amount1_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            unpack_token_account(acc_data1).amount as u128
        }));

        match (amount0_result, amount1_result) {
            (Ok(amount0), Ok(amount1)) => {
                // Log pool amounts for verification (real data from blockchain)
                debug!("Orca pool {}: Token {} amount = {} (scaled), Token {} amount = {} (scaled) - REAL DATA FROM BLOCKCHAIN", 
                       self.address.0, id0, amount0, id1, amount1);
                self.pool_amounts.insert(id0.clone(), amount0);
                self.pool_amounts.insert(id1.clone(), amount1);
            }
            (Err(_), _) => {
                warn!("Orca pool set_update_accounts: Failed to unpack account 0. Account data may be corrupted.");
            }
            (_, Err(_)) => {
                warn!("Orca pool set_update_accounts: Failed to unpack account 1. Account data may be corrupted.");
            }
        }
    }

    fn get_name(&self) -> String {
         
        "Orca".to_string()
    }

    fn mint_2_addr(&self, mint: &Pubkey) -> Pubkey {
        let token = self.tokens.get(&mint.to_string()).unwrap();
        
        token.addr.0
    }

    fn mint_2_scale(&self, mint: &Pubkey) -> u64 {
        let token = self.tokens.get(&mint.to_string()).unwrap();
                
        token.scale
    }

    fn get_mints(&self) -> Vec<Pubkey> {
        let mut mints: Vec<Pubkey> = self.token_ids
            .iter()
            .map(|k| str2pubkey(k))
            .collect();
        // sort so that its consistent across different pools 
        mints.sort();
        mints
    }
    
    fn get_pool_address(&self) -> Pubkey {
        self.address.0
    }
    
    fn get_pool_reserves(&self, mint_in: &Pubkey, mint_out: &Pubkey) -> Option<(u128, u128)> {
        let reserve_in = self.pool_amounts.get(&mint_in.to_string())?;
        let reserve_out = self.pool_amounts.get(&mint_out.to_string())?;
        Some((*reserve_in, *reserve_out))
    }
}