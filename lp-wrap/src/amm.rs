//! AMM pool deconstruction.
//!
//! Each supported AMM stores its pool state with a different account layout.
//! This module identifies the AMM from the pool account's owning program and
//! extracts the three fields we care about: the LP mint and the two underlying
//! token mints. All offsets are byte offsets into the raw account data and were
//! taken from each program's on-chain state definition.
//!
//! NOTE: concentrated-liquidity AMMs (Raydium CLMM, Orca Whirlpools, Meteora
//! DLMM) are intentionally unsupported: they represent positions as NFTs rather
//! than fungible LP tokens, so there is nothing fungible to wrap.

use {
    crate::error::LpWrapError,
    solana_program_error::ProgramError,
    solana_pubkey::{pubkey, Pubkey},
};

/// Raydium Liquidity Pool AMM v4 (the classic OpenBook constant-product AMM).
/// Raydium uses different program ids on devnet; the `devnet` feature selects
/// them so the program can be exercised end-to-end on devnet.
#[cfg(not(feature = "devnet"))]
pub const RAYDIUM_AMM_V4_PROGRAM_ID: Pubkey = pubkey!("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8");
/// Raydium AMM v4 program id on devnet.
#[cfg(feature = "devnet")]
pub const RAYDIUM_AMM_V4_PROGRAM_ID: Pubkey = pubkey!("HWy1jotHpo6UqeQxx49dpYYdQB8wj9Qk9MdxwjLvDHB8");
/// Raydium CPMM / CP-Swap (the newer Token-2022-aware constant-product AMM).
#[cfg(not(feature = "devnet"))]
pub const RAYDIUM_CPMM_PROGRAM_ID: Pubkey = pubkey!("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C");
/// Raydium CPMM program id on devnet.
#[cfg(feature = "devnet")]
pub const RAYDIUM_CPMM_PROGRAM_ID: Pubkey = pubkey!("CPMDWBwJDtYax9qW7AyRuVC19Cc4L4Vcy4n2BHAbHkCW");
/// PumpSwap AMM (pump.fun's constant-product AMM).
pub const PUMP_SWAP_PROGRAM_ID: Pubkey = pubkey!("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA");
/// Meteora Dynamic AMM (formerly Mercurial), constant-product with fungible LP.
pub const METEORA_DYNAMIC_AMM_PROGRAM_ID: Pubkey =
    pubkey!("Eo7WjKq67rjJQSZxS6z3YkapzY3eMj6Xy8X5EQVn5UaB");

// --- Raydium AMM v4 `AmmInfo` packed layout (no discriminator) ---
const RAYDIUM_V4_COIN_MINT_OFFSET: usize = 400;
const RAYDIUM_V4_PC_MINT_OFFSET: usize = 432;
const RAYDIUM_V4_LP_MINT_OFFSET: usize = 464;
const RAYDIUM_V4_MIN_LEN: usize = RAYDIUM_V4_LP_MINT_OFFSET + 32;

// --- Raydium CPMM `PoolState` (8-byte anchor discriminator) ---
const RAYDIUM_CPMM_LP_MINT_OFFSET: usize = 136;
const RAYDIUM_CPMM_TOKEN_0_MINT_OFFSET: usize = 168;
const RAYDIUM_CPMM_TOKEN_1_MINT_OFFSET: usize = 200;
const RAYDIUM_CPMM_MIN_LEN: usize = RAYDIUM_CPMM_TOKEN_1_MINT_OFFSET + 32;

// --- PumpSwap `Pool` (8-byte anchor discriminator) ---
// disc(8) pool_bump(1) index(2) creator(32) base_mint(32) quote_mint(32) lp_mint(32) ...
const PUMP_BASE_MINT_OFFSET: usize = 43;
const PUMP_QUOTE_MINT_OFFSET: usize = 75;
const PUMP_LP_MINT_OFFSET: usize = 107;
const PUMP_MIN_LEN: usize = PUMP_LP_MINT_OFFSET + 32;

// --- Meteora Dynamic AMM `Pool` (8-byte anchor discriminator) ---
// disc(8) lp_mint(32) token_a_mint(32) token_b_mint(32) a_vault(32) ...
const METEORA_LP_MINT_OFFSET: usize = 8;
const METEORA_TOKEN_A_MINT_OFFSET: usize = 40;
const METEORA_TOKEN_B_MINT_OFFSET: usize = 72;
const METEORA_MIN_LEN: usize = METEORA_TOKEN_B_MINT_OFFSET + 32;

