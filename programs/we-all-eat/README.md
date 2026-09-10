# WE ALL EAT program v1

A new Pinocchio program in the token-wrap fork. The upstream `program/`, CLI and
TwRap ID remain unchanged. This crate contains the WE ALL EAT instruction handlers,
versioned config and Rust instruction builders. It is not deployed.

Prepared program ID: `4mEQkdKdjZS7q4963gduWRVqtKhkqpWuAr6oh2GUeB35`.
The corresponding deployment keypair is local, ignored and mode 0600. No mainnet
transaction has been submitted. Deploy this program under the matching ID; it
rejects calls made under a different program ID.

## Implemented behavior

- Permissionless recipe creation for classic SPL and supported Token-2022 mints.
  All cooked outputs are Token-2022, with the same decimal precision as the raw coin.
- Creator + nonce identify each recipe. Multiple recipes for the same raw mint
  coexist; the StonkFun list is not an allowlist. The chosen reward mint is recorded.
- Creation initializes the config, cooked mint, reserve, permanently held seed
  shares, LP/bid fee vaults, collection vault and creator's cooked ATA, and performs
  a seed deposit in one instruction. It accepts lamport-prefunded PDAs. Reserve
  addresses are program PDAs instead of externally creatable ATAs, preventing
  outsiders from initializing or token-prefunding an empty reserve.
- Cook prices shares at the pre-transfer reserve/supply ratio, using the actual
  increase in spendable reserve after the raw token transfer. Entry fees apply
  to gross shares. The burn share is never issued; retained allocations are minted
  directly into their fixed fee vaults, avoiding another transfer tax.
- Uncook burns the user's cooked tokens directly, applies the creator's exit fee,
  and returns the corresponding raw backing. The actual net raw credit must meet
  the user's minimum, including raw Token-2022 fees and caps at the current epoch.
  A failed check rolls back the entire instruction and all token CPIs.
- Permissionless harvest processes up to 16 accounts plus any fees already harvested
  to the mint. It withdraws only to the canonical collector. It atomically burns
  collected cooked shares and reissues the retained LP/bid allocations to fixed
  vaults. Net supply reduction equals the burn allocation; raw backing is untouched.
  Repeating a completed harvest does not charge or burn it again.
- Mint, fee configuration and withheld-fee authorities are the recipe PDA. There
  is no creator withdrawal, arbitrary CPI executor, vault-owner change or authority
  handoff instruction. Cooked freeze authority is absent.
- Creators can schedule entry, exit, transfer and allocation percentages together
  for epoch + 2. An outstanding schedule cannot be replaced before activation.
  Token-2022's delayed transfer-fee change and the program's entry/exit policy switch
  at the same epoch. Transfer fees must stay nonzero; their maximum-fee cap is fixed
  at `u64::MAX`, so a zero cap cannot silently disable the required percentage.
- Canonical account, mint, token-program, authority, state, decimal and post-CPI
  reserve/supply checks apply on every operation. User trade accounts must belong
  to the signing user. Account aliasing cannot turn fee or reserve vaults into user
  destinations. User minimum outputs must be positive.

All amounts and ratios use checked integer math from `we-all-eat-math`. Bootstrap
locks `10^min(decimals,3)` raw cooked units in an inaccessible account. Those shares
stay in supply. Donations and external burns affect R/S; no stored exchange rate
can drift from the actual reserve and mint supply.

## Supported underlying extensions

Plain SPL and Token-2022 mints, TransferFeeConfig, MetadataPointer, TokenMetadata,
GroupPointer, TokenGroup, GroupMemberPointer and TokenGroupMember are accepted.
All other mint extensions are rejected explicitly, including transfer hooks,
permanent delegates, confidential balances, default freezing and pausable mints.
There is no untrusted callback CPI path. User-account CPI guards or frozen accounts
can also cause the token program to reject an operation atomically.

Raw token freeze/mint authorities remain properties of the raw asset. This wrapper
does not remove those powers. Creator-selected fees can be high and can change
with the disclosed delay; a 100% fee can make an operation produce no usable output.
R/S growth is not a guarantee of net profit or a rising market price.

## Build and test

With the Solana SBF toolchain on PATH:

```sh
./scripts/test-we-all-eat.sh
```

Or set `CARGO_BUILD_SBF` to its installed executable. The script builds the actual
SBF binary, runs 14 Mollusk scenarios against real SPL/Token-2022/ATA program
binaries, runs the 12 accounting tests, and checks Clippy. It submits no network
transactions. Run from the repository root. The checked-in crate lockfile pins
compatible dependencies; `solana-address` is explicitly pinned to avoid a wincode
ABI mismatch between newer transitive address and instruction releases.

The SVM tests cover both raw token programs, actual transfer taxes and caps, fee
epoch transitions, donations, direct burns, final-user exit with seed shares
remaining, harvest idempotence, all-burn and split policies, dust rounding, 16-source
harvests, prefunding, failed-bootstrap and failed-redemption rollback, authority
hijack attempts, vault substitution, invalid signers, unsupported mints, and wire
version rejection. Test compute ceilings: create <400k CU; cook/uncook and a
16-source harvest <200k CU. These are test ceilings, not wallet fee estimates.

## Remaining integration

LP/bid allocations are held in program vaults. Jupiter conversion, Orca liquidity
funding, order-book placement, and later spending of those allocations are **not
implemented by this crate yet**. They must consume only their allocated shares,
keep backing and outputs in validated accounts, and enforce per-leg net minimums.
No default protocol MEME mint has been invented. Public recipe discovery, cooked
metadata, browser transaction adapters and production deployment remain separate
work. The public site should keep cooking unavailable until deployment and its
transaction paths have been wired and checked. Do not treat this implementation
or its tests as a completed independent audit.
