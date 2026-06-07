//! LP Wrap program
//!
//! A unified wrapper for AMM liquidity-provider (LP) tokens.
//!
//! Where the `spl-token-wrap` program wraps a single mint 1:1, this program
//! wraps the LP token of an AMM *pool* into a single canonical "share" token
//! that is keyed off the **pair of underlying mints**, not off the LP mint
//! itself. That means LP tokens from different AMMs (Raydium AMM v4, Raydium
//! CPMM, PumpSwap, ...) that all represent the same `(mint_a, mint_b)` pair
//! collapse into **one** wrapped share token, agnostic of the source AMM.
//!
//! On `Wrap`, the caller supplies an LP token together with its AMM pool
//! account. The program deconstructs the pool, reads its two underlying mints
//! and its LP mint, and verifies:
//!   * the supplied LP mint exactly matches the pool's LP mint, and
//!   * the pool's two mints exactly match the pair this wrapped token is for.
//!
//! Unlike vanilla token-wrap, mint/burn is **not** 1:1. The wrapped token is a
//! vault share: the number of shares minted/burned is computed from the ratio
//! of total escrowed reserves to the current share supply. Consequently any
//! external donation of LP into an escrow, or any external burn of the share
//! supply, accrues pro-rata to every remaining share.
#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod entrypoint;
pub mod amm;
pub mod error;
pub mod instruction;
pub mod processor;
pub mod state;

use {
    solana_pubkey::Pubkey,
    spl_associated_token_account_interface::address::get_associated_token_address_with_program_id,
};

solana_pubkey::declare_id!("JCacx5xeDKuYW1GLjqwt46MzQqPZp3oPQ9WrGdyq9ppd");

/// Maximum number of distinct LP mints (i.e. source AMM pools) that may be
/// registered against a single wrapped pair. Three is enough for the three
/// natively-supported AMMs, the extra room allows multiple pools per AMM.
pub const MAX_LP_MINTS: usize = 8;

const WRAPPED_MINT_SEED: &[u8] = br"lp_mint";
const WRAPPED_MINT_AUTHORITY_SEED: &[u8] = br"authority";
const PAIR_CONFIG_SEED: &[u8] = br"config";

/// Returns the two mints in canonical (ascending byte) order so that the
/// wrapped token is independent of the order the caller passes them in.
pub fn sort_pair(mint_x: &Pubkey, mint_y: &Pubkey) -> (Pubkey, Pubkey) {
    if mint_x.to_bytes() <= mint_y.to_bytes() {
        (*mint_x, *mint_y)
    } else {
        (*mint_y, *mint_x)
    }
}

pub(crate) fn get_wrapped_mint_seeds<'a>(
    mint_a: &'a Pubkey,
    mint_b: &'a Pubkey,
    wrapped_token_program_id: &'a Pubkey,
) -> [&'a [u8]; 4] {
    [
        WRAPPED_MINT_SEED,
        mint_a.as_ref(),
        mint_b.as_ref(),
        wrapped_token_program_id.as_ref(),
    ]
}

pub(crate) fn get_wrapped_mint_signer_seeds<'a>(
    mint_a: &'a Pubkey,
    mint_b: &'a Pubkey,
    wrapped_token_program_id: &'a Pubkey,
    bump_seed: &'a [u8],
) -> [&'a [u8]; 5] {
    [
        WRAPPED_MINT_SEED,
        mint_a.as_ref(),
        mint_b.as_ref(),
        wrapped_token_program_id.as_ref(),
        bump_seed,
    ]
}

/// Derive the wrapped share mint for a `(mint_a, mint_b)` pair and a chosen
/// wrapped token program. The pair is sorted internally, so argument order does
/// not matter.
pub fn get_wrapped_mint_address(
    mint_x: &Pubkey,
    mint_y: &Pubkey,
    wrapped_token_program_id: &Pubkey,
) -> Pubkey {
    get_wrapped_mint_address_with_seed(mint_x, mint_y, wrapped_token_program_id).0
}

pub(crate) fn get_wrapped_mint_address_with_seed(
    mint_x: &Pubkey,
    mint_y: &Pubkey,
    wrapped_token_program_id: &Pubkey,
) -> (Pubkey, u8) {
    let (mint_a, mint_b) = sort_pair(mint_x, mint_y);
    Pubkey::find_program_address(
        &get_wrapped_mint_seeds(&mint_a, &mint_b, wrapped_token_program_id),
        &id(),
    )
}

