//! Program state processor

use {
    crate::{
        amm::parse_pool,
        assets_on_redeem_common,
        error::LpWrapError,
        from_common_units, get_escrow_address, get_pair_config_address_with_seed,
        get_pair_config_signer_seeds, get_wrapped_mint_address, get_wrapped_mint_address_with_seed,
        get_wrapped_mint_authority_signer_seeds, get_wrapped_mint_authority_with_seed,
        get_wrapped_mint_signer_seeds, instruction::LpWrapInstruction, shares_on_deposit, sort_pair,
        state::PairConfig, to_common_units,
    },
    solana_account_info::{next_account_info, AccountInfo},
    solana_cpi::{invoke, invoke_signed},
    solana_msg::msg,
    solana_program_error::{ProgramError, ProgramResult},
    solana_pubkey::Pubkey,
    solana_rent::Rent,
    solana_system_interface::instruction::{allocate, assign},
    solana_sysvar::Sysvar,
    spl_associated_token_account_interface::address::get_associated_token_address_with_program_id,
    spl_pod::optional_keys::OptionalNonZeroPubkey,
    spl_token_2022::{
        extension::{ExtensionType, PodStateWithExtensions},
        pod::{PodAccount, PodMint},
        state::Mint,
    },
    spl_token_metadata_interface::state::TokenMetadata,
};

/// Reads `(owner, mint, amount)` from a token account.
fn read_token_account(account: &AccountInfo) -> Result<(Pubkey, Pubkey, u64), ProgramError> {
    let data = account.try_borrow_data()?;
    let state = PodStateWithExtensions::<PodAccount>::unpack(&data)?;
    Ok((
        state.base.owner,
        state.base.mint,
        u64::from(state.base.amount),
    ))
}

/// Reads the supply of a mint.
fn read_mint_supply(account: &AccountInfo) -> Result<u64, ProgramError> {
    let data = account.try_borrow_data()?;
    let state = PodStateWithExtensions::<PodMint>::unpack(&data)?;
    Ok(u64::from(state.base.supply))
}

/// Reads the decimals of a mint.
fn read_mint_decimals(account: &AccountInfo) -> Result<u8, ProgramError> {
    let data = account.try_borrow_data()?;
    let state = PodStateWithExtensions::<PodMint>::unpack(&data)?;
    Ok(state.base.decimals)
}

/// Loads and validates the `PairConfig` PDA for a wrapped mint, returning a copy.
fn load_pair_config(
    program_id: &Pubkey,
    pair_config: &AccountInfo,
    wrapped_mint: &Pubkey,
) -> Result<PairConfig, ProgramError> {
    let (expected, _) = get_pair_config_address_with_seed(wrapped_mint);
    if *pair_config.key != expected {
        return Err(LpWrapError::PairConfigMismatch.into());
    }
    if pair_config.owner != program_id {
        return Err(ProgramError::InvalidAccountOwner);
    }
    let data = pair_config.try_borrow_data()?;
    let config = bytemuck::try_from_bytes::<PairConfig>(&data)
        .map_err(|_| ProgramError::InvalidAccountData)?;
    Ok(*config)
}

/// Sum the normalized reserves contributed by the "other" registered escrows
/// (every registered escrow except the one for `current_lp_mint`), verifying
/// that the provided set exactly matches the registry and that each escrow is
/// the correct authority-owned ATA. Returns reserves in the common scale.
fn sum_other_escrows(
    others: &[AccountInfo],
    config: &PairConfig,
    current_lp_mint: &Pubkey,
    authority: &Pubkey,
) -> Result<u128, ProgramError> {
    // Expected set: every registered LP mint except the current one.
    let expected: Vec<(Pubkey, u8)> = config
        .registered()
        .iter()
        .enumerate()
        .filter(|(_, m)| *m != current_lp_mint)
        .map(|(i, m)| (*m, config.lp_decimals[i]))
        .collect();

    if others.len() != expected.len() {
        return Err(LpWrapError::EscrowSetMismatch.into());
    }

    let mut seen = vec![false; expected.len()];
    let mut reserves: u128 = 0;
    for escrow in others {
        let (owner, mint, amount) = read_token_account(escrow)?;
        if owner != *authority {
            return Err(LpWrapError::EscrowOwnerMismatch.into());
        }
        // The escrow's owning program is the LP token program; recompute the ATA.
        let expected_ata =
            get_associated_token_address_with_program_id(authority, &mint, escrow.owner);
        if *escrow.key != expected_ata {
            return Err(LpWrapError::EscrowMismatch.into());
        }
        let idx = expected
            .iter()
            .enumerate()
            .find(|(i, (m, _))| *m == mint && !seen[*i])
            .map(|(i, _)| i)
            .ok_or(LpWrapError::EscrowSetMismatch)?;
        seen[idx] = true;
        let normalized = to_common_units(amount, expected[idx].1, config.share_decimals)
            .ok_or(LpWrapError::Overflow)?;
        reserves = reserves
            .checked_add(normalized)
            .ok_or(LpWrapError::Overflow)?;
    }
    if seen.iter().any(|s| !s) {
        return Err(LpWrapError::EscrowSetMismatch.into());
    }
    Ok(reserves)
}

