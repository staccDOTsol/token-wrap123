//! Program instructions

use {
    solana_instruction::{AccountMeta, Instruction},
    solana_program_error::ProgramError,
    solana_pubkey::Pubkey,
};

/// Instructions supported by the LP Wrap program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LpWrapInstruction {
    /// Create the wrapped share mint and `PairConfig` for a `(mint_a, mint_b)`
    /// pair. The wrapped mint is an SPL Token-2022 mint carrying a
    /// `TokenMetadata` extension populated from the caller-supplied
    /// name/symbol/uri. The caller must pre-fund the wrapped mint and config
    /// PDAs with enough lamports for rent.
    ///
    /// Accounts:
    /// 0. `[w]` Wrapped share mint to create (PDA)
    /// 1. `[w]` `PairConfig` account to create (PDA)
    /// 2. `[]` Wrapped mint authority (PDA), signs the metadata init CPI
    /// 3. `[]` First underlying mint
    /// 4. `[]` Second underlying mint
    /// 5. `[]` System program
    /// 6. `[]` SPL Token-2022 program
    CreatePairMint {
        /// Decimals for the wrapped share mint.
        decimals: u8,
        /// Metadata name.
        name: String,
        /// Metadata symbol.
        symbol: String,
        /// Metadata URI.
        uri: String,
    },

    /// Deposit an AMM LP token and mint wrapped shares. The amount of shares
    /// minted is proportional to the vault's reserves and supply, not 1:1.
    ///
    /// Accounts:
    /// 0. `[w]` Recipient wrapped share token account
    /// 1. `[w]` Wrapped share mint
    /// 2. `[]` Wrapped mint authority (PDA)
    /// 3. `[w]` `PairConfig` (PDA)
    /// 4. `[]` SPL Token-2022 program (wrapped mint)
    /// 5. `[]` LP token program
    /// 6. `[w]` Source LP token account
    /// 7. `[]` LP mint
    /// 8. `[w]` LP escrow ATA for this LP mint
    /// 9. `[]` AMM pool account (owned by the AMM program)
    /// 10. `[s]` Transfer authority on the source LP account
    /// 11. `..` `[w]` Every *other* registered escrow (registry minus this one),
    ///     used to total reserves
    Wrap {
        /// Amount of LP tokens to deposit.
        amount: u64,
    },

    /// Burn wrapped shares and withdraw the proportional amount of a chosen LP
    /// token from its escrow.
    ///
    /// Accounts:
    /// 0. `[w]` Source wrapped share token account (burned from)
    /// 1. `[w]` Wrapped share mint
    /// 2. `[]` Wrapped mint authority (PDA)
    /// 3. `[w]` `PairConfig` (PDA)
    /// 4. `[]` SPL Token-2022 program (wrapped mint)
    /// 5. `[]` LP token program
    /// 6. `[w]` Destination LP token account
    /// 7. `[]` Requested LP mint
    /// 8. `[w]` LP escrow ATA for the requested LP mint
    /// 9. `[s]` Burn authority on the source share account
    /// 10. `..` `[w]` Every *other* registered escrow (registry minus this one),
    ///     used to total reserves
    Unwrap {
        /// Amount of wrapped shares to burn.
        shares: u64,
    },
}

fn pack_string(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
    buf.extend_from_slice(s.as_bytes());
}

fn unpack_string(input: &[u8]) -> Result<(String, &[u8]), ProgramError> {
    let (len_bytes, rest) = input
        .split_at_checked(4)
        .ok_or(ProgramError::InvalidInstructionData)?;
    let len = u32::from_le_bytes(len_bytes.try_into().unwrap()) as usize;
    let (str_bytes, rest) = rest
        .split_at_checked(len)
        .ok_or(ProgramError::InvalidInstructionData)?;
    let s = String::from_utf8(str_bytes.to_vec())
        .map_err(|_| ProgramError::InvalidInstructionData)?;
    Ok((s, rest))
}

impl LpWrapInstruction {
    /// Pack into a byte buffer.
    pub fn pack(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        match self {
            LpWrapInstruction::CreatePairMint {
                decimals,
                name,
                symbol,
                uri,
            } => {
                buf.push(0);
                buf.push(*decimals);
                pack_string(&mut buf, name);
                pack_string(&mut buf, symbol);
                pack_string(&mut buf, uri);
            }
            LpWrapInstruction::Wrap { amount } => {
                buf.push(1);
                buf.extend_from_slice(&amount.to_le_bytes());
            }
            LpWrapInstruction::Unwrap { shares } => {
                buf.push(2);
                buf.extend_from_slice(&shares.to_le_bytes());
            }
        }
        buf
    }

    /// Unpack from a byte buffer.
    pub fn unpack(input: &[u8]) -> Result<Self, ProgramError> {
        let (&tag, rest) = input
            .split_first()
            .ok_or(ProgramError::InvalidInstructionData)?;
        match tag {
            0 => {
                let (&decimals, rest) = rest
                    .split_first()
                    .ok_or(ProgramError::InvalidInstructionData)?;
                let (name, rest) = unpack_string(rest)?;
                let (symbol, rest) = unpack_string(rest)?;
                let (uri, _rest) = unpack_string(rest)?;
                Ok(LpWrapInstruction::CreatePairMint {
                    decimals,
                    name,
                    symbol,
                    uri,
                })
            }
            1 if rest.len() == 8 => Ok(LpWrapInstruction::Wrap {
                amount: u64::from_le_bytes(rest.try_into().unwrap()),
            }),
            2 if rest.len() == 8 => Ok(LpWrapInstruction::Unwrap {
                shares: u64::from_le_bytes(rest.try_into().unwrap()),
            }),
            _ => Err(ProgramError::InvalidInstructionData),
        }
    }
}

