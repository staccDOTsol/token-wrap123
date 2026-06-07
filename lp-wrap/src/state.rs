//! Program state

use {
    crate::{error::LpWrapError, MAX_LP_MINTS},
    bytemuck::{Pod, Zeroable},
    solana_program_error::ProgramError,
    solana_pubkey::Pubkey,
};

/// On-chain configuration for a wrapped pair.
///
/// Stored at the `PairConfig` PDA derived from the wrapped mint. It records the
/// pair's two underlying mints (so unwrap can re-derive and re-verify without
/// trusting the caller), the share mint's decimals, and a registry of every LP
/// mint that has been wrapped into this pair together with each LP mint's own
/// decimals.
///
/// The registry is what makes reserve accounting both *safe* and
/// *decimal-aware*: only LP mints that passed AMM verification at wrap time are
/// ever counted as reserves (so junk-token donations can't inflate share
/// value), and each entry carries its decimals so balances of LP tokens with
/// different decimals can be normalized to a common scale before being summed.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Pod, Zeroable)]
pub struct PairConfig {
    /// First underlying mint (canonical sorted order).
    pub mint_a: Pubkey,
    /// Second underlying mint (canonical sorted order).
    pub mint_b: Pubkey,
    /// Token program of the wrapped share mint.
    pub wrapped_token_program: Pubkey,
    /// Number of populated entries in `lp_mints` / `lp_decimals`.
    pub lp_mint_count: u64,
    /// Registry of LP mints wrapped into this pair (first `lp_mint_count` valid).
    pub lp_mints: [Pubkey; MAX_LP_MINTS],
    /// Decimals of each registered LP mint, index-aligned with `lp_mints`.
    pub lp_decimals: [u8; MAX_LP_MINTS],
    /// Decimals of the wrapped share mint; also the common normalization scale.
    pub share_decimals: u8,
    /// Explicit padding so the struct has no implicit padding (needed for Pod).
    pub _padding: [u8; 7],
}

impl PairConfig {
    /// Serialized length in bytes.
    pub const LEN: usize = core::mem::size_of::<Self>();

    /// Number of registered LP mints.
    pub fn count(&self) -> usize {
        (self.lp_mint_count as usize).min(MAX_LP_MINTS)
    }

    /// The registered LP mints as a slice.
    pub fn registered(&self) -> &[Pubkey] {
        &self.lp_mints[..self.count()]
    }

    /// Whether `lp_mint` is already registered.
    pub fn contains(&self, lp_mint: &Pubkey) -> bool {
        self.registered().contains(lp_mint)
    }

    /// Look up the stored decimals for a registered LP mint.
    pub fn decimals_of(&self, lp_mint: &Pubkey) -> Option<u8> {
        self.registered()
            .iter()
            .position(|m| m == lp_mint)
            .map(|i| self.lp_decimals[i])
    }

    /// Register a new LP mint with its decimals, returning an error if the
    /// registry is full. Idempotent: registering an already-present mint is a
    /// no-op (the originally-stored decimals are kept).
    pub fn register(&mut self, lp_mint: &Pubkey, decimals: u8) -> Result<(), ProgramError> {
        if self.contains(lp_mint) {
            return Ok(());
        }
        let idx = self.count();
        if idx >= MAX_LP_MINTS {
            return Err(LpWrapError::RegistryFull.into());
        }
        self.lp_mints[idx] = *lp_mint;
        self.lp_decimals[idx] = decimals;
        self.lp_mint_count = (idx as u64) + 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn len_has_no_padding() {
        // 3 pubkeys + u64 + 8 pubkeys + 8 decimals + 1 + 7 padding
        assert_eq!(
            PairConfig::LEN,
            32 * 3 + 8 + 32 * MAX_LP_MINTS + MAX_LP_MINTS + 1 + 7
        );
    }

    #[test]
    fn register_tracks_mint_and_decimals() {
        let mut cfg = PairConfig::zeroed();
        let a = Pubkey::new_unique();
        cfg.register(&a, 6).unwrap();
        cfg.register(&a, 9).unwrap(); // idempotent, keeps original decimals
        assert_eq!(cfg.count(), 1);
        assert!(cfg.contains(&a));
        assert_eq!(cfg.decimals_of(&a), Some(6));

        for _ in 1..MAX_LP_MINTS {
            cfg.register(&Pubkey::new_unique(), 9).unwrap();
        }
        assert_eq!(cfg.count(), MAX_LP_MINTS);
        assert_eq!(
            cfg.register(&Pubkey::new_unique(), 9).unwrap_err(),
            LpWrapError::RegistryFull.into()
        );
    }
}