/// Processes the `CreatePairMint` instruction.
pub fn process_create_pair_mint(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    decimals: u8,
    name: String,
    symbol: String,
    uri: String,
) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let wrapped_mint_account = next_account_info(account_info_iter)?;
    let pair_config_account = next_account_info(account_info_iter)?;
    let wrapped_mint_authority_account = next_account_info(account_info_iter)?;
    let mint_a_account = next_account_info(account_info_iter)?;
    let mint_b_account = next_account_info(account_info_iter)?;
    let _system_program = next_account_info(account_info_iter)?;
    let wrapped_token_program = next_account_info(account_info_iter)?;

    // Metadata requires Token-2022.
    if *wrapped_token_program.key != spl_token_2022::id() {
        return Err(LpWrapError::WrappedMintNotToken2022.into());
    }

    let (mint_a, mint_b) = sort_pair(mint_a_account.key, mint_b_account.key);

    let (wrapped_mint_address, mint_bump) =
        get_wrapped_mint_address_with_seed(&mint_a, &mint_b, wrapped_token_program.key);
    if *wrapped_mint_account.key != wrapped_mint_address {
        return Err(LpWrapError::WrappedMintMismatch.into());
    }

    let (pair_config_address, config_bump) =
        get_pair_config_address_with_seed(wrapped_mint_account.key);
    if *pair_config_account.key != pair_config_address {
        return Err(LpWrapError::PairConfigMismatch.into());
    }

    let (authority, _authority_bump) = get_wrapped_mint_authority_with_seed(wrapped_mint_account.key);
    if *wrapped_mint_authority_account.key != authority {
        return Err(LpWrapError::MintAuthorityMismatch.into());
    }

    if wrapped_mint_account.data_len() > 0 || pair_config_account.data_len() > 0 {
        return Err(ProgramError::AccountAlreadyInitialized);
    }

    let rent = Rent::get()?;

    // --- Create the wrapped share mint (Token-2022 + transfer fee + metadata) ---
    let base_space = ExtensionType::try_calculate_account_len::<Mint>(&[
        ExtensionType::TransferFeeConfig,
        ExtensionType::MetadataPointer,
    ])?;

    let metadata = TokenMetadata {
        update_authority: OptionalNonZeroPubkey::try_from(Some(authority))?,
        mint: *wrapped_mint_account.key,
        name,
        symbol,
        uri,
        additional_metadata: vec![],
    };
    let metadata_space = metadata.tlv_size_of()?;
    let total_space = base_space
        .checked_add(metadata_space)
        .ok_or(LpWrapError::Overflow)?;

    let mint_rent = rent.minimum_balance(total_space);
    if wrapped_mint_account.lamports() < mint_rent {
        msg!(
            "Error: wrapped mint requires pre-funding of {} lamports",
            mint_rent
        );
        return Err(ProgramError::AccountNotRentExempt);
    }

    let mint_bump = [mint_bump];
    let mint_seeds =
        get_wrapped_mint_signer_seeds(&mint_a, &mint_b, wrapped_token_program.key, &mint_bump);

    // Allocate only the fixed (pointer) space; the metadata init reallocs the rest.
    invoke_signed(
        &allocate(wrapped_mint_account.key, base_space as u64),
        core::slice::from_ref(wrapped_mint_account),
        &[&mint_seeds],
    )?;
    invoke_signed(
        &assign(wrapped_mint_account.key, wrapped_token_program.key),
        core::slice::from_ref(wrapped_mint_account),
        &[&mint_seeds],
    )?;

    // Fixed-length extensions must be initialized before the mint itself.
    // 1 bps transfer fee, with the mint authority PDA as both the fee-config
    // and withheld-withdraw authority.
    invoke(
        &spl_token_2022::extension::transfer_fee::instruction::initialize_transfer_fee_config(
            wrapped_token_program.key,
            wrapped_mint_account.key,
            Some(&authority),
            Some(&authority),
            crate::FEE_BASIS_POINTS,
            u64::MAX,
        )?,
        core::slice::from_ref(wrapped_mint_account),
    )?;
    // Metadata pointer points the mint at itself.
    invoke(
        &spl_token_2022::extension::metadata_pointer::instruction::initialize(
            wrapped_token_program.key,
            wrapped_mint_account.key,
            Some(authority),
            Some(*wrapped_mint_account.key),
        )?,
        core::slice::from_ref(wrapped_mint_account),
    )?;
    invoke(
        &spl_token_2022::instruction::initialize_mint2(
            wrapped_token_program.key,
            wrapped_mint_account.key,
            &authority,
            None,
            decimals,
        )?,
        core::slice::from_ref(wrapped_mint_account),
    )?;

    // Initialize the embedded TokenMetadata, signed by the mint authority PDA.
    let authority_bump = [_authority_bump];
    let authority_seeds =
        get_wrapped_mint_authority_signer_seeds(wrapped_mint_account.key, &authority_bump);
    invoke_signed(
        &spl_token_metadata_interface::instruction::initialize(
            wrapped_token_program.key,
            wrapped_mint_account.key,
            wrapped_mint_authority_account.key,
            wrapped_mint_account.key,
            wrapped_mint_authority_account.key,
            metadata.name.clone(),
            metadata.symbol.clone(),
            metadata.uri.clone(),
        ),
        &[
            wrapped_mint_account.clone(),
            wrapped_mint_authority_account.clone(),
        ],
        &[&authority_seeds],
    )?;

    // --- Create and populate the PairConfig PDA ---
    let config_rent = rent.minimum_balance(PairConfig::LEN);
    if pair_config_account.lamports() < config_rent {
        msg!(
            "Error: pair config requires pre-funding of {} lamports",
            config_rent
        );
        return Err(ProgramError::AccountNotRentExempt);
    }
    let config_bump = [config_bump];
    let config_seeds = get_pair_config_signer_seeds(wrapped_mint_account.key, &config_bump);
    invoke_signed(
        &allocate(pair_config_account.key, PairConfig::LEN as u64),
        core::slice::from_ref(pair_config_account),
        &[&config_seeds],
    )?;
    invoke_signed(
        &assign(pair_config_account.key, program_id),
        core::slice::from_ref(pair_config_account),
        &[&config_seeds],
    )?;

    let mut config_data = pair_config_account.try_borrow_mut_data()?;
    let config = bytemuck::try_from_bytes_mut::<PairConfig>(&mut config_data)
        .map_err(|_| ProgramError::InvalidAccountData)?;
    config.mint_a = mint_a;
    config.mint_b = mint_b;
    config.wrapped_token_program = *wrapped_token_program.key;
    config.share_decimals = decimals;
    config.lp_mint_count = 0;

    Ok(())
}