pub(crate) fn get_wrapped_mint_authority_seeds(wrapped_mint: &Pubkey) -> [&[u8]; 2] {
    [WRAPPED_MINT_AUTHORITY_SEED, wrapped_mint.as_ref()]
}

pub(crate) fn get_wrapped_mint_authority_signer_seeds<'a>(
    wrapped_mint: &'a Pubkey,
    bump_seed: &'a [u8],
) -> [&'a [u8]; 3] {
    [
        WRAPPED_MINT_AUTHORITY_SEED,
        wrapped_mint.as_ref(),
        bump_seed,
    ]
}

pub(crate) fn get_wrapped_mint_authority_with_seed(wrapped_mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&get_wrapped_mint_authority_seeds(wrapped_mint), &id())
}

/// Derive the mint authority PDA that controls the wrapped share mint and owns
/// every LP escrow for the pair.
pub fn get_wrapped_mint_authority(wrapped_mint: &Pubkey) -> Pubkey {
    get_wrapped_mint_authority_with_seed(wrapped_mint).0
}

pub(crate) fn get_pair_config_seeds(wrapped_mint: &Pubkey) -> [&[u8]; 2] {
    [PAIR_CONFIG_SEED, wrapped_mint.as_ref()]
}

pub(crate) fn get_pair_config_signer_seeds<'a>(
    wrapped_mint: &'a Pubkey,
    bump_seed: &'a [u8],
) -> [&'a [u8]; 3] {
    [PAIR_CONFIG_SEED, wrapped_mint.as_ref(), bump_seed]
}

pub(crate) fn get_pair_config_address_with_seed(wrapped_mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&get_pair_config_seeds(wrapped_mint), &id())
}

/// Derive the `PairConfig` PDA that stores the pair's underlying mints and the
/// registry of LP mints that have been wrapped into it.
pub fn get_pair_config_address(wrapped_mint: &Pubkey) -> Pubkey {
    get_pair_config_address_with_seed(wrapped_mint).0
}

/// Derive the escrow associated-token-account that holds a given LP mint for a
/// pair. The escrow is an ATA owned by the wrapped mint authority.
pub fn get_escrow_address(
    wrapped_mint_authority: &Pubkey,
    lp_mint: &Pubkey,
    lp_token_program_id: &Pubkey,
) -> Pubkey {
    get_associated_token_address_with_program_id(wrapped_mint_authority, lp_mint, lp_token_program_id)
}

/// Convert a raw token amount expressed in `token_decimals` into a common
/// decimal scale (`common_decimals`).
///
/// This is what makes the vault decimal-aware: LP tokens from different AMMs can
/// have different decimals, and the wrapped share mint has its own decimals.
/// All reserve totals and the share supply are reasoned about in the common
/// scale so they can be summed and compared coherently. Scaling *down* (when
/// `token_decimals > common_decimals`) floors, which can lose precision; callers
/// should pick a `common_decimals` at least as large as any input LP's decimals.
pub fn to_common_units(amount: u64, token_decimals: u8, common_decimals: u8) -> Option<u128> {
    let amount = amount as u128;
    if common_decimals >= token_decimals {
        let factor = 10u128.checked_pow((common_decimals - token_decimals) as u32)?;
        amount.checked_mul(factor)
    } else {
        let factor = 10u128.checked_pow((token_decimals - common_decimals) as u32)?;
        Some(amount / factor)
    }
}

/// Convert an amount expressed in the common decimal scale back into a token's
/// own `token_decimals`. Inverse of [`to_common_units`]. Returns `None` on
/// overflow of the target `u64`.
pub fn from_common_units(amount: u128, token_decimals: u8, common_decimals: u8) -> Option<u64> {
    let scaled = if token_decimals >= common_decimals {
        let factor = 10u128.checked_pow((token_decimals - common_decimals) as u32)?;
        amount.checked_mul(factor)?
    } else {
        let factor = 10u128.checked_pow((common_decimals - token_decimals) as u32)?;
        amount / factor
    };
    u64::try_from(scaled).ok()
}

