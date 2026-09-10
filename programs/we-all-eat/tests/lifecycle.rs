#![cfg(feature = "svm-tests")]
use borsh::BorshDeserialize;
use mollusk_svm::Mollusk;
use solana_account::Account;
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use spl_token_2022_interface::{
    extension::{
        transfer_fee::{TransferFeeAmount, TransferFeeConfig},
        BaseStateWithExtensions, BaseStateWithExtensionsMut, ExtensionType, StateWithExtensions,
        StateWithExtensionsMut,
    },
    instruction as token_ix,
    state::{Account as TokenAccount, AccountState, Mint},
};
use std::{collections::BTreeMap, path::PathBuf};
use we_all_eat_program::{
    instruction::{self, WaeInstruction as I},
    state::{Addresses, Fees, Recipe},
    tokens::T22,
    Error, ID,
};
const TOKEN: Pubkey = pinocchio_token::ID;
const FEES: Fees = Fees {
    mint_bps: 300,
    redeem_bps: 300,
    transfer_bps: 600,
    lp_bps: 2500,
    bid_bps: 2500,
};
const NO_ENTRY: Fees = Fees {
    mint_bps: 0,
    redeem_bps: 0,
    ..FEES
};
fn account(owner: Pubkey, data: Vec<u8>) -> Account {
    Account {
        owner,
        data,
        lamports: 50_000_000,
        ..Default::default()
    }
}
fn mint(program: Pubkey, fee: Option<(u16, u64)>) -> Account {
    let exts = if fee.is_some() {
        vec![ExtensionType::TransferFeeConfig]
    } else {
        vec![]
    };
    let mut data = vec![0; ExtensionType::try_calculate_account_len::<Mint>(&exts).unwrap()];
    let mut m = StateWithExtensionsMut::<Mint>::unpack_uninitialized(&mut data).unwrap();
    if let Some((bps, cap)) = fee {
        let f = m.init_extension::<TransferFeeConfig>(true).unwrap();
        f.older_transfer_fee.transfer_fee_basis_points = bps.into();
        f.older_transfer_fee.maximum_fee = cap.into();
        f.newer_transfer_fee = f.older_transfer_fee;
    }
    m.base.decimals = 6;
    m.base.supply = 100_000_000;
    m.base.is_initialized = true;
    m.pack_base();
    m.init_account_type().unwrap();
    account(program, data)
}
fn token(program: Pubkey, mint: Pubkey, owner: Pubkey, balance: u64, fee: bool) -> Account {
    let exts = if fee {
        vec![ExtensionType::TransferFeeAmount]
    } else {
        vec![]
    };
    let mut data =
        vec![0; ExtensionType::try_calculate_account_len::<TokenAccount>(&exts).unwrap()];
    let mut t = StateWithExtensionsMut::<TokenAccount>::unpack_uninitialized(&mut data).unwrap();
    if fee {
        t.init_extension::<TransferFeeAmount>(true).unwrap();
    }
    t.base.mint = mint;
    t.base.owner = owner;
    t.base.amount = balance;
    t.base.state = AccountState::Initialized;
    t.pack_base();
    t.init_account_type().unwrap();
    account(program, data)
}
struct Bank {
    vm: Mollusk,
    accounts: BTreeMap<Pubkey, Account>,
    user: Pubkey,
    raw: Pubkey,
    raw_program: Pubkey,
    source: Pubkey,
    dest: Pubkey,
    nonce: [u8; 32],
    a: Addresses,
}
impl Bank {
    fn new(fee: Option<(u16, u64)>) -> Self {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/deploy/we_all_eat_program");
        let mut vm = Mollusk::new(&ID, path.to_str().unwrap());
        vm.sysvars.clock.epoch = 1;
        mollusk_svm_programs_token::token::add_program(&mut vm);
        mollusk_svm_programs_token::token2022::add_program(&mut vm);
        mollusk_svm_programs_token::associated_token::add_program(&mut vm);
        let mut accounts = BTreeMap::from([
            mollusk_svm::program::keyed_account_for_system_program(),
            mollusk_svm_programs_token::token::keyed_account(),
            mollusk_svm_programs_token::token2022::keyed_account(),
            mollusk_svm_programs_token::associated_token::keyed_account(),
        ]);
        let user = Pubkey::new_unique();
        let raw = Pubkey::new_unique();
        let source = Pubkey::new_unique();
        let nonce = [7u8; 32];
        let raw_program = if fee.is_some() { T22 } else { TOKEN };
        let a = Addresses::new(&ID, &user, &nonce);
        let dest=spl_associated_token_account_interface::address::get_associated_token_address_with_program_id(&user,&a.cooked,&T22);
        accounts.insert(
            user,
            Account {
                lamports: 10_000_000_000,
                ..Default::default()
            },
        );
        accounts.insert(raw, mint(raw_program, fee));
        accounts.insert(
            source,
            token(raw_program, raw, user, 100_000_000, fee.is_some()),
        );
        Self {
            vm,
            accounts,
            user,
            raw,
            raw_program,
            source,
            dest,
            nonce,
            a,
        }
    }
    fn execute(&mut self, ix: Instruction, expected: Option<u32>) -> u64 {
        let mut keys: Vec<_> = ix.accounts.iter().map(|a| a.pubkey).collect();
        keys.sort();
        keys.dedup();
        let before: Vec<_> = keys
            .into_iter()
            .map(|k| (k, self.accounts.get(&k).cloned().unwrap_or_default()))
            .collect();
        let result = self.vm.process_instruction(&ix, &before);
        match expected {
            None => assert!(
                result.program_result.is_ok(),
                "{:?}, data {:?}",
                result.program_result,
                ix.data
            ),
            Some(code) => assert_eq!(
                result.program_result,
                mollusk_svm::result::ProgramResult::Failure(
                    solana_program_error::ProgramError::Custom(code)
                ),
                "{:?}",
                result.program_result
            ),
        }
        if expected.is_none() {
            for (k, v) in result.resulting_accounts {
                self.accounts.insert(k, v);
            }
        } else {
            assert_eq!(
                result.resulting_accounts, before,
                "failed instruction must roll back every account"
            );
        }
        result.compute_units_consumed
    }
    fn create_ix(&self, fees: Fees) -> Instruction {
        instruction::create(
            self.user,
            self.nonce,
            self.raw,
            self.raw_program,
            self.source,
            self.raw,
            fees,
            1_000_000,
            1,
        )
    }
    fn init(&mut self, fees: Fees) {
        let cu = self.execute(self.create_ix(fees), None);
        assert!(cu < 400_000, "initialization compute {cu}");
    }
    fn trade_ix(&self, i: I) -> Instruction {
        instruction::trade(
            self.user,
            self.a.recipe,
            self.raw,
            self.raw_program,
            self.source,
            self.dest,
            i,
        )
    }
    fn trade(&mut self, i: I) {
        let cu = self.execute(self.trade_ix(i), None);
        assert!(cu < 200_000, "trade compute {cu}");
    }
    fn amount(&self, k: Pubkey) -> u64 {
        StateWithExtensions::<TokenAccount>::unpack(&self.accounts[&k].data)
            .unwrap()
            .base
            .amount
    }
    fn supply(&self) -> u64 {
        StateWithExtensions::<Mint>::unpack(&self.accounts[&self.a.cooked].data)
            .unwrap()
            .base
            .supply
    }
    fn recipe(&self) -> Recipe {
        Recipe::try_from_slice(&self.accounts[&self.a.recipe].data[8..]).unwrap()
    }
    fn receiver(&mut self) -> Pubkey {
        let key = Pubkey::new_unique();
        self.accounts.insert(
            key,
            token(T22, self.a.cooked, Pubkey::new_unique(), 0, true),
        );
        key
    }
}
#[test]
fn classic_create_cook_uncook_real_token_cpis() {
    let mut b = Bank::new(None);
    b.init(FEES);
    assert_eq!(b.accounts[&b.a.recipe].data.len(), Recipe::SPACE);
    assert_eq!(b.amount(b.a.reserve), 1_000_000);
    assert_eq!(b.supply(), 985_000);
    assert_eq!(b.amount(b.dest), 969_000);
    assert_eq!(b.amount(b.a.locked), 1_000);
    assert_eq!(b.amount(b.a.lp), 7_500);
    assert_eq!(b.amount(b.a.bids), 7_500);
    let m = StateWithExtensions::<Mint>::unpack(&b.accounts[&b.a.cooked].data).unwrap();
    let fee = m.get_extension::<TransferFeeConfig>().unwrap();
    assert_eq!(m.base.mint_authority, Some(b.a.recipe).into());
    assert!(m.base.freeze_authority.is_none());
    assert_eq!(
        Option::<Pubkey>::from(fee.withdraw_withheld_authority),
        Some(b.a.recipe)
    );
    assert_eq!(
        Option::<Pubkey>::from(fee.transfer_fee_config_authority),
        Some(b.a.recipe)
    );
    assert_eq!(u64::from(fee.newer_transfer_fee.maximum_fee), u64::MAX);
    b.trade(I::Cook {
        amount: 100_000,
        min_cooked: 95_545,
    });
    assert_eq!(b.amount(b.a.reserve), 1_100_000);
    assert_eq!(b.supply(), 1_082_021);
    assert_eq!(b.amount(b.dest), 1_064_545);
    let user_raw = b.amount(b.source);
    let paid = (9700u128 * 1_100_000 / 1_082_021) as u64;
    b.trade(I::Uncook {
        shares: 10_000,
        min_raw_received: paid,
    });
    assert_eq!(b.amount(b.source) - user_raw, paid);
    assert_eq!(b.supply(), 1_082_021 - 10_000 + 150);
    assert_eq!(b.amount(b.a.reserve), 1_100_000 - paid);
}
#[test]
fn taxed_underlying_uses_net_delta_and_enforces_net_redemption_with_rollback() {
    let mut b = Bank::new(Some((1000, 500)));
    b.init(NO_ENTRY);
    assert_eq!(b.amount(b.a.reserve), 999_500);
    assert_eq!(b.supply(), 999_500);
    let reserve =
        StateWithExtensions::<TokenAccount>::unpack(&b.accounts[&b.a.reserve].data).unwrap();
    assert_eq!(
        u64::from(
            reserve
                .get_extension::<TransferFeeAmount>()
                .unwrap()
                .withheld_amount
        ),
        500
    );
    b.trade(I::Cook {
        amount: 10_000,
        min_cooked: 9_500,
    });
    assert_eq!(b.supply(), 1_009_000);
    let raw = b.amount(b.source);
    let supply = b.supply();
    b.execute(
        b.trade_ix(I::Uncook {
            shares: 10_000,
            min_raw_received: 9_501,
        }),
        Some(Error::Slippage as u32),
    );
    assert_eq!(b.supply(), supply);
    assert_eq!(b.amount(b.source), raw);
    b.trade(I::Uncook {
        shares: 10_000,
        min_raw_received: 9_500,
    });
    assert_eq!(b.amount(b.source) - raw, 9_500);
}
#[test]
fn donations_and_direct_burns_change_exchange_rate_without_unbacked_minting() {
    let mut b = Bank::new(None);
    b.init(NO_ENTRY);
    b.execute(
        token_ix::transfer_checked(
            &TOKEN,
            &b.source,
            &b.raw,
            &b.a.reserve,
            &b.user,
            &[],
            1_000_000,
            6,
        )
        .unwrap(),
        None,
    );
    assert_eq!(b.amount(b.a.reserve), 2_000_000);
    assert_eq!(b.supply(), 1_000_000);
    b.trade(I::Cook {
        amount: 100_000,
        min_cooked: 50_000,
    });
    assert_eq!(b.supply(), 1_050_000);
    b.trade(I::Uncook {
        shares: 50_000,
        min_raw_received: 100_000,
    });
    assert_eq!(b.amount(b.a.reserve), 2_000_000);
    b.execute(
        token_ix::burn_checked(&T22, &b.dest, &b.a.cooked, &b.user, &[], 100_000, 6).unwrap(),
        None,
    );
    b.trade(I::Cook {
        amount: 100_000,
        min_cooked: 45_000,
    });
    assert_eq!(b.supply(), 945_000);
    b.execute(
        b.trade_ix(I::Cook {
            amount: 1,
            min_cooked: 1,
        }),
        Some(Error::Accounting as u32),
    );
}
#[test]
fn actual_transfer_fees_are_permissionlessly_collected_and_burned_once() {
    let mut b = Bank::new(None);
    b.init(FEES);
    let receiver = b.receiver();
    b.execute(
        token_ix::transfer_checked(
            &T22,
            &b.dest,
            &b.a.cooked,
            &receiver,
            &b.user,
            &[],
            10_000,
            6,
        )
        .unwrap(),
        None,
    );
    assert_eq!(b.amount(receiver), 9400);
    let supply = b.supply();
    let reserve = b.amount(b.a.reserve);
    let lp = b.amount(b.a.lp);
    b.execute(instruction::harvest(b.a.recipe, b.raw, &[receiver]), None);
    assert_eq!(b.supply(), supply - 300);
    assert_eq!(b.amount(b.a.reserve), reserve);
    assert_eq!(b.amount(b.a.lp), lp + 150);
    assert_eq!(b.amount(b.a.collector), 0);
    let received =
        StateWithExtensions::<TokenAccount>::unpack(&b.accounts[&receiver].data).unwrap();
    assert_eq!(
        u64::from(
            received
                .get_extension::<TransferFeeAmount>()
                .unwrap()
                .withheld_amount
        ),
        0
    );
    let after = b.supply();
    b.execute(
        instruction::harvest(b.a.recipe, b.raw, &[receiver, receiver]),
        None,
    );
    assert_eq!(b.supply(), after);
}
#[test]
fn all_burn_policy_and_mint_harvesting_keep_every_raw_token() {
    let mut b = Bank::new(None);
    b.init(Fees {
        lp_bps: 0,
        bid_bps: 0,
        ..FEES
    });
    let receiver = b.receiver();
    b.execute(
        token_ix::transfer_checked(
            &T22,
            &b.dest,
            &b.a.cooked,
            &receiver,
            &b.user,
            &[],
            10_000,
            6,
        )
        .unwrap(),
        None,
    );
    b.execute(spl_token_2022_interface::extension::transfer_fee::instruction::harvest_withheld_tokens_to_mint(&T22,&b.a.cooked,&[&receiver]).unwrap(),None);
    let reserve = b.amount(b.a.reserve);
    let supply = b.supply();
    b.execute(instruction::harvest(b.a.recipe, b.raw, &[]), None);
    assert_eq!(b.supply(), supply - 600);
    assert_eq!(b.amount(b.a.reserve), reserve);
    assert_eq!(b.amount(b.a.lp), 0);
    assert_eq!(b.amount(b.a.bids), 0);
}
#[test]
fn fee_changes_switch_together_after_two_epochs_and_require_creator() {
    let mut b = Bank::new(None);
    b.init(FEES);
    let changed = Fees {
        mint_bps: 0,
        redeem_bps: 0,
        transfer_bps: 1000,
        ..FEES
    };
    b.execute(
        instruction::schedule_fees(Pubkey::new_unique(), b.a.recipe, changed),
        Some(Error::Unauthorized as u32),
    );
    b.execute(
        instruction::schedule_fees(b.user, b.a.recipe, changed),
        None,
    );
    assert_eq!(b.recipe().effective_epoch, 3);
    assert_eq!(b.recipe().fees(2), FEES);
    b.execute(
        instruction::schedule_fees(b.user, b.a.recipe, changed),
        Some(Error::PendingPolicy as u32),
    );
    b.vm.sysvars.clock.epoch = 2;
    let receiver = b.receiver();
    b.execute(
        token_ix::transfer_checked(
            &T22,
            &b.dest,
            &b.a.cooked,
            &receiver,
            &b.user,
            &[],
            10_000,
            6,
        )
        .unwrap(),
        None,
    );
    assert_eq!(b.amount(receiver), 9400);
    b.vm.sysvars.clock.epoch = 3;
    assert_eq!(b.recipe().fees(3), changed);
    let other = b.receiver();
    b.execute(
        token_ix::transfer_checked(&T22, &b.dest, &b.a.cooked, &other, &b.user, &[], 10_000, 6)
            .unwrap(),
        None,
    );
    assert_eq!(b.amount(other), 9000);
    let expected = (100_000u128 * b.supply() as u128 / b.amount(b.a.reserve) as u128) as u64;
    let before = b.amount(b.dest);
    b.trade(I::Cook {
        amount: 100_000,
        min_cooked: expected,
    });
    assert_eq!(b.amount(b.dest) - before, expected);
    b.execute(
        instruction::schedule_fees(
            b.user,
            b.a.recipe,
            Fees {
                transfer_bps: 0,
                ..changed
            },
        ),
        Some(Error::InvalidPolicy as u32),
    );
}
#[test]
fn prefunded_pdas_initialize_but_failed_bootstrap_rolls_everything_back() {
    let mut b = Bank::new(None);
    for k in [
        b.a.recipe,
        b.a.cooked,
        b.a.reserve,
        b.a.locked,
        b.a.lp,
        b.a.bids,
        b.a.collector,
    ] {
        b.accounts.insert(
            k,
            Account {
                lamports: 123,
                ..Default::default()
            },
        );
    }
    let before = b.accounts.clone();
    let mut fail = b.create_ix(FEES);
    fail.data = instruction::encode(&I::Create {
        nonce: b.nonce,
        fees: FEES,
        amount: 100,
        min_cooked: 1,
    });
    b.execute(fail, Some(Error::Accounting as u32));
    assert_eq!(b.accounts, before);
    b.init(FEES);
    b.execute(b.create_ix(FEES), Some(Error::AlreadyInitialized as u32));
}
#[test]
fn invalid_destinations_signers_and_cross_recipe_accounts_cannot_spend_backing() {
    let mut b = Bank::new(None);
    b.init(FEES);
    let mut no_signature = b.trade_ix(I::Cook {
        amount: 10_000,
        min_cooked: 1,
    });
    no_signature.accounts[0].is_signer = false;
    b.execute(no_signature, Some(Error::Unauthorized as u32));
    for index in [2, 5, 7, 8, 9] {
        let mut bad = b.trade_ix(I::Uncook {
            shares: 10_000,
            min_raw_received: 1,
        });
        let original = bad.accounts[index].pubkey;
        let clone = Pubkey::new_unique();
        b.accounts.insert(clone, b.accounts[&original].clone());
        bad.accounts[index].pubkey = clone;
        b.execute(
            bad,
            Some(if index == 2 {
                Error::InvalidAccount
            } else {
                Error::InvalidPda
            } as u32),
        );
    }
    let mut steal = b.trade_ix(I::Uncook {
        shares: 10,
        min_raw_received: 1,
    });
    steal.accounts[6].pubkey = b.a.lp;
    b.execute(steal, Some(Error::InvalidVault as u32));
    let mut alias = b.trade_ix(I::Uncook {
        shares: 10,
        min_raw_received: 1,
    });
    alias.accounts[4].pubkey = b.a.reserve;
    b.execute(alias, Some(Error::InvalidVault as u32));
    let mut bad_harvest = instruction::harvest(b.a.recipe, b.raw, &[]);
    bad_harvest.accounts[7].pubkey = b.dest;
    b.execute(bad_harvest, Some(Error::InvalidPda as u32));
    let mut bad_program = b.trade_ix(I::Cook {
        amount: 10_000,
        min_cooked: 1,
    });
    bad_program.accounts[10].pubkey = T22;
    b.execute(bad_program, Some(Error::InvalidAccount as u32));
}
#[test]
fn minimums_are_mandatory_and_donations_cannot_force_bad_quotes() {
    let mut b = Bank::new(None);
    b.init(NO_ENTRY);
    b.execute(
        b.trade_ix(I::Cook {
            amount: 1000,
            min_cooked: 0,
        }),
        Some(Error::Accounting as u32),
    );
    b.execute(
        b.trade_ix(I::Uncook {
            shares: 1000,
            min_raw_received: 0,
        }),
        Some(Error::Accounting as u32),
    );
    b.execute(
        token_ix::transfer_checked(
            &TOKEN,
            &b.source,
            &b.raw,
            &b.a.reserve,
            &b.user,
            &[],
            1_000_000,
            6,
        )
        .unwrap(),
        None,
    );
    b.execute(
        b.trade_ix(I::Cook {
            amount: 10_000,
            min_cooked: 9_000,
        }),
        Some(Error::Slippage as u32),
    );
    let user = b.amount(b.dest);
    b.trade(I::Uncook {
        shares: user,
        min_raw_received: 1,
    });
    assert_eq!(b.supply(), 1_000);
    assert_eq!(b.amount(b.a.locked), 1_000);
    assert!(b.amount(b.a.reserve) > 0);
}
#[test]
fn incompatible_token_extensions_and_non_mint_accounts_are_rejected() {
    let mut b = Bank::new(Some((100, 1000)));
    let mut data =
        vec![
            0;
            ExtensionType::try_calculate_account_len::<Mint>(&[ExtensionType::NonTransferable])
                .unwrap()
        ];
    let mut m = StateWithExtensionsMut::<Mint>::unpack_uninitialized(&mut data).unwrap();
    m.init_extension::<spl_token_2022_interface::extension::non_transferable::NonTransferable>(
        true,
    )
    .unwrap();
    m.base.is_initialized = true;
    m.base.decimals = 6;
    m.pack_base();
    m.init_account_type().unwrap();
    b.accounts.insert(b.raw, account(T22, data));
    b.execute(b.create_ix(FEES), Some(Error::UnsupportedMint as u32));
    b.accounts.get_mut(&b.raw).unwrap().owner = ID;
    b.execute(b.create_ix(FEES), Some(Error::InvalidAccount as u32));
}

