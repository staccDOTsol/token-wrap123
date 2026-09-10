use crate::{require, Error, Result};
use pinocchio::{
    cpi::{Seed, Signer},
    sysvars::{rent::Rent, Sysvar},
    AccountView, Address, ProgramResult,
};
use pinocchio_system::instructions::{Allocate, Assign, CreateAccount, Transfer};
pub fn signer(a: &AccountView) -> ProgramResult {
    require(a.is_signer(), Error::Unauthorized)
}
pub fn program(a: &AccountView, id: &Address) -> ProgramResult {
    require(a.address() == id && a.executable(), Error::InvalidAccount)
}
pub fn pda(a: &AccountView, id: &Address, seeds: &[&[u8]]) -> Result<u8> {
    let (k, b) = Address::find_program_address(seeds, id);
    require(a.address() == &k, Error::InvalidPda)?;
    Ok(b)
}
pub fn create(
    payer: &AccountView,
    a: &AccountView,
    owner: &Address,
    size: usize,
    seeds: &[Seed],
) -> ProgramResult {
    signer(payer)?;
    require(
        payer.is_writable() && a.is_writable(),
        Error::InvalidAccount,
    )?;
    require(
        a.owned_by(&pinocchio_system::ID) && a.data_len() == 0,
        Error::AlreadyInitialized,
    )?;
    let signers = [Signer::from(seeds)];
    let rent = Rent::get()?.try_minimum_balance(size)?;
    // Accept a PDA prefunded with lamports. Reserve addresses are program PDAs,
    // not ATAs, so outsiders cannot pre-create a token vault or donate token dust.
    if a.lamports() == 0 {
        CreateAccount {
            from: payer,
            to: a,
            lamports: rent,
            space: size as u64,
            owner,
        }
        .invoke_signed(&signers)?;
    } else {
        if a.lamports() < rent {
            Transfer {
                from: payer,
                to: a,
                lamports: rent - a.lamports(),
            }
            .invoke()?;
        }
        Allocate {
            account: a,
            space: size as u64,
        }
        .invoke_signed(&signers)?;
        Assign { account: a, owner }.invoke_signed(&signers)?;
    }
    Ok(())
}
pub fn create_derived(
    payer: &AccountView,
    a: &AccountView,
    owner: &Address,
    size: usize,
    tag: &[u8],
    recipe: &Address,
    id: &Address,
) -> ProgramResult {
    let bump = [pda(a, id, &[tag, recipe.as_ref()])?];
    create(
        payer,
        a,
        owner,
        size,
        &[
            Seed::from(tag),
            Seed::from(recipe.as_ref()),
            Seed::from(&bump),
        ],
    )
}
