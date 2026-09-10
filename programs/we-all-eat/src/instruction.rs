//! Versioned wire ABI; no inherited token-wrap instruction is repurposed.
use crate::{
    state::{Addresses, Fees, Key},
    tokens, ID,
};
use borsh::{BorshDeserialize, BorshSerialize};
use pinocchio::Address;
use solana_instruction::{AccountMeta, Instruction};
pub const PREFIX: &[u8; 8] = b"WAEIX001";
#[derive(Debug, Clone, BorshDeserialize, BorshSerialize)]
pub enum WaeInstruction {
    Create {
        nonce: Key,
        fees: Fees,
        amount: u64,
        min_cooked: u64,
    },
    Cook {
        amount: u64,
        min_cooked: u64,
    },
    Uncook {
        shares: u64,
        min_raw_received: u64,
    },
    Harvest,
    ScheduleFees {
        fees: Fees,
    },
}
pub fn encode(i: &WaeInstruction) -> Vec<u8> {
    let mut data = PREFIX.to_vec();
    data.extend(borsh::to_vec(i).expect("serialize instruction"));
    data
}
fn w(k: Address) -> AccountMeta {
    AccountMeta::new(k, false)
}
fn r(k: Address) -> AccountMeta {
    AccountMeta::new_readonly(k, false)
}
fn s(k: Address) -> AccountMeta {
    AccountMeta::new(k, true)
}
#[allow(clippy::too_many_arguments)]
pub fn create(
    creator: Address,
    nonce: Key,
    raw: Address,
    raw_program: Address,
    source: Address,
    reward: Address,
    fees: Fees,
    amount: u64,
    min_cooked: u64,
) -> Instruction {
    let a = Addresses::new(&ID, &creator, &nonce);
    let dest=spl_associated_token_account_interface::address::get_associated_token_address_with_program_id(&creator,&a.cooked,&tokens::T22);
    Instruction {
        program_id: ID,
        accounts: vec![
            s(creator),
            w(a.recipe),
            w(a.cooked),
            r(raw),
            w(source),
            w(a.reserve),
            w(a.locked),
            w(a.lp),
            w(a.bids),
            w(a.collector),
            w(dest),
            r(reward),
            r(raw_program),
            r(tokens::T22),
            r(pinocchio_system::ID),
            r(spl_associated_token_account_interface::program::ID),
        ],
        data: encode(&WaeInstruction::Create {
            nonce,
            fees,
            amount,
            min_cooked,
        }),
    }
}
/// Cook/uncook use the same account order. `raw_account` is owned by `user`;
/// `cooked_account` is likewise user-owned. Programs never select destinations.
#[allow(clippy::too_many_arguments)]
pub fn trade(
    user: Address,
    recipe: Address,
    raw: Address,
    raw_program: Address,
    raw_account: Address,
    cooked_account: Address,
    instruction: WaeInstruction,
) -> Instruction {
    assert!(matches!(
        instruction,
        WaeInstruction::Cook { .. } | WaeInstruction::Uncook { .. }
    ));
    let a = Addresses::for_recipe(&ID, &recipe);
    Instruction {
        program_id: ID,
        accounts: vec![
            r_signer(user),
            r(recipe),
            w(a.cooked),
            r(raw),
            w(raw_account),
            w(a.reserve),
            w(cooked_account),
            r(a.locked),
            w(a.lp),
            w(a.bids),
            r(raw_program),
            r(tokens::T22),
        ],
        data: encode(&instruction),
    }
}
fn r_signer(k: Address) -> AccountMeta {
    AccountMeta::new_readonly(k, true)
}
pub fn harvest(recipe: Address, raw: Address, sources: &[Address]) -> Instruction {
    let a = Addresses::for_recipe(&ID, &recipe);
    let mut accounts = vec![
        r(recipe),
        w(a.cooked),
        r(raw),
        r(a.reserve),
        r(a.locked),
        w(a.lp),
        w(a.bids),
        w(a.collector),
        r(tokens::T22),
    ];
    accounts.extend(sources.iter().copied().map(w));
    Instruction {
        program_id: ID,
        accounts,
        data: encode(&WaeInstruction::Harvest),
    }
}
pub fn schedule_fees(creator: Address, recipe: Address, fees: Fees) -> Instruction {
    let a = Addresses::for_recipe(&ID, &recipe);
    Instruction {
        program_id: ID,
        accounts: vec![r_signer(creator), w(recipe), w(a.cooked), r(tokens::T22)],
        data: encode(&WaeInstruction::ScheduleFees { fees }),
    }
}