#[test]
fn creator_cannot_take_mint_or_withheld_fee_authorities_or_drain_fee_vaults() {
    let mut b = Bank::new(None);
    b.init(FEES);
    let mismatch = spl_token_2022_interface::error::TokenError::OwnerMismatch as u32;
    for auth_type in [
        token_ix::AuthorityType::MintTokens,
        token_ix::AuthorityType::TransferFeeConfig,
        token_ix::AuthorityType::WithheldWithdraw,
    ] {
        b.execute(
            token_ix::set_authority(&T22, &b.a.cooked, Some(&b.user), auth_type, &b.user, &[])
                .unwrap(),
            Some(mismatch),
        );
    }
    b.execute(
        token_ix::transfer_checked(&T22, &b.a.lp, &b.a.cooked, &b.dest, &b.user, &[], 100, 6)
            .unwrap(),
        Some(mismatch),
    );
    b.execute(spl_token_2022_interface::extension::transfer_fee::instruction::withdraw_withheld_tokens_from_mint(&T22,&b.a.cooked,&b.dest,&b.user,&[]).unwrap(),Some(mismatch));
}

#[test]
fn underlying_fee_epoch_changes_are_measured_after_cpi() {
    let mut b = Bank::new(Some((1000, 500)));
    {
        let data = &mut b.accounts.get_mut(&b.raw).unwrap().data;
        let mut mint = StateWithExtensionsMut::<Mint>::unpack(data).unwrap();
        mint.get_extension_mut::<TransferFeeConfig>()
            .unwrap()
            .transfer_fee_config_authority = Some(b.user).try_into().unwrap();
    }
    b.init(NO_ENTRY);
    b.execute(
        spl_token_2022_interface::extension::transfer_fee::instruction::set_transfer_fee(
            &T22,
            &b.raw,
            &b.user,
            &[],
            2000,
            u64::MAX,
        )
        .unwrap(),
        None,
    );
    b.vm.sysvars.clock.epoch = 3;
    b.execute(
        b.trade_ix(I::Cook {
            amount: 10_000,
            min_cooked: 9500,
        }),
        Some(Error::Slippage as u32),
    );
    b.trade(I::Cook {
        amount: 10_000,
        min_cooked: 8000,
    });
    b.execute(
        b.trade_ix(I::Uncook {
            shares: 10_000,
            min_raw_received: 8001,
        }),
        Some(Error::Slippage as u32),
    );
    b.trade(I::Uncook {
        shares: 10_000,
        min_raw_received: 8000,
    });
}

