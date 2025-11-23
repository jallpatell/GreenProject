use anyhow::Result;
use crate::{
    pool_utils,
    pool_utils::base::{SwapCurve, CurveType},
    pool_utils::fees::Fees,
    pool_utils::{constant_product::ConstantProductCurve, stable::StableCurve},
};
use core::panic;
use std::sync::Arc;

pub fn get_pool_quote_with_amounts(
    amount_in: u128, 
    curve_type: CurveType, 
    amp: u64,
    fees: &Fees,
    input_token_pool_amount: u128,
    output_token_pool_amount: u128,
    slippage_percent: Option<[u128;2]>,
) -> Result<u128> {
    let mut quote;
    let trade_direction = pool_utils::calculator::TradeDirection::AtoB;
    
    if curve_type == CurveType::ConstantProduct { // constant product (1 for orca)
        let swap_curve = SwapCurve {
            curve_type: CurveType::ConstantProduct,
            calculator: Arc::new(ConstantProductCurve {}),
        };
        let swap_quote = swap_curve.swap(
            amount_in, 
            input_token_pool_amount, 
            output_token_pool_amount, 
            trade_direction, 
            fees
        );
        quote = match swap_quote {
            Some(v) => { v.destination_amount_swapped },
            None => {
                // println!("swap err: {} {} {}", amount_in, 
                //     input_token_pool_amount, 
                //     output_token_pool_amount);
                0
            }
        }

    } else if curve_type == CurveType::Stable { // stableswap (2 for orca)
        let swap_curve = SwapCurve {
            curve_type: CurveType::Stable,
            calculator: Arc::new(StableCurve { amp }),
        };
        // Handle swap calculation errors properly - None means calculation failed
        match swap_curve.swap(
            amount_in, 
            input_token_pool_amount, 
            output_token_pool_amount, 
            trade_direction, 
            fees
        ) {
            Some(swap_result) => quote = swap_result.destination_amount_swapped,
            None => {
                // Swap calculation returned None - this can happen with invalid pool amounts
                // Return error instead of panicking
                return Err(anyhow::anyhow!("Stable swap calculation returned None. This may indicate invalid pool amounts or parameters."));
            }
        }

    } else {
        panic!("invalid curve type for swap: {:?}", curve_type);
    }
            
    // add slippage amount if its given 
    if let Some([num, denom]) = slippage_percent {
        quote = quote * (denom - num) / denom
    }


    Ok(quote)
}
