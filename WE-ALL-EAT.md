# WE ALL EAT wrapper fork

Work branch: `codex/we-all-eat-wrap`. This fork starts from current upstream
`solana-program/token-wrap`. The inherited program/CLI still implement upstream
behavior. **No WE ALL EAT factory is deployed. Do not deploy or present the inherited
TwRap program ID as WE ALL EAT.**

Implemented first: `crates/we-all-eat-math`, a dependency-free, checked integer
accounting kernel with executable tests. Run:

```sh
cargo test --manifest-path crates/we-all-eat-math/Cargo.toml
```

## Permissionless factory target

Anyone can create a wrapper for a supported classic SPL or Token-2022 mint;
StonkFun's quote list is discovery only. Every output mint must be Token-2022.
Creator-selected mint/redemption fees and a mandatory nonzero transfer fee are
stored in a wrapper config PDA. The creator selects a reward mint. Canonical
wrapper seeds must include the config identity so another creator cannot squat
an underlying with unwanted fee settings.

Mint, withheld-fee withdrawal, and fee-configuration authorities must be program
PDAs. No creator EOA can withdraw backing or harvested fees. Configuration changes
must validate ranges and handle the Token-2022 two-epoch fee transition. Unsupported
underlying extensions must be rejected explicitly (transfer hooks need a guarded,
separately tested integration; confidential/nontransferable mints cannot be assumed
compatible with Orca). AMM support is checked separately from factory support.

## Accounting implemented

- R is spendable underlying in the canonical vault. S is actual wrapper supply,
  including fee-vault shares and permanently locked bootstrap shares.
- Deposit D is the **measured net vault balance increase** after transfer taxes.
  Gross shares = floor(D × S / R), then creator mint fees apply.
- Redeem Q uses the **pre-burn** R/S ratio on Q minus the redemption fee. The user
  burns Q directly; only the nonburn fee allocations are reissued to fee vaults.
- Harvested transfer fees already exist in S. Burning that allocation reduces S
  without reducing R. LP/bid shares retain collateral until separately redeemed.
- Allocation is configurable: LP/bid allocations floor, remainder burns. Tests
  exercise 33.33% LP, 33.33% bids, remainder burn; this is not a deployed default.
- Locked bootstrap shares remain in supply forever. Zero outputs and zero minimums
  reject; donated reserves adjust the price; no unchecked or floating point math.

## Next on-chain work (not implemented yet)

1. New deployment ID, config accounts, versioned create/wrap/unwrap/harvest
   instructions, and client encoding; do not silently change upstream instruction
   semantics while keeping its deployment ID.
2. Wire the kernel into guarded CPI handlers. Check account owners, canonical
   mints/vaults, authority signatures, supply and reserve immediately around CPIs.
   Enforce both min minted shares and **actual net underlying received** on redeem.
3. Atomic bootstrap: permissionless creation plus seed deposit, inaccessible
   seed-share account, and explicit handling of prefunded vault dust. The kernel
   rejects a nonempty reserve with zero supply; production must avoid donation DoS.
4. Initialize mandatory transfer-fee extension and PDA authorities. Permissionless
   harvest cannot redirect output: burn/LP/bid destinations are fixed by config.
5. Real Token-2022 SVM tests for transfer caps/epoch changes, withheld fees, account
   substitutions, CPI rollback/reentrancy, donations and last-holder behavior.
6. Jupiter swaps and Orca Splash/optional order-book legs can consume only allocated
   fee assets. Never spend the same fee as both a burn and liquidity funding.

The website exposes mainnet Orca LP tools now and clearly marks factory minting
and redemption pending. Core math tests are not a deployed-program audit.