/// Processes the `Wrap` instruction.
pub fn process_wrap(program_id: &Pubkey, accounts: &[AccountInfo], amount: u64) -> ProgramResult {
    if amount == 0 {
        return Err(LpWrapError::ZeroAmount.into());
    }
    let account_info_iter = &mut accounts.iter();
    let recipient_share = next_account_info(account_info_iter)?;
    let wrapped_mint = next_account_info(account_info_iter)?;
    let wrapped_mint_authority = next_account_info(account_info_iter)?;
    let pair_config = next_account_info(account_info_iter)?;
    let wrapped_token_program = next_account_info(account_info_iter)?;
    let lp_token_program = next_account_info(account_info_iter)?;
    let source_lp = next_account_info(account_info_iter)?;
    let lp_mint = next_account_info(account_info_iter)?;
    let lp_escrow = next_account_info(account_info_iter)?;
    let amm_pool = next_account_info(account_info_iter)?;
    let transfer_authority = next_account_info(account_info_iter)?;
    let other_escrows = account_info_iter.as_slice();

    let config = load_pair_config(program_id, pair_config, wrapped_mint.key)?;

    // Verify the wrapped mint PDA derives from the config's pair.
    let expected_mint =
        get_wrapped_mint_address(&config.mint_a, &config.mint_b, wrapped_token_program.key);
    if expected_mint != *wrapped_mint.key {
        return Err(LpWrapError::WrappedMintMismatch.into());
    }

    let (authority, authority_bump) = get_wrapped_mint_authority_with_seed(wrapped_mint.key);
    if *wrapped_mint_authority.key != authority {
        return Err(LpWrapError::MintAuthorityMismatch.into());
    }

    // Verify the deposit escrow is the right ATA.
    let expected_escrow = get_escrow_address(&authority, lp_mint.key, lp_token_program.key);
    if *lp_escrow.key != expected_escrow {
        return Err(LpWrapError::EscrowMismatch.into());
    }

    // Deconstruct the AMM pool and verify the LP mint + underlying pair.
    let pool = {
        let pool_data = amm_pool.try_borrow_data()?;
        parse_pool(amm_pool.owner, &pool_data)?
    };
    pool.verify(lp_mint.key, &config.mint_a, &config.mint_b)?;

    let lp_decimals = read_mint_decimals(lp_mint)?;

    // Total reserves (common scale) BEFORE this deposit: current escrow + others.
    let (escrow_owner, escrow_mint, current_amount) = read_token_account(lp_escrow)?;
    if escrow_owner != authority {
        return Err(LpWrapError::EscrowOwnerMismatch.into());
    }
    if escrow_mint != *lp_mint.key {
        return Err(LpWrapError::EscrowMismatch.into());
    }
    let current_common =
        to_common_units(current_amount, lp_decimals, config.share_decimals).ok_or(LpWrapError::Overflow)?;
    let other_common = sum_other_escrows(other_escrows, &config, lp_mint.key, &authority)?;
    let reserves = current_common
        .checked_add(other_common)
        .ok_or(LpWrapError::Overflow)?;

    let supply = read_mint_supply(wrapped_mint)?;
    let deposit_common =
        to_common_units(amount, lp_decimals, config.share_decimals).ok_or(LpWrapError::Overflow)?;
    let gross_shares =
        shares_on_deposit(deposit_common, supply, reserves).ok_or(LpWrapError::Overflow)?;
    // 1 bps mint fee: the fee shares are never minted (i.e. minted-then-burned),
    // so the full deposit backs fewer shares and NAV per share rises.
    let (_mint_fee, net_shares) = crate::apply_fee(gross_shares);
    if net_shares == 0 {
        return Err(LpWrapError::ZeroSharesMinted.into());
    }

    // Move LP from the user into the escrow.
    invoke(
        &spl_token_2022::instruction::transfer_checked(
            lp_token_program.key,
            source_lp.key,
            lp_mint.key,
            lp_escrow.key,
            transfer_authority.key,
            &[],
            amount,
            lp_decimals,
        )?,
        &[
            source_lp.clone(),
            lp_mint.clone(),
            lp_escrow.clone(),
            transfer_authority.clone(),
        ],
    )?;

    // Mint shares to the recipient, signed by the mint authority PDA.
    let authority_bump = [authority_bump];
    let authority_seeds =
        get_wrapped_mint_authority_signer_seeds(wrapped_mint.key, &authority_bump);
    invoke_signed(
        &spl_token_2022::instruction::mint_to(
            wrapped_token_program.key,
            wrapped_mint.key,
            recipient_share.key,
            wrapped_mint_authority.key,
            &[],
            net_shares,
        )?,
        &[
            wrapped_mint.clone(),
            recipient_share.clone(),
            wrapped_mint_authority.clone(),
        ],
        &[&authority_seeds],
    )?;

    // Register the LP mint (with its decimals) if it is new.
    let mut config_data = pair_config.try_borrow_mut_data()?;
    let config_mut = bytemuck::try_from_bytes_mut::<PairConfig>(&mut config_data)
        .map_err(|_| ProgramError::InvalidAccountData)?;
    config_mut.register(lp_mint.key, lp_decimals)?;

    Ok(())
}