/// The supported AMMs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AmmKind {
    /// Raydium AMM v4.
    RaydiumAmmV4,
    /// Raydium CPMM / CP-Swap.
    RaydiumCpmm,
    /// PumpSwap.
    PumpSwap,
    /// Meteora Dynamic AMM.
    MeteoraDynamicAmm,
}

impl AmmKind {
    /// Map a pool account's owning program to a supported AMM, if any.
    pub fn from_program_id(program_id: &Pubkey) -> Option<Self> {
        match *program_id {
            RAYDIUM_AMM_V4_PROGRAM_ID => Some(AmmKind::RaydiumAmmV4),
            RAYDIUM_CPMM_PROGRAM_ID => Some(AmmKind::RaydiumCpmm),
            PUMP_SWAP_PROGRAM_ID => Some(AmmKind::PumpSwap),
            METEORA_DYNAMIC_AMM_PROGRAM_ID => Some(AmmKind::MeteoraDynamicAmm),
            _ => None,
        }
    }
}

/// The fields extracted from an AMM pool account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolInfo {
    /// Which AMM this pool belongs to.
    pub kind: AmmKind,
    /// The pool's LP mint.
    pub lp_mint: Pubkey,
    /// The pool's first underlying mint (unsorted, as stored by the AMM).
    pub mint_a: Pubkey,
    /// The pool's second underlying mint (unsorted, as stored by the AMM).
    pub mint_b: Pubkey,
}

fn read_pubkey(data: &[u8], offset: usize) -> Result<Pubkey, ProgramError> {
    let bytes = data
        .get(offset..offset.checked_add(32).ok_or(ProgramError::InvalidAccountData)?)
        .ok_or(LpWrapError::PoolTooSmall)?;
    Ok(Pubkey::new_from_array(bytes.try_into().unwrap()))
}

/// Deconstruct a raw AMM pool account into its LP mint and two underlying
/// mints. `owner` is the account's owning program id, used to pick the layout.
pub fn parse_pool(owner: &Pubkey, data: &[u8]) -> Result<PoolInfo, ProgramError> {
    let kind = AmmKind::from_program_id(owner).ok_or(LpWrapError::UnsupportedAmm)?;
    let (lp_off, a_off, b_off, min_len) = match kind {
        AmmKind::RaydiumAmmV4 => (
            RAYDIUM_V4_LP_MINT_OFFSET,
            RAYDIUM_V4_COIN_MINT_OFFSET,
            RAYDIUM_V4_PC_MINT_OFFSET,
            RAYDIUM_V4_MIN_LEN,
        ),
        AmmKind::RaydiumCpmm => (
            RAYDIUM_CPMM_LP_MINT_OFFSET,
            RAYDIUM_CPMM_TOKEN_0_MINT_OFFSET,
            RAYDIUM_CPMM_TOKEN_1_MINT_OFFSET,
            RAYDIUM_CPMM_MIN_LEN,
        ),
        AmmKind::PumpSwap => (
            PUMP_LP_MINT_OFFSET,
            PUMP_BASE_MINT_OFFSET,
            PUMP_QUOTE_MINT_OFFSET,
            PUMP_MIN_LEN,
        ),
        AmmKind::MeteoraDynamicAmm => (
            METEORA_LP_MINT_OFFSET,
            METEORA_TOKEN_A_MINT_OFFSET,
            METEORA_TOKEN_B_MINT_OFFSET,
            METEORA_MIN_LEN,
        ),
    };
    if data.len() < min_len {
        return Err(LpWrapError::PoolTooSmall.into());
    }
    Ok(PoolInfo {
        kind,
        lp_mint: read_pubkey(data, lp_off)?,
        mint_a: read_pubkey(data, a_off)?,
        mint_b: read_pubkey(data, b_off)?,
    })
}

