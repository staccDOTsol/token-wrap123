# WE ALL EAT wrapper fork

Work branch: `codex/we-all-eat-wrap`. The new implementation is
[`programs/we-all-eat`](programs/we-all-eat/README.md), a Pinocchio program with a
separate deployment ID and [versioned ABI](programs/we-all-eat/ABI.md). It uses the
checked accounting crate in `crates/we-all-eat-math`.

**Core instruction handlers are implemented and tested locally. The program is
not deployed.** The inherited `program/`, upstream CLI and TwRap ID still implement
upstream token-wrap behavior. Do not deploy or present TwRap as WE ALL EAT.

Implemented: atomic recipe/mint/vault creation plus bootstrap deposit, R/S-priced
cook and uncook with actual net transfer accounting, compulsory nonzero Token-2022
transfer fees, creator-selected entry/exit fees and burn/LP/bid allocations,
permissionless fee harvesting/burning, fixed PDA authorities/destinations, and
synchronized two-epoch fee schedules. Output mints are always Token-2022. Anyone
can create a recipe for a supported raw mint; StonkFun's list is only discovery.

The creator-selected reward mint is recorded. LP/bid fee shares are retained in
canonical program vaults. Spending those allocations through Jupiter, Orca or an
order book remains unimplemented; no backing is exposed to an arbitrary executor.
Public recipe discovery, metadata and browser transaction adapters still need to
be connected before enabling cooking on the site.

Validation uses 14 compiled SBF scenarios with real token-program CPIs plus the
12 independent accounting tests. Run `./scripts/test-we-all-eat.sh` after installing
the SBF toolchain. Test passing is not a mainnet deployment or an independent audit.
