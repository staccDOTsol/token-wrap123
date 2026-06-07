# SPL LP Wrap Program

A unified wrapper for AMM **liquidity-provider (LP) tokens**, in the spirit of
`spl-token-wrap` but built for pooled liquidity instead of plain mints.

## What it does

Where `spl-token-wrap` wraps a single mint 1:1, this program wraps the LP token
of an AMM *pool* into a single canonical **share token** keyed off the **pair of
underlying mints** — not off the LP mint. LP tokens from different AMMs that all
represent the same `(mint_a, mint_b)` pair collapse into **one** wrapped token,
**agnostic of the source AMM**.

Supported AMMs (constant-product, fungible LP):

| AMM | Program ID | Pool layout source |
| --- | --- | --- |
| Raydium AMM v4 | `675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8` | `AmmInfo` (packed) |
| Raydium CPMM | `CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C` | `PoolState` (anchor) |
| PumpSwap | `pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA` | `Pool` (anchor) |
| Meteora Dynamic AMM | `Eo7WjKq67rjJQSZxS6z3YkapzY3eMj6Xy8X5EQVn5UaB` | `Pool` (anchor) |

Concentrated-liquidity AMMs (Raydium CLMM, Orca Whirlpools, Meteora DLMM) are
intentionally unsupported: their positions are NFTs, not fungible LP tokens, so
there is nothing fungible to wrap.

## How a wrap is verified

On `Wrap` the caller supplies the LP token **and the AMM pool account**. The
program:

1. identifies the AMM from the pool account's **owning program**, so only a real
   pool owned by a real AMM program is ever trusted;
2. deconstructs the pool to read its LP mint and two underlying mints;
3. verifies the supplied LP mint **exactly** equals the pool's LP mint; and
4. verifies the pool's two mints, sorted, **exactly** equal the pair this wrapped
   token is for.

## Share accounting (not 1:1)

The wrapped token is a **vault share**, not a 1:1 wrapper. Mint/burn is computed
from the ratio of total escrowed reserves to current share supply:

```
shares_minted = deposit * supply / reserves      (bootstrap: shares = deposit)
lp_returned   = shares  * reserves / supply
```

Consequences (by design):

- **Donating** LP into an escrow raises `reserves` without minting shares, so
  every remaining share is worth more.
- **Externally burning** the share supply raises value per remaining share.

### Decimal awareness

LP tokens from different AMMs can have different decimals, and the share mint has
its own decimals. Every reserve balance is **normalized to a common scale** (the
share mint's decimals) before being summed, and redemptions are denormalized
back into the specific LP mint's decimals. Each LP mint's decimals are recorded
in `PairConfig` at registration time, so reserve totals across heterogeneous LP
tokens are coherent.

### Safe reserve accounting

`PairConfig` keeps a registry of the LP mints that have actually been wrapped
(each one having passed AMM verification). Only registered LP mints count toward
reserves, so an attacker cannot inflate share value by donating a junk token into
an authority-owned account.

## The wrapped share token

Every wrapped share mint is **SPL Token-2022** and carries:

- **Metadata in the mint itself** — a `MetadataPointer` pointing at the mint plus
  an embedded `TokenMetadata` extension populated from caller-supplied
  name/symbol/uri.
- **A 1 bps transfer fee** — native Token-2022 `TransferFeeConfig`, with the mint
  authority PDA as the fee-config and withheld-withdraw authority.
- **A 1 bps mint fee and 1 bps burn fee**, charged in-program:
  - on `Wrap`, the fee shares are never minted (minted-then-burned), so the full
    deposit backs fewer shares;
  - on `Unwrap`, the full share amount is burned but assets are paid only on the
    post-fee amount, leaving the fee's reserves in escrow.

  Both fees are effectively burned, raising **NAV per remaining share**.

## Instructions

- `CreatePairMint { decimals, name, symbol, uri }` — creates the Token-2022 share
  mint (with a `TokenMetadata` extension from the caller-supplied metadata) and
  the `PairConfig` for a pair.
- `Wrap { amount }` — deposit an AMM LP token, mint shares.
- `Unwrap { shares }` — burn shares, withdraw a chosen LP token from its escrow.

## Status

Pure logic (share math, decimal normalization, AMM pool parsing/verification,
PDA derivation, registry) is covered by unit tests (`cargo test -p spl-lp-wrap`).
The pool byte-offsets are taken from each AMM's on-chain state definitions and
should be confirmed against live mainnet pool accounts before deployment.