impl PoolInfo {
    /// Verify that this pool matches the supplied LP mint and underlying pair.
    ///
    /// * the supplied LP mint must exactly equal the pool's LP mint, and
    /// * the pool's two mints, sorted, must exactly equal the sorted pair.
    pub fn verify(
        &self,
        supplied_lp_mint: &Pubkey,
        pair_mint_a: &Pubkey,
        pair_mint_b: &Pubkey,
    ) -> Result<(), ProgramError> {
        if self.lp_mint != *supplied_lp_mint {
            return Err(LpWrapError::LpMintMismatch.into());
        }
        let pool_pair = crate::sort_pair(&self.mint_a, &self.mint_b);
        let expected_pair = crate::sort_pair(pair_mint_a, pair_mint_b);
        if pool_pair != expected_pair {
            return Err(LpWrapError::PairMismatch.into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool_bytes(len: usize, fields: &[(usize, Pubkey)]) -> Vec<u8> {
        let mut data = vec![0u8; len];
        for (off, key) in fields {
            data[*off..*off + 32].copy_from_slice(key.as_ref());
        }
        data
    }

    #[test]
    fn parses_raydium_v4() {
        let coin = Pubkey::new_from_array([1; 32]);
        let pc = Pubkey::new_from_array([2; 32]);
        let lp = Pubkey::new_from_array([3; 32]);
        let data = pool_bytes(
            752,
            &[
                (RAYDIUM_V4_COIN_MINT_OFFSET, coin),
                (RAYDIUM_V4_PC_MINT_OFFSET, pc),
                (RAYDIUM_V4_LP_MINT_OFFSET, lp),
            ],
        );
        let info = parse_pool(&RAYDIUM_AMM_V4_PROGRAM_ID, &data).unwrap();
        assert_eq!(info.kind, AmmKind::RaydiumAmmV4);
        assert_eq!(info.lp_mint, lp);
        info.verify(&lp, &coin, &pc).unwrap();
        // order independence of the pair
        info.verify(&lp, &pc, &coin).unwrap();
    }

    #[test]
    fn parses_raydium_cpmm() {
        let t0 = Pubkey::new_from_array([4; 32]);
        let t1 = Pubkey::new_from_array([5; 32]);
        let lp = Pubkey::new_from_array([6; 32]);
        let data = pool_bytes(
            640,
            &[
                (RAYDIUM_CPMM_TOKEN_0_MINT_OFFSET, t0),
                (RAYDIUM_CPMM_TOKEN_1_MINT_OFFSET, t1),
                (RAYDIUM_CPMM_LP_MINT_OFFSET, lp),
            ],
        );
        let info = parse_pool(&RAYDIUM_CPMM_PROGRAM_ID, &data).unwrap();
        assert_eq!(info.kind, AmmKind::RaydiumCpmm);
        info.verify(&lp, &t0, &t1).unwrap();
    }

    #[test]
    fn parses_pump_swap() {
        let base = Pubkey::new_from_array([7; 32]);
        let quote = Pubkey::new_from_array([8; 32]);
        let lp = Pubkey::new_from_array([9; 32]);
        let data = pool_bytes(
            300,
            &[
                (PUMP_BASE_MINT_OFFSET, base),
                (PUMP_QUOTE_MINT_OFFSET, quote),
                (PUMP_LP_MINT_OFFSET, lp),
            ],
        );
        let info = parse_pool(&PUMP_SWAP_PROGRAM_ID, &data).unwrap();
        assert_eq!(info.kind, AmmKind::PumpSwap);
        info.verify(&lp, &base, &quote).unwrap();
    }

    #[test]
    fn parses_meteora_dynamic_amm() {
        let lp = Pubkey::new_from_array([10; 32]);
        let a = Pubkey::new_from_array([11; 32]);
        let b = Pubkey::new_from_array([12; 32]);
        let data = pool_bytes(
            300,
            &[
                (METEORA_LP_MINT_OFFSET, lp),
                (METEORA_TOKEN_A_MINT_OFFSET, a),
                (METEORA_TOKEN_B_MINT_OFFSET, b),
            ],
        );
        let info = parse_pool(&METEORA_DYNAMIC_AMM_PROGRAM_ID, &data).unwrap();
        assert_eq!(info.kind, AmmKind::MeteoraDynamicAmm);
        info.verify(&lp, &a, &b).unwrap();
    }

    #[test]
    fn rejects_unknown_program() {
        let data = vec![0u8; 800];
        let err = parse_pool(&Pubkey::new_unique(), &data).unwrap_err();
        assert_eq!(err, LpWrapError::UnsupportedAmm.into());
    }

    #[test]
    fn rejects_wrong_lp_mint() {
        let coin = Pubkey::new_from_array([1; 32]);
        let pc = Pubkey::new_from_array([2; 32]);
        let lp = Pubkey::new_from_array([3; 32]);
        let data = pool_bytes(
            752,
            &[
                (RAYDIUM_V4_COIN_MINT_OFFSET, coin),
                (RAYDIUM_V4_PC_MINT_OFFSET, pc),
                (RAYDIUM_V4_LP_MINT_OFFSET, lp),
            ],
        );
        let info = parse_pool(&RAYDIUM_AMM_V4_PROGRAM_ID, &data).unwrap();
        let wrong = Pubkey::new_from_array([99; 32]);
        assert_eq!(
            info.verify(&wrong, &coin, &pc).unwrap_err(),
            LpWrapError::LpMintMismatch.into()
        );
        assert_eq!(
            info.verify(&lp, &coin, &wrong).unwrap_err(),
            LpWrapError::PairMismatch.into()
        );
    }
}