/// Compute how many wrapped shares to mint for a deposit, where `deposit_common`
/// is the deposit already normalized to the common (share-decimal) scale,
/// `reserves_common` is the vault's total reserves in the common scale before
/// this deposit, and `supply` is the current share supply.
///
/// Bootstrap (empty vault) mints the normalized deposit directly. Otherwise
/// `deposit_common * supply / reserves_common`, floored. Returns `None` on
/// overflow.
pub fn shares_on_deposit(deposit_common: u128, supply: u64, reserves_common: u128) -> Option<u64> {
    if supply == 0 || reserves_common == 0 {
        return u64::try_from(deposit_common).ok();
    }
    let shares = deposit_common
        .checked_mul(supply as u128)?
        .checked_div(reserves_common)?;
    u64::try_from(shares).ok()
}

/// Compute how many LP tokens (in the common scale) to release for a burn of
/// `shares` shares from a vault holding `reserves_common` total reserves and
/// `supply` shares. The caller is responsible for denormalizing the result into
/// the specific LP mint's decimals via [`from_common_units`].
///
/// `shares * reserves_common / supply`, floored. Returns `None` on overflow or
/// when `supply` is zero.
pub fn assets_on_redeem_common(shares: u64, supply: u64, reserves_common: u128) -> Option<u128> {
    if supply == 0 {
        return None;
    }
    (shares as u128)
        .checked_mul(reserves_common)?
        .checked_div(supply as u128)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_is_one_to_one() {
        assert_eq!(shares_on_deposit(1_000, 0, 0), Some(1_000));
        // first deposit even if a phantom reserve exists but no supply
        assert_eq!(shares_on_deposit(1_000, 0, 500), Some(1_000));
    }

    #[test]
    fn proportional_mint_and_redeem_round_trip() {
        // vault: 1000 reserves, 1000 shares -> 1:1
        assert_eq!(shares_on_deposit(500, 1_000, 1_000), Some(500));
        assert_eq!(assets_on_redeem_common(500, 1_500, 1_500), Some(500));
    }

    #[test]
    fn donation_increases_share_value() {
        // 1000 shares, someone donates so reserves become 2000.
        // redeeming all 1000 shares now returns 2000 LP.
        assert_eq!(assets_on_redeem_common(1_000, 1_000, 2_000), Some(2_000));
        // a new depositor of 100 LP into the now-richer vault gets fewer shares
        assert_eq!(shares_on_deposit(100, 1_000, 2_000), Some(50));
    }

    #[test]
    fn external_supply_burn_increases_share_value() {
        // reserves 1000, but supply was externally burned down to 500.
        // each remaining share now redeems 2 LP.
        assert_eq!(assets_on_redeem_common(250, 500, 1_000), Some(500));
    }

    #[test]
    fn decimal_normalization_round_trips() {
        // 6-decimal LP token, 9-decimal share scale: 1.0 LP == 1_000_000 raw.
        let common = to_common_units(1_000_000, 6, 9).unwrap();
        assert_eq!(common, 1_000_000_000);
        assert_eq!(from_common_units(common, 6, 9), Some(1_000_000));
    }

    #[test]
    fn decimal_aware_cross_pool_sum() {
        // Pool X LP has 6 decimals, pool Y LP has 9 decimals. One "whole" unit
        // of each must contribute equally to reserves at common scale 9.
        let x = to_common_units(1_000_000, 6, 9).unwrap(); // 1.0 in 6-dec
        let y = to_common_units(1_000_000_000, 9, 9).unwrap(); // 1.0 in 9-dec
        assert_eq!(x, y);
        let reserves = x + y; // == 2.0 at common scale
        // depositing another 1.0 of the 6-dec token into a 1:1-priced vault
        // (supply == reserves) yields 1.0 of shares at common scale.
        let deposit = to_common_units(1_000_000, 6, 9).unwrap();
        let supply = u64::try_from(reserves).unwrap();
        assert_eq!(shares_on_deposit(deposit, supply, reserves), Some(1_000_000_000));
    }

    #[test]
    fn sort_pair_is_canonical() {
        let a = Pubkey::new_from_array([1; 32]);
        let b = Pubkey::new_from_array([2; 32]);
        assert_eq!(sort_pair(&a, &b), sort_pair(&b, &a));
        assert_eq!(sort_pair(&b, &a), (a, b));
    }

    #[test]
    fn wrapped_mint_is_pair_order_independent() {
        let a = Pubkey::new_from_array([7; 32]);
        let b = Pubkey::new_from_array([9; 32]);
        let tp = spl_token_2022::id();
        assert_eq!(
            get_wrapped_mint_address(&a, &b, &tp),
            get_wrapped_mint_address(&b, &a, &tp)
        );
    }
}