#[test]
fn sixteen_real_fee_sources_fit_and_rounding_dust_burns() {
    let mut b = Bank::new(None);
    b.init(FEES);
    let mut sources = Vec::new();
    for _ in 0..16 {
        let receiver = b.receiver();
        b.execute(
            token_ix::transfer_checked(&T22, &b.dest, &b.a.cooked, &receiver, &b.user, &[], 1, 6)
                .unwrap(),
            None,
        );
        sources.push(receiver);
    }
    let reserve = b.amount(b.a.reserve);
    let supply = b.supply();
    let cu = b.execute(instruction::harvest(b.a.recipe, b.raw, &sources), None);
    assert!(cu < 200_000, "harvest compute {cu}");
    assert_eq!(b.supply(), supply - 8);
    assert_eq!(b.amount(b.a.reserve), reserve);
    let receiver = b.receiver();
    b.execute(
        token_ix::transfer_checked(&T22, &b.dest, &b.a.cooked, &receiver, &b.user, &[], 1, 6)
            .unwrap(),
        None,
    );
    let supply = b.supply();
    b.execute(instruction::harvest(b.a.recipe, b.raw, &[receiver]), None);
    assert_eq!(b.supply(), supply - 1);
    sources.push(receiver);
    b.execute(
        instruction::harvest(b.a.recipe, b.raw, &sources),
        Some(Error::TooManySources as u32),
    );
}

#[test]
fn inherited_wire_tags_and_trailing_bytes_cannot_reinterpret_instructions() {
    let mut b = Bank::new(None);
    b.init(FEES);
    let mut ix = b.trade_ix(I::Cook {
        amount: 100,
        min_cooked: 1,
    });
    ix.data = vec![1; 17];
    b.execute(ix, Some(Error::InvalidInstruction as u32));
    let mut ix = b.trade_ix(I::Cook {
        amount: 100,
        min_cooked: 1,
    });
    ix.data.push(0);
    b.execute(ix, Some(Error::InvalidInstruction as u32));
}
