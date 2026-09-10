use crate::{
    accounts::{self, program, signer},
    instruction::{WaeInstruction as I, PREFIX},
    math, require,
    state::{Addresses, Fees, Recipe, VERSION},
    tokens::{self, T22},
    Error, Result, ID,
};
use borsh::BorshDeserialize;
use pinocchio::{
    cpi::Seed,
    sysvars::{clock::Clock, Sysvar},
    AccountView, Address, ProgramResult,
};
use spl_token_2022_interface::{
    extension::{transfer_fee::instruction as fee_ix, ExtensionType},
    instruction as token_ix,
    state::Mint,
};
use we_all_eat_math::State;

pub fn process_instruction(id: &Address, a: &mut [AccountView], data: &[u8]) -> ProgramResult {
    require(
        id == &ID && data.starts_with(PREFIX),
        Error::InvalidInstruction,
    )?;
    let ix = I::try_from_slice(&data[8..]).map_err(|_| Error::InvalidInstruction)?;
    match ix {
        I::Create {
            nonce,
            fees,
            amount,
            min_cooked,
        } => create(id, a, nonce, fees, amount, min_cooked),
        I::Cook { amount, min_cooked } => trade(id, a, amount, min_cooked, false),
        I::Uncook {
            shares,
            min_raw_received,
        } => trade(id, a, shares, min_raw_received, true),
        I::Harvest => harvest(id, a),
        I::ScheduleFees { fees } => schedule(id, a, fees),
    }
}
fn count(a: &[AccountView], expected: usize) -> ProgramResult {
    require(a.len() == expected, Error::InvalidAccount)
}
fn epoch() -> Result<u64> {
    Ok(Clock::get()?.epoch)
}
fn seeds<'a>(r: &'a Recipe, bump: &'a [u8; 1]) -> [Seed<'a>; 4] {
    [
        Seed::from(b"recipe"),
        Seed::from(&r.creator),
        Seed::from(&r.nonce),
        Seed::from(bump),
    ]
}
fn same_after(
    r: &Recipe,
    reserve: &AccountView,
    cooked: &AccountView,
    state: State,
) -> ProgramResult {
    let raw_program = Address::new_from_array(r.raw_program);
    let balance = tokens::account(reserve, &r.raw_mint, None, &raw_program)?.amount;
    require(
        balance == state.reserve && tokens::mint(cooked)?.supply == state.supply,
        Error::Accounting,
    )
}
fn create(
    id: &Address,
    a: &mut [AccountView],
    nonce: [u8; 32],
    fees: Fees,
    amount: u64,
    min_cooked: u64,
) -> ProgramResult {
    count(a, 16)?;
    fees.validate()?;
    signer(&a[0])?;
    program(&a[12], a[3].owner())?;
    program(&a[13], &T22)?;
    program(&a[14], &pinocchio_system::ID)?;
    program(&a[15], &spl_associated_token_account_interface::program::ID)?;
    let raw = tokens::mint(&a[3])?;
    tokens::mint(&a[11])?;
    tokens::account(
        &a[4],
        &a[3].address().to_bytes(),
        Some(&a[0].address().to_bytes()),
        a[3].owner(),
    )?;
    let bump = accounts::pda(&a[1], id, &[b"recipe", a[0].address().as_ref(), &nonce])?;
    let addresses = Addresses::for_recipe(id, a[1].address());
    require(a[2].address() == &addresses.cooked, Error::InvalidPda)?;
    let r = Recipe {
        version: VERSION,
        bump,
        creator: a[0].address().to_bytes(),
        nonce,
        raw_mint: a[3].address().to_bytes(),
        raw_program: a[3].owner().to_bytes(),
        cooked_mint: addresses.cooked.to_bytes(),
        reward_mint: a[11].address().to_bytes(),
        decimals: raw.decimals,
        locked_shares: 10u64.pow(u32::from(raw.decimals.min(3))),
        current_fees: fees,
        next_fees: fees,
        effective_epoch: 0,
    };
    let bump = [bump];
    let sign = seeds(&r, &bump);
    accounts::create(&a[0], &a[1], id, Recipe::SPACE, &sign)?;
    let mint_size =
        ExtensionType::try_calculate_account_len::<Mint>(&[ExtensionType::TransferFeeConfig])?;
    accounts::create_derived(&a[0], &a[2], &T22, mint_size, b"cooked", a[1].address(), id)?;
    tokens::invoke(
        fee_ix::initialize_transfer_fee_config(
            &T22,
            a[2].address(),
            Some(a[1].address()),
            Some(a[1].address()),
            fees.transfer_bps,
            u64::MAX,
        )?,
        &[&a[2]],
        &[],
    )?;
    tokens::invoke(
        token_ix::initialize_mint2(&T22, a[2].address(), a[1].address(), None, raw.decimals)?,
        &[&a[2]],
        &[],
    )?;
    tokens::create_vault(&a[0], &a[5], &a[3], a[1].address(), b"reserve", id)?;
    for (index, tag) in [
        (6, b"locked".as_slice()),
        (7, b"lp"),
        (8, b"bids"),
        (9, b"collector"),
    ] {
        tokens::create_vault(&a[0], &a[index], &a[2], a[1].address(), tag, id)?;
    }
    let ata=spl_associated_token_account_interface::address::get_associated_token_address_with_program_id(a[0].address(),a[2].address(),&T22);
    require(a[10].address() == &ata, Error::InvalidVault)?;
    tokens::invoke(spl_associated_token_account_interface::instruction::create_associated_token_account_idempotent(a[0].address(),a[0].address(),a[2].address(),&T22),&[&a[0],&a[10],&a[0],&a[2],&a[14],&a[13]],&[])?;
    // Creation and seed deposit are atomic. No initialized, unfunded recipe is exposed.
    let before = State {
        reserve: 0,
        supply: 0,
        locked_shares: r.locked_shares,
    };
    tokens::transfer(&a[4], &a[3], &a[5], &a[0], amount, &[])?;
    let received = tokens::account(
        &a[5],
        &r.raw_mint,
        Some(&a[1].address().to_bytes()),
        a[3].owner(),
    )?
    .amount;
    let q = math(we_all_eat_math::deposit(
        before,
        received,
        min_cooked,
        fees.math(),
    ))?;
    tokens::issue(&a[2], &a[10], &a[1], q.user_shares, r.decimals, &sign)?;
    tokens::issue(
        &a[2],
        &a[6],
        &a[1],
        q.newly_locked_shares,
        r.decimals,
        &sign,
    )?;
    tokens::issue(&a[2], &a[7], &a[1], q.fees.lp, r.decimals, &sign)?;
    tokens::issue(&a[2], &a[8], &a[1], q.fees.bids, r.decimals, &sign)?;
    same_after(&r, &a[5], &a[2], q.after)?;
    r.save(&mut a[1])
}
// Keep the validated account roles explicit at this trust boundary.
#[allow(clippy::too_many_arguments)]
fn state(
    r: &Recipe,
    recipe: &AccountView,
    raw: &AccountView,
    cooked: &AccountView,
    reserve: &AccountView,
    locked: &AccountView,
    lp: &AccountView,
    bids: &AccountView,
    id: &Address,
    epoch: u64,
) -> Result<State> {
    let k = recipe.address().to_bytes();
    let addresses = Addresses::for_recipe(id, recipe.address());
    require(
        raw.address().to_bytes() == r.raw_mint && raw.owner().to_bytes() == r.raw_program,
        Error::InvalidAccount,
    )?;
    require(
        tokens::mint(raw)?.decimals == r.decimals,
        Error::InvalidAccount,
    )?;
    let m = tokens::cooked(cooked, recipe, r, epoch)?;
    require(cooked.address() == &addresses.cooked, Error::InvalidPda)?;
    let reserve = tokens::vault(reserve, &addresses.reserve, &r.raw_mint, &k, raw.owner())?;
    let locked = tokens::vault(locked, &addresses.locked, &r.cooked_mint, &k, &T22)?;
    require(
        locked >= r.locked_shares && r.locked_shares > 0 && m.supply >= locked,
        Error::Accounting,
    )?;
    tokens::vault(lp, &addresses.lp, &r.cooked_mint, &k, &T22)?;
    tokens::vault(bids, &addresses.bids, &r.cooked_mint, &k, &T22)?;
    Ok(State {
        reserve,
        supply: m.supply,
        locked_shares: r.locked_shares,
    })
}
fn trade(
    id: &Address,
    a: &[AccountView],
    amount: u64,
    minimum: u64,
    redeem: bool,
) -> ProgramResult {
    count(a, 12)?;
    signer(&a[0])?;
    let r = Recipe::load(&a[1], id)?;
    let epoch = epoch()?;
    program(&a[10], &Address::new_from_array(r.raw_program))?;
    program(&a[11], &T22)?;
    let before = state(
        &r, &a[1], &a[3], &a[2], &a[5], &a[7], &a[8], &a[9], id, epoch,
    )?;
    let owner = a[0].address().to_bytes();
    require(owner != a[1].address().to_bytes(), Error::Unauthorized)?;
    let user_raw = tokens::account(&a[4], &r.raw_mint, Some(&owner), a[3].owner())?.amount;
    tokens::account(&a[6], &r.cooked_mint, Some(&owner), &T22)?;
    let bump = [r.bump];
    let sign = seeds(&r, &bump);
    let policy = r.fees(epoch).math();
    if redeem {
        let q = math(we_all_eat_math::redeem(before, amount, minimum, policy))?;
        tokens::burn(&a[2], &a[6], &a[0], amount, r.decimals, &[])?;
        tokens::issue(&a[2], &a[8], &a[1], q.fees.lp, r.decimals, &sign)?;
        tokens::issue(&a[2], &a[9], &a[1], q.fees.bids, r.decimals, &sign)?;
        tokens::transfer(&a[5], &a[3], &a[4], &a[1], q.underlying_debit, &sign)?;
        let net = tokens::account(&a[4], &r.raw_mint, Some(&owner), a[3].owner())?
            .amount
            .checked_sub(user_raw)
            .ok_or(Error::Accounting)?;
        require(net >= minimum, Error::Slippage)?;
        same_after(&r, &a[5], &a[2], q.after)
    } else {
        tokens::transfer(&a[4], &a[3], &a[5], &a[0], amount, &[])?;
        let actual = tokens::account(&a[5], &r.raw_mint, None, a[3].owner())?.amount;
        let received = actual
            .checked_sub(before.reserve)
            .ok_or(Error::Accounting)?;
        let q = math(we_all_eat_math::deposit(before, received, minimum, policy))?;
        require(q.newly_locked_shares == 0, Error::Accounting)?;
        tokens::issue(&a[2], &a[6], &a[1], q.user_shares, r.decimals, &sign)?;
        tokens::issue(&a[2], &a[8], &a[1], q.fees.lp, r.decimals, &sign)?;
        tokens::issue(&a[2], &a[9], &a[1], q.fees.bids, r.decimals, &sign)?;
        same_after(&r, &a[5], &a[2], q.after)
    }
}
fn harvest(id: &Address, a: &[AccountView]) -> ProgramResult {
    require(a.len() >= 9 && a.len() <= 25, Error::TooManySources)?;
    let r = Recipe::load(&a[0], id)?;
    let epoch = epoch()?;
    program(&a[8], &T22)?;
    let before = state(
        &r, &a[0], &a[2], &a[1], &a[3], &a[4], &a[5], &a[6], id, epoch,
    )?;
    let addresses = Addresses::for_recipe(id, a[0].address());
    tokens::vault(
        &a[7],
        &addresses.collector,
        &r.cooked_mint,
        &a[0].address().to_bytes(),
        &T22,
    )?;
    let bump = [r.bump];
    let sign = seeds(&r, &bump);
    for source in &a[9..] {
        tokens::account(source, &r.cooked_mint, None, &T22)?;
        tokens::invoke(
            fee_ix::harvest_withheld_tokens_to_mint(&T22, a[1].address(), &[source.address()])?,
            &[&a[1], source],
            &[],
        )?;
    }
    tokens::invoke(
        fee_ix::withdraw_withheld_tokens_from_mint(
            &T22,
            a[1].address(),
            a[7].address(),
            a[0].address(),
            &[],
        )?,
        &[&a[1], &a[7], &a[0]],
        &sign,
    )?;
    let collected = tokens::account(&a[7], &r.cooked_mint, None, &T22)?.amount;
    let (after, split) = math(we_all_eat_math::harvest(
        before,
        collected,
        r.fees(epoch).math(),
    ))?;
    // Burn/reissue the retained allocations atomically instead of taxing an
    // internal transfer again. Net supply reduction is exactly split.burn.
    tokens::burn(&a[1], &a[7], &a[0], collected, r.decimals, &sign)?;
    tokens::issue(&a[1], &a[5], &a[0], split.lp, r.decimals, &sign)?;
    tokens::issue(&a[1], &a[6], &a[0], split.bids, r.decimals, &sign)?;
    require(
        tokens::account(&a[7], &r.cooked_mint, None, &T22)?.amount == 0,
        Error::Accounting,
    )?;
    same_after(&r, &a[3], &a[1], after)
}
fn schedule(id: &Address, a: &mut [AccountView], fees: Fees) -> ProgramResult {
    count(a, 4)?;
    fees.validate()?;
    signer(&a[0])?;
    program(&a[3], &T22)?;
    let mut r = Recipe::load(&a[1], id)?;
    let epoch = epoch()?;
    require(a[0].address().to_bytes() == r.creator, Error::Unauthorized)?;
    require(epoch >= r.effective_epoch, Error::PendingPolicy)?;
    tokens::cooked(&a[2], &a[1], &r, epoch)?;
    let bump = [r.bump];
    let sign = seeds(&r, &bump);
    tokens::invoke(
        fee_ix::set_transfer_fee(
            &T22,
            a[2].address(),
            a[1].address(),
            &[],
            fees.transfer_bps,
            u64::MAX,
        )?,
        &[&a[2], &a[1]],
        &sign,
    )?;
    r.current_fees = r.fees(epoch);
    r.next_fees = fees;
    r.effective_epoch = epoch.checked_add(2).ok_or(Error::Accounting)?;
    r.save(&mut a[1])
}
