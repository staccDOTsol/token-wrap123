//! Integer accounting kernel for a reserve/supply-priced Token-2022 wrapper.
//!
//! Amounts are raw units, with identical underlying/wrapper decimals. Reserve is
//! the spendable underlying escrow balance (excluding withheld transfer fees).
//! Supply includes fee-vault shares and permanently locked bootstrap shares.
//! This crate does not perform token CPIs or validate Solana accounts.
#![no_std]
#![forbid(unsafe_code)]

const BPS: u128 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidFee,
    InvalidState,
    ZeroOutput,
    Slippage,
    Overflow,
    LockedSupply,
}
type Result<T> = core::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeePolicy {
    pub mint_bps: u16,
    pub redeem_bps: u16,
    /// Required and nonzero; token-program fee caps are enforced separately.
    pub transfer_bps: u16,
    /// The remainder after LP/bid allocations is burned, including rounding dust.
    pub lp_bps: u16,
    pub bid_bps: u16,
}
impl FeePolicy {
    pub fn validate(self) -> Result<Self> {
        if self.mint_bps > 10_000
            || self.redeem_bps > 10_000
            || self.transfer_bps == 0
            || self.transfer_bps > 10_000
            || u32::from(self.lp_bps) + u32::from(self.bid_bps) >= 10_000
        {
            return Err(Error::InvalidFee);
        }
        Ok(self)
    }
    pub fn split(self, fee_shares: u64) -> Result<FeeSplit> {
        self.validate()?;
        let lp = mul_div(fee_shares, u64::from(self.lp_bps), 10_000)?;
        let bids = mul_div(fee_shares, u64::from(self.bid_bps), 10_000)?;
        Ok(FeeSplit {
            burn: fee_shares - lp - bids,
            lp,
            bids,
        })
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeSplit {
    pub burn: u64,
    pub lp: u64,
    pub bids: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct State {
    pub reserve: u64,
    pub supply: u64,
    /// Supply retained forever in an inaccessible bootstrap token account.
    /// These shares are NOT burned; burning them would remove the protection.
    pub locked_shares: u64,
}
impl State {
    fn validate(self) -> Result<Self> {
        if self.locked_shares == 0
            || (self.supply == 0 && self.reserve != 0)
            || (self.supply != 0 && (self.reserve == 0 || self.supply < self.locked_shares))
        {
            return Err(Error::InvalidState);
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deposit {
    pub user_shares: u64,
    pub newly_locked_shares: u64,
    pub fees: FeeSplit,
    pub after: State,
}

/// Quote from the measured escrow delta AFTER the underlying transfer.
/// Mint user + LP + bid + bootstrap shares. The burn allocation is never issued.
/// `before` must be captured before the CPI, not reconstructed from a quote API.
pub fn deposit(
    before: State,
    received: u64,
    min_user_shares: u64,
    policy: FeePolicy,
) -> Result<Deposit> {
    before.validate()?;
    policy.validate()?;
    if received == 0 || min_user_shares == 0 {
        return Err(Error::ZeroOutput);
    }
    let gross = if before.supply == 0 {
        received
    } else {
        mul_div(received, before.supply, before.reserve)?
    };
    let fee = charge(gross, policy.mint_bps)?;
    let locked = if before.supply == 0 {
        before.locked_shares
    } else {
        0
    };
    let user = gross
        .checked_sub(fee)
        .and_then(|n| n.checked_sub(locked))
        .ok_or(Error::ZeroOutput)?;
    if user == 0 {
        return Err(Error::ZeroOutput);
    }
    if user < min_user_shares {
        return Err(Error::Slippage);
    }
    let fees = policy.split(fee)?;
    let after = State {
        reserve: before
            .reserve
            .checked_add(received)
            .ok_or(Error::Overflow)?,
        supply: before
            .supply
            .checked_add(gross - fees.burn)
            .ok_or(Error::Overflow)?,
        ..before
    };
    after.validate()?;
    Ok(Deposit {
        user_shares: user,
        newly_locked_shares: locked,
        fees,
        after,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Redemption {
    /// Gross underlying debited from reserve. Recipient may receive less if the
    /// underlying has a transfer tax; enforce min-received using its actual delta.
    pub underlying_debit: u64,
    pub fees: FeeSplit,
    pub after: State,
}

/// Burn all `input_shares`, then mint only LP/bid fee shares to program vaults.
/// This is a direct burn, not a transfer of the input through the taxed mint.
pub fn redeem(
    before: State,
    input_shares: u64,
    min_gross_underlying: u64,
    policy: FeePolicy,
) -> Result<Redemption> {
    before.validate()?;
    policy.validate()?;
    if input_shares == 0 || min_gross_underlying == 0 {
        return Err(Error::ZeroOutput);
    }
    if input_shares > before.supply.saturating_sub(before.locked_shares) {
        return Err(Error::LockedSupply);
    }
    let fee = charge(input_shares, policy.redeem_bps)?;
    let underlying_debit = mul_div(input_shares - fee, before.reserve, before.supply)?;
    if underlying_debit == 0 {
        return Err(Error::ZeroOutput);
    }
    if underlying_debit < min_gross_underlying {
        return Err(Error::Slippage);
    }
    let fees = policy.split(fee)?;
    let after = State {
        reserve: before.reserve - underlying_debit,
        supply: before.supply - input_shares + fees.lp + fees.bids,
        ..before
    };
    after.validate()?;
    Ok(Redemption {
        underlying_debit,
        fees,
        after,
    })
}

/// Split already-issued, harvested transfer-fee shares. Only the burn portion
/// reduces supply. LP/bid shares retain backing until redeemed in a later leg.
pub fn harvest(
    before: State,
    collected_shares: u64,
    policy: FeePolicy,
) -> Result<(State, FeeSplit)> {
    before.validate()?;
    if collected_shares > before.supply.saturating_sub(before.locked_shares) {
        return Err(Error::LockedSupply);
    }
    let fees = policy.split(collected_shares)?;
    let after = State {
        supply: before.supply - fees.burn,
        ..before
    };
    after.validate()?;
    Ok((after, fees))
}

/// Token-2022-style ceiling fee, before any token-program maximum-fee cap.
pub fn charge(amount: u64, basis_points: u16) -> Result<u64> {
    if basis_points > 10_000 {
        return Err(Error::InvalidFee);
    }
    let numerator = u128::from(amount) * u128::from(basis_points);
    u64::try_from(numerator.div_ceil(BPS)).map_err(|_| Error::Overflow)
}
fn mul_div(a: u64, b: u64, denominator: u64) -> Result<u64> {
    if denominator == 0 {
        return Err(Error::InvalidState);
    }
    u64::try_from(u128::from(a) * u128::from(b) / u128::from(denominator))
        .map_err(|_| Error::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    const POLICY: FeePolicy = FeePolicy {
        mint_bps: 300,
        redeem_bps: 300,
        transfer_bps: 600,
        lp_bps: 3333,
        bid_bps: 3333,
    };
    const NO_ENTRY_EXIT: FeePolicy = FeePolicy {
        mint_bps: 0,
        redeem_bps: 0,
        ..POLICY
    };
    const STATE: State = State {
        reserve: 2_000_000,
        supply: 1_000_000,
        locked_shares: 1_000,
    };
    fn ratio_does_not_fall(before: State, after: State) {
        assert!(
            u128::from(after.reserve) * u128::from(before.supply)
                >= u128::from(before.reserve) * u128::from(after.supply)
        );
    }
    #[test]
    fn adjusted_deposit_and_fee_conservation() {
        let q = deposit(STATE, 20_000, 9_700, POLICY).unwrap();
        assert_eq!(q.user_shares, 9_700);
        assert_eq!(
            q.fees,
            FeeSplit {
                burn: 102,
                lp: 99,
                bids: 99
            }
        );
        assert_eq!(q.after.supply, 1_009_898);
        assert_eq!(q.after.reserve, 2_020_000);
        ratio_does_not_fall(STATE, q.after);
    }
    #[test]
    fn taxed_underlying_uses_net_receipt() {
        let q = deposit(STATE, 19_400, 9_700, NO_ENTRY_EXIT).unwrap();
        assert_eq!(q.user_shares, 9_700);
        assert_eq!(q.after.reserve, 2_019_400);
    }
    #[test]
    fn redemption_does_not_spend_burned_backing() {
        let q = redeem(STATE, 10_000, 19_400, POLICY).unwrap();
        assert_eq!(q.underlying_debit, 19_400);
        assert_eq!(q.after.supply, 990_198);
        assert_eq!(q.after.reserve, 1_980_600);
        ratio_does_not_fall(STATE, q.after);
    }
    #[test]
    fn transfer_harvest_never_debits_reserve() {
        let (after, fee) = harvest(STATE, 30_000, POLICY).unwrap();
        assert_eq!(fee.burn + fee.lp + fee.bids, 30_000);
        assert_eq!(after.reserve, STATE.reserve);
        assert_eq!(after.supply, STATE.supply - fee.burn);
        ratio_does_not_fall(STATE, after);
    }
    #[test]
    fn bootstrap_shares_are_retained_in_supply() {
        let q = deposit(
            State {
                reserve: 0,
                supply: 0,
                locked_shares: 1_000,
            },
            100_000,
            1,
            POLICY,
        )
        .unwrap();
        assert_eq!(q.newly_locked_shares, 1_000);
        assert_eq!(
            q.after.supply,
            q.user_shares + q.newly_locked_shares + q.fees.lp + q.fees.bids
        );
        assert_eq!(
            redeem(q.after, q.after.supply, 1, POLICY),
            Err(Error::LockedSupply)
        );
    }
    #[test]
    fn rejects_stranded_backing_and_unbacked_supply() {
        assert_eq!(
            deposit(
                State {
                    reserve: 7,
                    supply: 0,
                    locked_shares: 1
                },
                100,
                1,
                POLICY
            ),
            Err(Error::InvalidState)
        );
        assert_eq!(
            deposit(
                State {
                    reserve: 0,
                    ..STATE
                },
                100,
                1,
                POLICY
            ),
            Err(Error::InvalidState)
        );
    }
    #[test]
    fn donation_cannot_force_a_zero_output_deposit() {
        assert_eq!(
            deposit(
                State {
                    reserve: u64::MAX,
                    ..STATE
                },
                1,
                1,
                NO_ENTRY_EXIT
            ),
            Err(Error::ZeroOutput)
        );
        assert_eq!(
            deposit(
                State {
                    reserve: 4_000_000,
                    ..STATE
                },
                20_000,
                9_000,
                NO_ENTRY_EXIT
            ),
            Err(Error::Slippage)
        );
    }
    #[test]
    fn slippage_limits_and_zero_minimums() {
        assert_eq!(deposit(STATE, 20_000, 9_701, POLICY), Err(Error::Slippage));
        assert_eq!(redeem(STATE, 10_000, 19_401, POLICY), Err(Error::Slippage));
        assert_eq!(deposit(STATE, 20_000, 0, POLICY), Err(Error::ZeroOutput));
        assert_eq!(redeem(STATE, 10_000, 0, POLICY), Err(Error::ZeroOutput));
    }
    #[test]
    fn transfer_fee_is_mandatory_and_burn_allocation_nonzero() {
        assert_eq!(
            FeePolicy {
                transfer_bps: 0,
                ..POLICY
            }
            .validate(),
            Err(Error::InvalidFee)
        );
        assert_eq!(
            FeePolicy {
                lp_bps: 5000,
                bid_bps: 5000,
                ..POLICY
            }
            .validate(),
            Err(Error::InvalidFee)
        );
        assert_eq!(
            FeePolicy {
                transfer_bps: 10_001,
                ..POLICY
            }
            .validate(),
            Err(Error::InvalidFee)
        );
    }
    #[test]
    fn rounding_dust_goes_to_burn() {
        assert_eq!(charge(1, 1), Ok(1));
        assert_eq!(
            POLICY.split(1),
            Ok(FeeSplit {
                burn: 1,
                lp: 0,
                bids: 0
            })
        );
        assert_eq!(charge(u64::MAX, 10_000), Ok(u64::MAX));
    }
    #[test]
    fn overflow_is_rejected() {
        assert_eq!(
            deposit(
                State {
                    reserve: u64::MAX,
                    ..STATE
                },
                u64::MAX,
                1,
                POLICY
            ),
            Err(Error::Overflow)
        );
        assert_eq!(
            deposit(
                State {
                    reserve: 1,
                    supply: u64::MAX,
                    locked_shares: 1
                },
                2,
                1,
                POLICY
            ),
            Err(Error::Overflow)
        );
    }
    #[test]
    fn conservative_rounding_and_conservation_across_states() {
        for reserve in [10_000, 17_011, 1_000_000] {
            for supply in [9_999, 11_111, 500_000] {
                let before = State {
                    reserve,
                    supply,
                    locked_shares: 100,
                };
                for amount in [1_000, 2_351, 9_000] {
                    let d = deposit(before, amount, 1, POLICY).unwrap();
                    ratio_does_not_fall(before, d.after);
                    assert_eq!(
                        d.after.supply - supply,
                        d.user_shares + d.fees.lp + d.fees.bids
                    );
                    let r = redeem(before, amount, 1, POLICY).unwrap();
                    ratio_does_not_fall(before, r.after);
                    assert_eq!(supply - r.after.supply, amount - r.fees.lp - r.fees.bids);
                }
            }
        }
    }
}
