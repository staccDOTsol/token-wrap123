//! Error types

use {
    num_derive::FromPrimitive,
    num_traits::FromPrimitive,
    solana_msg::msg,
    solana_program_error::{ProgramError, ToStr},
    std::convert::TryFrom,
    thiserror::Error,
};

/// Errors that may be returned by the LP Wrap program.
#[derive(Clone, Debug, Eq, Error, PartialEq, FromPrimitive)]
pub enum LpWrapError {
    // 0
    /// Wrapped mint account address does not match expected PDA
    #[error("Wrapped mint account address does not match expected PDA")]
    WrappedMintMismatch,
    /// Wrapped mint authority does not match expected PDA
    #[error("Wrapped mint authority does not match expected PDA")]
    MintAuthorityMismatch,
    /// Pair config account address does not match expected PDA
    #[error("Pair config account address does not match expected PDA")]
    PairConfigMismatch,
    /// Escrow account address does not match expected ATA
    #[error("Escrow account address does not match expected ATA")]
    EscrowMismatch,
    /// Escrow token account owner is not the expected mint authority PDA
    #[error("Escrow token account owner is not the expected mint authority PDA")]
    EscrowOwnerMismatch,

    // 5
    /// The pool account is owned by an AMM this program does not support
    #[error("The pool account is owned by an unsupported AMM")]
    UnsupportedAmm,
    /// The pool account data is smaller than the expected layout
    #[error("The pool account data is smaller than the expected layout")]
    PoolTooSmall,
    /// The supplied LP mint does not match the pool's LP mint
    #[error("The supplied LP mint does not match the pool's LP mint")]
    LpMintMismatch,
    /// The pool's two mints do not match this wrapped token's pair
    #[error("The pool's two mints do not match this wrapped token's pair")]
    PairMismatch,
    /// Amount must be positive
    #[error("Amount must be positive")]
    ZeroAmount,

    // 10
    /// Arithmetic overflow while computing shares or assets
    #[error("Arithmetic overflow while computing shares or assets")]
    Overflow,
    /// The LP mint registry for this pair is full
    #[error("The LP mint registry for this pair is full")]
    RegistryFull,
    /// The set of escrow accounts provided does not match the registry
    #[error("The set of escrow accounts provided does not match the registry")]
    EscrowSetMismatch,
    /// Not enough of the requested LP in its escrow to satisfy the redemption
    #[error("Not enough of the requested LP in its escrow to satisfy the redemption")]
    InsufficientEscrowLiquidity,
    /// The wrapped mint must be an SPL Token-2022 mint
    #[error("The wrapped mint must be an SPL Token-2022 mint")]
    WrappedMintNotToken2022,

    // 15
    /// The deposit produced zero shares
    #[error("The deposit produced zero shares")]
    ZeroSharesMinted,
    /// Creator fee account is not the expected share ATA
    #[error("Creator fee account is not the expected share ATA")]
    CreatorAccountMismatch,
    /// Deployer fee account is not the expected share ATA
    #[error("Deployer fee account is not the expected share ATA")]
    DeployerAccountMismatch,
    /// LP token program must be SPL Token or Token-2022 and own the LP mint
    #[error("LP token program must be SPL Token or Token-2022 and own the LP mint")]
    InvalidLpTokenProgram,
    /// A wrapped pair may only ever hold one LP mint (no cross-LP mixing)
    #[error("A wrapped pair may only hold one LP mint")]
    OneLpPerPair,
}

impl From<LpWrapError> for ProgramError {
    fn from(e: LpWrapError) -> Self {
        ProgramError::Custom(e as u32)
    }
}

impl TryFrom<u32> for LpWrapError {
    type Error = ProgramError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        LpWrapError::from_u32(value).ok_or(ProgramError::InvalidArgument)
    }
}

impl ToStr for LpWrapError {
    fn to_str(&self) -> &'static str {
        match self {
            LpWrapError::WrappedMintMismatch => "Error: WrappedMintMismatch",
            LpWrapError::MintAuthorityMismatch => "Error: MintAuthorityMismatch",
            LpWrapError::PairConfigMismatch => "Error: PairConfigMismatch",
            LpWrapError::EscrowMismatch => "Error: EscrowMismatch",
            LpWrapError::EscrowOwnerMismatch => "Error: EscrowOwnerMismatch",
            LpWrapError::UnsupportedAmm => "Error: UnsupportedAmm",
            LpWrapError::PoolTooSmall => "Error: PoolTooSmall",
            LpWrapError::LpMintMismatch => "Error: LpMintMismatch",
            LpWrapError::PairMismatch => "Error: PairMismatch",
            LpWrapError::ZeroAmount => "Error: ZeroAmount",
            LpWrapError::Overflow => "Error: Overflow",
            LpWrapError::RegistryFull => "Error: RegistryFull",
            LpWrapError::EscrowSetMismatch => "Error: EscrowSetMismatch",
            LpWrapError::InsufficientEscrowLiquidity => "Error: InsufficientEscrowLiquidity",
            LpWrapError::WrappedMintNotToken2022 => "Error: WrappedMintNotToken2022",
            LpWrapError::ZeroSharesMinted => "Error: ZeroSharesMinted",
            LpWrapError::CreatorAccountMismatch => "Error: CreatorAccountMismatch",
            LpWrapError::DeployerAccountMismatch => "Error: DeployerAccountMismatch",
            LpWrapError::InvalidLpTokenProgram => "Error: InvalidLpTokenProgram",
            LpWrapError::OneLpPerPair => "Error: OneLpPerPair",
        }
    }
}

/// Logs program errors
pub fn log_error(err: &ProgramError) {
    msg!(err.to_str::<LpWrapError>());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_are_stable() {
        assert_eq!(LpWrapError::WrappedMintMismatch as u32, 0);
        assert_eq!(LpWrapError::UnsupportedAmm as u32, 5);
        assert_eq!(LpWrapError::Overflow as u32, 10);
        assert_eq!(LpWrapError::ZeroSharesMinted as u32, 15);
    }
}
