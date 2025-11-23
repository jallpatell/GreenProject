
use std::collections::HashMap;
use std::fmt::Debug;
use serde;
use serde::{Deserialize, Serialize};
use crate::serialize::token::{Token, WrappedPubkey, unpack_token_account};
use crate::pool::PoolOperations;

use anchor_client::solana_sdk::pubkey::Pubkey;
use anchor_client::Cluster;
use anchor_client::Program;

use solana_sdk::account::Account;
use solana_sdk::instruction::Instruction;
use log::{warn, debug};

use tmp::accounts as tmp_accounts;
use tmp::instruction as tmp_ix;

use crate::utils::{str2pubkey, derive_token_address};
use crate::constants::*;
use crate::pool_utils::stable::Stable;

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct SaberPool {
    pub pool_account: WrappedPubkey,
    pub authority: WrappedPubkey,
    pub pool_token_mint: WrappedPubkey,
    pub token_ids: Vec<String>,
    pub tokens: HashMap<String, Token>,
    pub target_amp: u64,
    pub fee_numerator: u64,
    pub fee_denominator: u64,
    // unique
    pub fee_accounts: HashMap<String, WrappedPubkey>,
    // to set later 
    #[serde(skip)]
    pub pool_amounts: HashMap<String, u128>
}

impl PoolOperations for SaberPool {
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
        
        let pool_src = self.tokens.get(&mint_in.to_string()).unwrap().addr.0;
        let pool_dst = self.tokens.get(&mint_out.to_string()).unwrap().addr.0;
        let fee_acc = self.fee_accounts.get(&mint_out.to_string()).unwrap();

        let swap_ix = program
            .request()
            .accounts(tmp_accounts::SaberSwap{
                pool_account: self.pool_account.0, 
                authority: self.authority.0, 
                user_transfer_authority: *owner, 
                user_src, 
                user_dst, 
                pool_src, 
                pool_dst, 
                fee_dst: fee_acc.0, 
                saber_swap_program: *SABER_PROGRAM_ID, 
                swap_state, 
                token_program: *TOKEN_PROGRAM_ID,
            }) 
            .args(tmp_ix::SaberSwap {}) 
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

        let calculator = Stable {
            amp: self.target_amp, 
            fee_numerator: self.fee_numerator as u128, 
            fee_denominator: self.fee_denominator as u128,
        };

        let pool_src_amount = match self.pool_amounts.get(&mint_in.to_string()) {
            Some(amount) => *amount,
            None => return 0,
        };
        let pool_dst_amount = match self.pool_amounts.get(&mint_out.to_string()) {
            Some(amount) => *amount,
            None => return 0,
        };
        let pool_amounts = [pool_src_amount, pool_dst_amount];
        let percision_multipliers = [1, 1];

        
        // Handle calculator errors properly - log when calculation fails
        match calculator.get_quote(
            pool_amounts,    
            percision_multipliers, 
            scaled_amount_in 
        ) {
            Some(quote) => quote,
            None => {
                // Calculator returned None - this indicates a calculation error
                // Log it for debugging but don't panic
                debug!("Saber pool calculator returned None for {} -> {}. This may indicate invalid pool amounts or calculation overflow.", 
                       mint_in, mint_out);
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

    fn set_update_accounts(&mut self, accounts: Vec<Option<Account>>, _cluster: Cluster) { 
        let ids: Vec<String> = self
            .get_mints()
            .iter()
            .map(|mint| mint.to_string())
            .collect();
        let id0 = &ids[0];
        let id1 = &ids[1];
        
        // Handle missing accounts properly - this indicates pool accounts weren't fetched
        if accounts.len() < 2 {
            warn!("Saber pool set_update_accounts: Expected 2 accounts but got {}. Pool may not have been fetched correctly.", accounts.len());
            return;
        }
        
        // Handle None accounts properly
        let acc_data0 = match &accounts[0] {
            Some(acc) => &acc.data,
            None => {
                warn!("Saber pool set_update_accounts: Account 0 is None. Pool account may not exist.");
                return;
            }
        };
        let acc_data1 = match &accounts[1] {
            Some(acc) => &acc.data,
            None => {
                warn!("Saber pool set_update_accounts: Account 1 is None. Pool account may not exist.");
                return;
            }
        };

        // Validate account data before unpacking (165 bytes for SPL Token account)
        const TOKEN_ACCOUNT_DATA_SIZE: usize = 165;
        if acc_data0.len() != TOKEN_ACCOUNT_DATA_SIZE {
            warn!("Saber pool set_update_accounts: Account 0 has invalid data size: {} bytes (expected {}).", acc_data0.len(), TOKEN_ACCOUNT_DATA_SIZE);
            return;
        }
        if acc_data1.len() != TOKEN_ACCOUNT_DATA_SIZE {
            warn!("Saber pool set_update_accounts: Account 1 has invalid data size: {} bytes (expected {}).", acc_data1.len(), TOKEN_ACCOUNT_DATA_SIZE);
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
        self.pool_amounts.insert(id0.clone(), amount0);
        self.pool_amounts.insert(id1.clone(), amount1);
            }
            (Err(_), _) => {
                warn!("Saber pool set_update_accounts: Failed to unpack account 0. Account data may be corrupted.");
            }
            (_, Err(_)) => {
                warn!("Saber pool set_update_accounts: Failed to unpack account 1. Account data may be corrupted.");
            }
        }
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

    fn get_name(&self) -> String {
         
        "Saber".to_string()
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
        self.pool_account.0
    }
    
    fn get_pool_reserves(&self, mint_in: &Pubkey, mint_out: &Pubkey) -> Option<(u128, u128)> {
        let reserve_in = self.pool_amounts.get(&mint_in.to_string())?;
        let reserve_out = self.pool_amounts.get(&mint_out.to_string())?;
        Some((*reserve_in, *reserve_out))
    }
}