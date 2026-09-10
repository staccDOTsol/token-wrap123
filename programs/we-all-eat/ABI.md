# Wire format v1

Program: `4mEQkdKdjZS7q4963gduWRVqtKhkqpWuAr6oh2GUeB35`.
Every instruction starts with eight ASCII bytes `WAEIX001`, followed by the Borsh
variant tag (u8) and fields in the order below. Integers are little-endian, amounts
are raw u64 units, addresses/nonces are 32 bytes. Trailing data is rejected.
`Fees` = five u16s: mint_bps, redeem_bps, transfer_bps, lp_bps, bid_bps. All fee
parameters are 0..10000 except transfer_bps which is 1..10000; lp_bps + bid_bps
must be less than 10000. The remainder burns. There is no configurable fee cap.

| Tag | Instruction | Fields |
| --- | --- | --- |
| 0 | Create | nonce[32], Fees, amount u64, min_cooked u64 |
| 1 | Cook | amount u64, min_cooked u64 |
| 2 | Uncook | shares u64, min_raw_received u64 |
| 3 | Harvest | none |
| 4 | ScheduleFees | Fees |

Use the Rust builders in `src/instruction.rs`. Builders use the fixed new ID and
correct account order; the inherited upstream CLI and IDL do not encode this ABI.

## Derived addresses

All use this program ID and canonical bump derivation.

- Recipe: `["recipe", creator, nonce]`
- Cooked mint: `["cooked", recipe]`
- Raw reserve: `["reserve", recipe]`
- Locked seed account: `["locked", recipe]`
- LP fee account: `["lp", recipe]`
- Bid fee account: `["bids", recipe]`
- Fee collector: `["collector", recipe]`

The recipe is the mint/token/fee authority. Token vaults are owned by their token
program; their token owner is the recipe. The creator's initial output is the ATA
for creator + cooked mint + Token-2022. Subsequent user token accounts need not be
ATAs but must have the correct mint and signing user owner.

## Accounts in exact order

`w` writable, `s` signer; unmarked accounts readonly. The transaction payer can be
separate from the user on Cook/Uncook/Harvest. Create's creator signs and funds
account rent and the seed deposit. There is no signing authority on Harvest.

**Create (16)**: creator(w,s), recipe(w), cooked mint(w), raw mint, user raw source(w),
reserve(w), locked(w), lp(w), bids(w), collector(w), creator cooked ATA(w), reward
mint, raw token program, Token-2022 program, System program, ATA program.

**Cook / Uncook (12)**: user(s), recipe, cooked mint(w), raw mint, user raw account(w),
reserve(w), user cooked account(w), locked, lp(w), bids(w), raw token program,
Token-2022 program. Cook debits raw and credits cooked; Uncook burns cooked and
credits raw. Neither instruction accepts an arbitrary recipient.

**Harvest (9..25)**: recipe, cooked mint(w), raw mint, reserve, locked, lp(w), bids(w),
collector(w), Token-2022 program, then up to 16 cooked token accounts(w) with
withheld fees. An empty source list settles fees already harvested to the mint.

**ScheduleFees (4)**: creator(s), recipe(w), cooked mint(w), Token-2022 program.

Readonly account identities can repeat where meaningful: reward mint may equal raw
mint, and raw program may equal Token-2022. Vault identities remain distinct and
canonical. Token program executables are validated, not just supplied as accounts.

## Recipe account layout

239 bytes, program-owned. Eight-byte `WAERECP1` discriminator, then Borsh fields:
version(u8 = 1), bump(u8), creator[32], nonce[32], raw_mint[32], raw_program[32],
cooked_mint[32], reward_mint[32], decimals(u8), locked_shares(u64), current_fees(10
bytes), next_fees(10 bytes), effective_epoch(u64). Before effective_epoch use
current_fees; on/after it use next_fees. On initialization both policies are equal.

Discovery filters can use dataSize=239 and discriminator bytes at offset 0; the
creator is at offset 10, raw mint at offset 74 and cooked mint at offset 138.
Always verify owner, version, canonical PDA and the actual token/vault accounts
before calculating backing or building a transaction.