/// Build a `CreatePairMint` instruction.
#[allow(clippy::too_many_arguments)]
pub fn create_pair_mint(
    program_id: &Pubkey,
    wrapped_mint: &Pubkey,
    pair_config: &Pubkey,
    wrapped_mint_authority: &Pubkey,
    mint_a: &Pubkey,
    mint_b: &Pubkey,
    wrapped_token_program_id: &Pubkey,
    decimals: u8,
    name: String,
    symbol: String,
    uri: String,
) -> Instruction {
    let accounts = vec![
        AccountMeta::new(*wrapped_mint, false),
        AccountMeta::new(*pair_config, false),
        AccountMeta::new_readonly(*wrapped_mint_authority, false),
        AccountMeta::new_readonly(*mint_a, false),
        AccountMeta::new_readonly(*mint_b, false),
        AccountMeta::new_readonly(solana_system_interface::program::id(), false),
        AccountMeta::new_readonly(*wrapped_token_program_id, false),
    ];
    let data = LpWrapInstruction::CreatePairMint {
        decimals,
        name,
        symbol,
        uri,
    }
    .pack();
    Instruction::new_with_bytes(*program_id, &data, accounts)
}

/// Build a `Wrap` instruction. `other_escrows` is every registered escrow for
/// the pair except the one for `lp_mint`.
#[allow(clippy::too_many_arguments)]
pub fn wrap(
    program_id: &Pubkey,
    recipient_share_account: &Pubkey,
    wrapped_mint: &Pubkey,
    wrapped_mint_authority: &Pubkey,
    pair_config: &Pubkey,
    wrapped_token_program_id: &Pubkey,
    lp_token_program_id: &Pubkey,
    source_lp_account: &Pubkey,
    lp_mint: &Pubkey,
    lp_escrow: &Pubkey,
    amm_pool: &Pubkey,
    transfer_authority: &Pubkey,
    other_escrows: &[&Pubkey],
    amount: u64,
) -> Instruction {
    let mut accounts = vec![
        AccountMeta::new(*recipient_share_account, false),
        AccountMeta::new(*wrapped_mint, false),
        AccountMeta::new_readonly(*wrapped_mint_authority, false),
        AccountMeta::new(*pair_config, false),
        AccountMeta::new_readonly(*wrapped_token_program_id, false),
        AccountMeta::new_readonly(*lp_token_program_id, false),
        AccountMeta::new(*source_lp_account, false),
        AccountMeta::new_readonly(*lp_mint, false),
        AccountMeta::new(*lp_escrow, false),
        AccountMeta::new_readonly(*amm_pool, false),
        AccountMeta::new_readonly(*transfer_authority, true),
    ];
    for escrow in other_escrows {
        accounts.push(AccountMeta::new(**escrow, false));
    }
    let data = LpWrapInstruction::Wrap { amount }.pack();
    Instruction::new_with_bytes(*program_id, &data, accounts)
}

/// Build an `Unwrap` instruction. `other_escrows` is every registered escrow
/// for the pair except the one for `lp_mint`.
#[allow(clippy::too_many_arguments)]
pub fn unwrap(
    program_id: &Pubkey,
    source_share_account: &Pubkey,
    wrapped_mint: &Pubkey,
    wrapped_mint_authority: &Pubkey,
    pair_config: &Pubkey,
    wrapped_token_program_id: &Pubkey,
    lp_token_program_id: &Pubkey,
    destination_lp_account: &Pubkey,
    lp_mint: &Pubkey,
    lp_escrow: &Pubkey,
    burn_authority: &Pubkey,
    other_escrows: &[&Pubkey],
    shares: u64,
) -> Instruction {
    let mut accounts = vec![
        AccountMeta::new(*source_share_account, false),
        AccountMeta::new(*wrapped_mint, false),
        AccountMeta::new_readonly(*wrapped_mint_authority, false),
        AccountMeta::new(*pair_config, false),
        AccountMeta::new_readonly(*wrapped_token_program_id, false),
        AccountMeta::new_readonly(*lp_token_program_id, false),
        AccountMeta::new(*destination_lp_account, false),
        AccountMeta::new_readonly(*lp_mint, false),
        AccountMeta::new(*lp_escrow, false),
        AccountMeta::new_readonly(*burn_authority, true),
    ];
    for escrow in other_escrows {
        accounts.push(AccountMeta::new(**escrow, false));
    }
    let data = LpWrapInstruction::Unwrap { shares }.pack();
    Instruction::new_with_bytes(*program_id, &data, accounts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_create_pair_mint() {
        let ix = LpWrapInstruction::CreatePairMint {
            decimals: 9,
            name: "SOL-USDC LP".to_string(),
            symbol: "wLP".to_string(),
            uri: "https://example.com/lp.json".to_string(),
        };
        assert_eq!(LpWrapInstruction::unpack(&ix.pack()).unwrap(), ix);
    }

    #[test]
    fn round_trip_wrap_unwrap() {
        let w = LpWrapInstruction::Wrap { amount: 12_345 };
        assert_eq!(LpWrapInstruction::unpack(&w.pack()).unwrap(), w);
        let u = LpWrapInstruction::Unwrap { shares: 67_890 };
        assert_eq!(LpWrapInstruction::unpack(&u.pack()).unwrap(), u);
    }

    #[test]
    fn rejects_bad_tag() {
        assert!(LpWrapInstruction::unpack(&[9, 0, 0]).is_err());
        assert!(LpWrapInstruction::unpack(&[]).is_err());
    }
}