/// Processes the `Unwrap` instruction.
pub fn process_unwrap(program_id: &Pubkey, accounts: &[AccountInfo], shares: u64) -> ProgramResult {
    if shares == 0 {
        return Err(LpWrapError::ZeroAmount.into());
    }
    let account_info_iter = &mut accounts.iter();
    let source_share = next_account_info(account_info_iter)?;
    let wrapped_mint = next_account_info(account_info_iter)?;
    let wrapped_mint_authority = next_account_info(account_info_iter)?;
    let pair_config = next_account_info(account_info_iter)?;
    let wrapped_token_program = next_account_info(account_info_iter)?;
    let lp_token_program = next_account_info(account_info_iter)?;
    let destination_lp = next_account_info(account_info_iter)?;
    let lp_mint = next_account_info(account_info_iter)?;
    let lp_escrow = next_account_info(account_info_iter)?;
    let burn_authority = next_account_info(account_info_iter)?;
    let other_escrows = account_info_iter.as_slice();

    let config = load_pair_config(program_id, pair_config, wrapped_mint.key)?;

    let expected_mint =
        get_wrapped_mint_address(&config.mint_a, &config.mint_b, wrapped_token_program.key);
    if expected_mint != *wrapped_mint.key {
        return Err(LpWrapError::WrappedMintMismatch.into());
    }

    let (authority, authority_bump) = get_wrapped_mint_authority_with_seed(wrapped_mint.key);
    if *wrapped_mint_authority.key != authority {
        return Err(LpWrapError::MintAuthorityMismatch.into());
    }

    let expected_escrow = get_escrow_address(&authority, lp_mint.key, lp_token_program.key);
    if *lp_escrow.key != expected_escrow {
        return Err(LpWrapError::EscrowMismatch.into());
    }

    // You can only withdraw an LP that was actually wrapped into this pair.
    if !config.contains(lp_mint.key) {
        return Err(LpWrapError::EscrowSetMismatch.into());
    }
    let lp_decimals = config
        .decimals_of(lp_mint.key)
        .ok_or(LpWrapError::EscrowSetMismatch)?;

    let (escrow_owner, escrow_mint, current_amount) = read_token_account(lp_escrow)?;
    if escrow_owner != authority {
        return Err(LpWrapError::EscrowOwnerMismatch.into());
    }
    if escrow_mint != *lp_mint.key {
        return Err(LpWrapError::EscrowMismatch.into());
    }

    let current_common =
        to_common_units(current_amount, lp_decimals, config.share_decimals).ok_or(LpWrapError::Overflow)?;
    let other_common = sum_other_escrows(other_escrows, &config, lp_mint.key, &authority)?;
    let reserves = current_common
        .checked_add(other_common)
        .ok_or(LpWrapError::Overflow)?;

    let supply = read_mint_supply(wrapped_mint)?;
    // 1 bps burn fee: the full `shares` are burned from supply, but assets are
    // paid out only on the post-fee amount. The fee portion's reserves stay in
    // escrow, lifting NAV per remaining share.
    let (_burn_fee, effective_shares) = crate::apply_fee(shares);
    let assets_common =
        assets_on_redeem_common(effective_shares, supply, reserves).ok_or(LpWrapError::Overflow)?;
    let assets =
        from_common_units(assets_common, lp_decimals, config.share_decimals).ok_or(LpWrapError::Overflow)?;
    if assets == 0 {
        return Err(LpWrapError::ZeroAmount.into());
    }
    if assets > current_amount {
        // The proportional share value exceeds what this particular escrow
        // holds; the caller should redeem against a different LP or a smaller
        // amount.
        return Err(LpWrapError::InsufficientEscrowLiquidity.into());
    }

    // Burn the shares from the user (user signs).
    invoke(
        &spl_token_2022::instruction::burn(
            wrapped_token_program.key,
            source_share.key,
            wrapped_mint.key,
            burn_authority.key,
            &[],
            shares,
        )?,
        &[
            source_share.clone(),
            wrapped_mint.clone(),
            burn_authority.clone(),
        ],
    )?;

    // Transfer the LP out of escrow, signed by the mint authority PDA.
    let authority_bump = [authority_bump];
    let authority_seeds =
        get_wrapped_mint_authority_signer_seeds(wrapped_mint.key, &authority_bump);
    invoke_signed(
        &spl_token_2022::instruction::transfer_checked(
            lp_token_program.key,
            lp_escrow.key,
            lp_mint.key,
            destination_lp.key,
            wrapped_mint_authority.key,
            &[],
            assets,
            lp_decimals,
        )?,
        &[
            lp_escrow.clone(),
            lp_mint.clone(),
            destination_lp.clone(),
            wrapped_mint_authority.clone(),
        ],
        &[&authority_seeds],
    )?;

    Ok(())
}

/// Instruction dispatcher.
pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    match LpWrapInstruction::unpack(instruction_data)? {
        LpWrapInstruction::CreatePairMint {
            decimals,
            name,
            symbol,
            uri,
        } => process_create_pair_mint(program_id, accounts, decimals, name, symbol, uri),
        LpWrapInstruction::Wrap { amount } => process_wrap(program_id, accounts, amount),
        LpWrapInstruction::Unwrap { shares } => process_unwrap(program_id, accounts, shares),
    }
}
