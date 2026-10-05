//! Versioned private allocation arithmetic shared by the claim circuit and
//! the on-chain transcript policy.
//!
//! A buy quantity is always denominated in base-token units. At the clearing
//! index only `buy` and `sell` at that exact index participate; quantities at
//! other price levels are not cumulative. The long side is prorated and is
//! rounded against the trader so the shared vault can never become insolvent.

/// First exact-index pro-rata allocation rule.
pub const ALLOCATION_RULE_VERSION: u16 = 1;
/// Token-2022 confidential transfers encode a 48-bit amount as three 16-bit
/// limbs. Claims must fit the same range.
pub const MAX_CONFIDENTIAL_TRANSFER_AMOUNT: u64 = (1_u64 << 48) - 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AllocationInput {
    /// Trader buy quantity at the clearing index, in base-token units.
    pub buy: u64,
    /// Trader sell quantity at the clearing index, in base-token units.
    pub sell: u64,
    /// Public aggregate demand at the clearing index.
    pub demand: u64,
    /// Public aggregate supply at the clearing index.
    pub supply: u64,
    /// Quote-token units paid per base-token unit.
    pub clearing_price: u64,
    /// Base collateral reserved by the order.
    pub base_reserve: u64,
    /// Quote collateral reserved by the order.
    pub quote_reserve: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AllocationOutput {
    pub base_debit: u64,
    pub base_credit: u64,
    pub quote_debit: u64,
    pub quote_credit: u64,
    pub base_payout: u64,
    pub quote_payout: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AllocationError {
    BothSidesAtClearingIndex,
    PrivateQuantityExceedsAggregate,
    InsufficientBaseReserve,
    InsufficientQuoteReserve,
    ArithmeticOverflow,
    TransferAmountOutOfRange,
}

pub type AllocationResult<T> = core::result::Result<T, AllocationError>;

/// Evaluate allocation rule v1 using checked integer arithmetic.
///
/// When demand exceeds supply, buyers are long: their base credit rounds down
/// and their quote debit rounds up. When supply exceeds demand, sellers are
/// long: their base debit rounds up and quote credit rounds down. This leaves
/// any indivisible dust in the common vault rather than creating value.
pub fn allocate(input: AllocationInput) -> AllocationResult<AllocationOutput> {
    if input.buy != 0 && input.sell != 0 {
        return Err(AllocationError::BothSidesAtClearingIndex);
    }
    if input.buy > input.demand || input.sell > input.supply {
        return Err(AllocationError::PrivateQuantityExceedsAggregate);
    }

    let buy = u128::from(input.buy);
    let sell = u128::from(input.sell);
    let demand = u128::from(input.demand);
    let supply = u128::from(input.supply);
    let price = u128::from(input.clearing_price);

    let (base_debit, base_credit, quote_debit, quote_credit) = if input.demand > input.supply {
        // Buyers are prorated; sellers execute in full.
        let buyer_numerator = checked_mul(buy, supply)?;
        let base_credit = buyer_numerator / demand;
        let quote_debit = ceil_div(checked_mul(buyer_numerator, price)?, demand)?;
        (sell, base_credit, quote_debit, checked_mul(sell, price)?)
    } else if input.supply > input.demand {
        // Sellers are prorated; buyers execute in full.
        let seller_numerator = checked_mul(sell, demand)?;
        let base_debit = ceil_div(seller_numerator, supply)?;
        let quote_credit = checked_mul(seller_numerator, price)? / supply;
        (base_debit, buy, checked_mul(buy, price)?, quote_credit)
    } else {
        // Includes the zero-volume case.
        (
            sell,
            buy,
            checked_mul(buy, price)?,
            checked_mul(sell, price)?,
        )
    };

    let base_payout = u128::from(input.base_reserve)
        .checked_sub(base_debit)
        .ok_or(AllocationError::InsufficientBaseReserve)?
        .checked_add(base_credit)
        .ok_or(AllocationError::ArithmeticOverflow)?;
    let quote_payout = u128::from(input.quote_reserve)
        .checked_sub(quote_debit)
        .ok_or(AllocationError::InsufficientQuoteReserve)?
        .checked_add(quote_credit)
        .ok_or(AllocationError::ArithmeticOverflow)?;

    let output = AllocationOutput {
        base_debit: narrow(base_debit)?,
        base_credit: narrow(base_credit)?,
        quote_debit: narrow(quote_debit)?,
        quote_credit: narrow(quote_credit)?,
        base_payout: narrow(base_payout)?,
        quote_payout: narrow(quote_payout)?,
    };
    if output.base_payout > MAX_CONFIDENTIAL_TRANSFER_AMOUNT
        || output.quote_payout > MAX_CONFIDENTIAL_TRANSFER_AMOUNT
    {
        return Err(AllocationError::TransferAmountOutOfRange);
    }
    Ok(output)
}

fn checked_mul(left: u128, right: u128) -> AllocationResult<u128> {
    left.checked_mul(right)
        .ok_or(AllocationError::ArithmeticOverflow)
}

fn ceil_div(numerator: u128, denominator: u128) -> AllocationResult<u128> {
    if denominator == 0 {
        return Err(AllocationError::ArithmeticOverflow);
    }
    let quotient = numerator / denominator;
    quotient
        .checked_add(u128::from(numerator % denominator != 0))
        .ok_or(AllocationError::ArithmeticOverflow)
}

fn narrow(value: u128) -> AllocationResult<u64> {
    u64::try_from(value).map_err(|_| AllocationError::ArithmeticOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(buy: u64, sell: u64, demand: u64, supply: u64) -> AllocationInput {
        AllocationInput {
            buy,
            sell,
            demand,
            supply,
            clearing_price: 7,
            base_reserve: 100,
            quote_reserve: 10_000,
        }
    }

    #[test]
    fn balanced_market_executes_both_sides_exactly() {
        let buyer = allocate(input(3, 0, 10, 10)).unwrap();
        assert_eq!((buyer.base_credit, buyer.quote_debit), (3, 21));
        let seller = allocate(input(0, 4, 10, 10)).unwrap();
        assert_eq!((seller.base_debit, seller.quote_credit), (4, 28));
    }

    #[test]
    fn long_buy_side_rounds_against_buyers() {
        let result = allocate(input(2, 0, 3, 2)).unwrap();
        assert_eq!(result.base_credit, 1); // floor(4/3)
        assert_eq!(result.quote_debit, 10); // ceil(28/3)
    }

    #[test]
    fn long_sell_side_rounds_against_sellers() {
        let result = allocate(input(0, 2, 2, 3)).unwrap();
        assert_eq!(result.base_debit, 2); // ceil(4/3)
        assert_eq!(result.quote_credit, 9); // floor(28/3)
    }

    #[test]
    fn zero_volume_returns_reserves() {
        let result = allocate(input(0, 0, 0, 0)).unwrap();
        assert_eq!(result.base_payout, 100);
        assert_eq!(result.quote_payout, 10_000);
    }

    #[test]
    fn invalid_private_values_are_rejected() {
        assert_eq!(
            allocate(input(1, 1, 2, 2)),
            Err(AllocationError::BothSidesAtClearingIndex)
        );
        assert_eq!(
            allocate(input(3, 0, 2, 2)),
            Err(AllocationError::PrivateQuantityExceedsAggregate)
        );
    }

    #[test]
    fn collateral_and_token_limits_are_enforced() {
        let mut insolvent = input(0, 2, 2, 3);
        insolvent.base_reserve = 1;
        assert_eq!(
            allocate(insolvent),
            Err(AllocationError::InsufficientBaseReserve)
        );

        let mut too_large = input(0, 0, 0, 0);
        too_large.base_reserve = MAX_CONFIDENTIAL_TRANSFER_AMOUNT + 1;
        assert_eq!(
            allocate(too_large),
            Err(AllocationError::TransferAmountOutOfRange)
        );
    }

    #[test]
    fn aggregate_rounding_never_overpays_the_long_side() {
        for demand in 1_u64..20 {
            for supply in 1_u64..20 {
                if demand > supply {
                    let total = allocate(input(demand, 0, demand, supply)).unwrap();
                    assert_eq!(total.base_credit, supply);
                    assert!(u128::from(total.quote_debit) >= u128::from(supply) * 7);
                } else if supply > demand {
                    let total = allocate(input(0, supply, demand, supply)).unwrap();
                    assert_eq!(total.base_debit, demand);
                    assert!(u128::from(total.quote_credit) <= u128::from(demand) * 7);
                }
            }
        }
    }
}
