use crate::{require, Error, Result};
use borsh::{BorshDeserialize, BorshSerialize};
use pinocchio::{AccountView, Address};
pub type Key = [u8; 32];
pub const TAG: &[u8; 8] = b"WAERECP1";
pub const VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Fees {
    pub mint_bps: u16,
    pub redeem_bps: u16,
    pub transfer_bps: u16,
    pub lp_bps: u16,
    pub bid_bps: u16,
}
impl Fees {
    pub fn math(self) -> we_all_eat_math::FeePolicy {
        we_all_eat_math::FeePolicy {
            mint_bps: self.mint_bps,
            redeem_bps: self.redeem_bps,
            transfer_bps: self.transfer_bps,
            lp_bps: self.lp_bps,
            bid_bps: self.bid_bps,
        }
    }
    pub fn validate(self) -> Result<()> {
        self.math()
            .validate()
            .map(|_| ())
            .map_err(|_| Error::InvalidPolicy.into())
    }
}
#[derive(Debug, Clone, BorshSerialize, BorshDeserialize)]
pub struct Recipe {
    pub version: u8,
    pub bump: u8,
    pub creator: Key,
    pub nonce: Key,
    pub raw_mint: Key,
    pub raw_program: Key,
    pub cooked_mint: Key,
    pub reward_mint: Key,
    pub decimals: u8,
    pub locked_shares: u64,
    pub current_fees: Fees,
    pub next_fees: Fees,
    pub effective_epoch: u64,
}
impl Recipe {
    pub const SPACE: usize = 239;
    pub fn fees(&self, epoch: u64) -> Fees {
        if epoch >= self.effective_epoch {
            self.next_fees
        } else {
            self.current_fees
        }
    }
    pub fn load(a: &AccountView, id: &Address) -> Result<Self> {
        require(
            a.owned_by(id) && a.data_len() == Self::SPACE,
            Error::InvalidAccount,
        )?;
        let d = a.try_borrow()?;
        require(&d[..8] == TAG, Error::InvalidAccount)?;
        let r = Self::try_from_slice(&d[8..]).map_err(|_| Error::InvalidAccount)?;
        require(r.version == VERSION, Error::InvalidAccount)?;
        let (key, bump) = Address::find_program_address(&[b"recipe", &r.creator, &r.nonce], id);
        require(a.address() == &key && r.bump == bump, Error::InvalidPda)?;
        r.current_fees.validate()?;
        r.next_fees.validate()?;
        Ok(r)
    }
    pub fn save(&self, a: &mut AccountView) -> Result<()> {
        require(
            a.is_writable() && a.data_len() == Self::SPACE,
            Error::InvalidAccount,
        )?;
        let mut d = a.try_borrow_mut()?;
        d[..8].copy_from_slice(TAG);
        self.serialize(&mut &mut d[8..])
            .map_err(|_| Error::InvalidAccount.into())
    }
}
#[derive(Debug, Clone, Copy)]
pub struct Addresses {
    pub recipe: Address,
    pub cooked: Address,
    pub reserve: Address,
    pub locked: Address,
    pub lp: Address,
    pub bids: Address,
    pub collector: Address,
}
impl Addresses {
    pub fn new(id: &Address, creator: &Address, nonce: &Key) -> Self {
        let recipe = Address::find_program_address(&[b"recipe", creator.as_ref(), nonce], id).0;
        Self::for_recipe(id, &recipe)
    }
    pub fn for_recipe(id: &Address, recipe: &Address) -> Self {
        let derive = |tag: &[u8]| Address::find_program_address(&[tag, recipe.as_ref()], id).0;
        Self {
            recipe: *recipe,
            cooked: derive(b"cooked"),
            reserve: derive(b"reserve"),
            locked: derive(b"locked"),
            lp: derive(b"lp"),
            bids: derive(b"bids"),
            collector: derive(b"collector"),
        }
    }
}
