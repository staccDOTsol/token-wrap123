#![allow(unexpected_cfgs)]
pub mod accounts;
pub mod instruction;
pub mod processor;
pub mod state;
pub mod tokens;

pub const ID: pinocchio::Address =
    pinocchio::Address::from_str_const("4mEQkdKdjZS7q4963gduWRVqtKhkqpWuAr6oh2GUeB35");
#[cfg(not(feature = "no-entrypoint"))]
pinocchio::entrypoint!(processor::process_instruction, 32);

#[repr(u32)]
#[derive(Debug, Clone, Copy)]
pub enum Error {
    InvalidInstruction = 6000,
    InvalidAccount,
    Unauthorized,
    InvalidPda,
    UnsupportedMint,
    InvalidVault,
    InvalidPolicy,
    AlreadyInitialized,
    Slippage,
    Accounting,
    PendingPolicy,
    TooManySources,
}
impl From<Error> for pinocchio::error::ProgramError {
    fn from(value: Error) -> Self {
        Self::Custom(value as u32)
    }
}
pub type Result<T> = core::result::Result<T, pinocchio::error::ProgramError>;
pub fn require(condition: bool, error: Error) -> pinocchio::ProgramResult {
    if condition {
        Ok(())
    } else {
        Err(error.into())
    }
}
pub fn math<T>(result: core::result::Result<T, we_all_eat_math::Error>) -> Result<T> {
    result.map_err(|e| match e {
        we_all_eat_math::Error::Slippage => Error::Slippage.into(),
        _ => Error::Accounting.into(),
    })
}
