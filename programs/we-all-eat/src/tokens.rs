use crate::{
    accounts, require,
    state::{Key, Recipe},
    Error, Result,
};
use pinocchio::{
    cpi::{invoke_signed_with_slice, Seed, Signer},
    instruction::{InstructionAccount, InstructionView},
    AccountView, Address, ProgramResult,
};
use spl_token_2022_interface::{
    extension::{
        transfer_fee::TransferFeeConfig, BaseStateWithExtensions, ExtensionType,
        StateWithExtensions,
    },
    instruction as ix,
    state::{Account, AccountState, Mint},
};
pub const T22: Address = Address::from_str_const("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");

pub fn mint(a: &AccountView) -> Result<Mint> {
    require(
        a.owned_by(&T22) || a.owned_by(&pinocchio_token::ID),
        Error::UnsupportedMint,
    )?;
    require(
        !a.executable() && (a.owner() != &pinocchio_token::ID || a.data_len() == 82),
        Error::UnsupportedMint,
    )?;
    let data = a.try_borrow()?;
    let m = StateWithExtensions::<Mint>::unpack(&data)?;
    // No callbacks, delegates that can debit backing, pause gates or confidential
    // accounting. Metadata and group membership do not change raw-unit transfers.
    for e in m.get_extension_types()? {
        require(
            matches!(
                e,
                ExtensionType::TransferFeeConfig
                    | ExtensionType::MetadataPointer
                    | ExtensionType::TokenMetadata
                    | ExtensionType::GroupPointer
                    | ExtensionType::TokenGroup
                    | ExtensionType::GroupMemberPointer
                    | ExtensionType::TokenGroupMember
            ),
            Error::UnsupportedMint,
        )?;
    }
    Ok(m.base)
}
pub fn cooked(a: &AccountView, recipe: &AccountView, r: &Recipe, epoch: u64) -> Result<Mint> {
    require(
        a.owned_by(&T22) && a.address().to_bytes() == r.cooked_mint,
        Error::InvalidAccount,
    )?;
    let m = mint(a)?;
    require(
        m.mint_authority == Some(*recipe.address()).into()
            && m.freeze_authority.is_none()
            && m.decimals == r.decimals,
        Error::InvalidAccount,
    )?;
    let data = a.try_borrow()?;
    let ext = StateWithExtensions::<Mint>::unpack(&data)?;
    require(
        ext.get_extension_types()? == vec![ExtensionType::TransferFeeConfig],
        Error::UnsupportedMint,
    )?;
    let fee = ext.get_extension::<TransferFeeConfig>()?;
    require(
        Option::<Address>::from(fee.withdraw_withheld_authority) == Some(*recipe.address())
            && Option::<Address>::from(fee.transfer_fee_config_authority)
                == Some(*recipe.address()),
        Error::InvalidAccount,
    )?;
    for f in [fee.older_transfer_fee, fee.newer_transfer_fee] {
        require(
            u16::from(f.transfer_fee_basis_points) > 0 && u64::from(f.maximum_fee) == u64::MAX,
            Error::InvalidPolicy,
        )?;
    }
    require(
        u16::from(fee.get_epoch_fee(epoch).transfer_fee_basis_points) == r.fees(epoch).transfer_bps,
        Error::InvalidPolicy,
    )?;
    Ok(m)
}
pub fn account(a: &AccountView, m: &Key, owner: Option<&Key>, tp: &Address) -> Result<Account> {
    require(
        a.owned_by(tp) && !a.executable() && (tp != &pinocchio_token::ID || a.data_len() == 165),
        Error::InvalidVault,
    )?;
    let d = a.try_borrow()?;
    let t = StateWithExtensions::<Account>::unpack(&d)?;
    require(
        t.base.mint.to_bytes() == *m
            && t.base.state == AccountState::Initialized
            && owner.is_none_or(|key| t.base.owner.to_bytes() == *key),
        Error::InvalidVault,
    )?;
    Ok(t.base)
}
pub fn vault(
    a: &AccountView,
    expected: &Address,
    m: &Key,
    owner: &Key,
    tp: &Address,
) -> Result<u64> {
    require(a.address() == expected, Error::InvalidPda)?;
    let t = account(a, m, Some(owner), tp)?;
    require(
        t.delegate.is_none() && t.close_authority.is_none(),
        Error::InvalidVault,
    )?;
    Ok(t.amount)
}
pub fn create_vault(
    payer: &AccountView,
    a: &AccountView,
    m: &AccountView,
    owner: &Address,
    tag: &[u8],
    id: &Address,
) -> ProgramResult {
    mint(m)?;
    let size = {
        let d = m.try_borrow()?;
        spl_token_2022_interface::extension::account_len::try_calculate_account_len_from_mint_data(
            &d,
            &[],
        )?
    };
    accounts::create_derived(payer, a, m.owner(), size, tag, owner, id)?;
    invoke(
        ix::initialize_account3(m.owner(), a.address(), m.address(), owner)?,
        &[a, m],
        &[],
    )
}
pub fn invoke(
    ix: solana_instruction::Instruction,
    accounts: &[&AccountView],
    seeds: &[Seed],
) -> ProgramResult {
    require(accounts.len() == ix.accounts.len(), Error::InvalidAccount)?;
    for (a, m) in accounts.iter().zip(&ix.accounts) {
        require(a.address() == &m.pubkey, Error::InvalidAccount)?;
    }
    let metas: Vec<_> = ix
        .accounts
        .iter()
        .map(|a| InstructionAccount::new(&a.pubkey, a.is_writable, a.is_signer))
        .collect();
    let view = InstructionView {
        program_id: &ix.program_id,
        accounts: &metas,
        data: &ix.data,
    };
    if seeds.is_empty() {
        invoke_signed_with_slice(&view, accounts, &[])
    } else {
        invoke_signed_with_slice(&view, accounts, &[Signer::from(seeds)])
    }
}
pub fn transfer(
    from: &AccountView,
    m: &AccountView,
    to: &AccountView,
    authority: &AccountView,
    amount: u64,
    seeds: &[Seed],
) -> ProgramResult {
    require(from.address() != to.address(), Error::InvalidVault)?;
    let decimals = mint(m)?.decimals;
    invoke(
        ix::transfer_checked(
            m.owner(),
            from.address(),
            m.address(),
            to.address(),
            authority.address(),
            &[],
            amount,
            decimals,
        )?,
        &[from, m, to, authority],
        seeds,
    )
}
pub fn issue(
    m: &AccountView,
    to: &AccountView,
    authority: &AccountView,
    amount: u64,
    decimals: u8,
    seeds: &[Seed],
) -> ProgramResult {
    if amount == 0 {
        return Ok(());
    }
    invoke(
        ix::mint_to_checked(
            &T22,
            m.address(),
            to.address(),
            authority.address(),
            &[],
            amount,
            decimals,
        )?,
        &[m, to, authority],
        seeds,
    )
}
pub fn burn(
    m: &AccountView,
    from: &AccountView,
    authority: &AccountView,
    amount: u64,
    decimals: u8,
    seeds: &[Seed],
) -> ProgramResult {
    if amount == 0 {
        return Ok(());
    }
    invoke(
        ix::burn_checked(
            &T22,
            from.address(),
            m.address(),
            authority.address(),
            &[],
            amount,
            decimals,
        )?,
        &[from, m, authority],
        seeds,
    )
}
