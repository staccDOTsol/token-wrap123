'use strict';
// SOL "zap" feature for the SPL LP Wrap program.
//
// ZAP-IN(mintA, mintB, solLamports):
//   1. Split SOL ~half and use the Jupiter API (plain fetch) to build swaps
//      SOL->mintA and SOL->mintB (a side equal to wSOL is skipped).
//   2. Add liquidity on a supported AMM (Meteora dynamic-amm, then Raydium CPMM)
//      that has a (mintA,mintB) pool -> user receives LP tokens.
//   3. Wrap the LP via the spl-lp-wrap Wrap instruction (CreatePairMint first if
//      the wrapped pair doesn't exist yet).
//
// ZAP-OUT(wrappedMint, shares): the reverse -- Unwrap -> remove liquidity ->
// Jupiter swaps mintA->SOL and mintB->SOL.
//
// The server holds NO private key. Every endpoint returns base64-encoded
// *versioned* transactions; the client signs and sends them in order, re-reading
// balances between steps (the swap outputs depend on slippage so the flow is
// deliberately stepwise).
//
// Heavy SDKs (Raydium / Meteora / web3 / spl-token) are lazy-required INSIDE the
// handlers so `node server.js` boots fast and works offline.

const WSOL = 'So11111111111111111111111111111111111111112';
const DEPLOYER_DEFAULT = 'WzMaL78srutrF6CsxEkWuhMaDF5HZA6jNRaEPengqpb';
const PROGRAM_ID_DEFAULT = 'EbmEELwtg3iqHdtNCKcRwZCKmWzGpF11ZTvc9sPxBQJB';

// Jupiter Lite API (no key required). Override with JUP_BASE if desired.
const JUP_BASE = process.env.JUP_BASE || 'https://lite-api.jup.ag/swap/v1';

function lazy() {
  const web3 = require('@solana/web3.js');
  const splToken = require('@solana/spl-token');
  return { web3, splToken };
}

// ------------------------------------------------------------------ PDAs ----
function sortPair(web3, a, b) {
  return Buffer.compare(a.toBuffer(), b.toBuffer()) <= 0 ? [a, b] : [b, a];
}
function wrappedMintPda(web3, programId, mintA, mintB, wtp) {
  const [x, y] = sortPair(web3, mintA, mintB);
  return web3.PublicKey.findProgramAddressSync(
    [Buffer.from('lp_mint'), x.toBuffer(), y.toBuffer(), wtp.toBuffer()], programId)[0];
}
function authorityPda(web3, programId, wrappedMint) {
  return web3.PublicKey.findProgramAddressSync(
    [Buffer.from('authority'), wrappedMint.toBuffer()], programId)[0];
}
function configPda(web3, programId, wrappedMint) {
  return web3.PublicKey.findProgramAddressSync(
    [Buffer.from('config'), wrappedMint.toBuffer()], programId)[0];
}

// --------------------------------------------------------- instr packing ----
function packWrap(amount) {
  const buf = Buffer.alloc(9);
  buf.writeUInt8(1, 0);
  buf.writeBigUInt64LE(BigInt(amount), 1);
  return buf;
}
function packUnwrap(shares) {
  const buf = Buffer.alloc(9);
  buf.writeUInt8(2, 0);
  buf.writeBigUInt64LE(BigInt(shares), 1);
  return buf;
}
function packCreate(decimals, name, symbol, uri) {
  const parts = [Buffer.from([0, decimals & 0xff])];
  for (const s of [name, symbol, uri]) {
    const b = Buffer.from(String(s), 'utf8');
    const len = Buffer.alloc(4);
    len.writeUInt32LE(b.length, 0);
    parts.push(len, b);
  }
  return Buffer.concat(parts);
}

// ----------------------------------------------------------- helpers --------
function pkOf(web3, x) { return new web3.PublicKey(x); }

function rpcUrl() { return process.env.RPC_URL || 'https://api.mainnet-beta.solana.com'; }

async function getConn(web3) {
  return new web3.Connection(rpcUrl(), 'confirmed');
}

// Build a v0 (versioned) tx from a list of legacy TransactionInstructions.
async function buildV0(web3, conn, payer, instructions, lookupTables = []) {
  const { blockhash } = await conn.getLatestBlockhash('confirmed');
  const msg = new web3.TransactionMessage({
    payerKey: payer,
    recentBlockhash: blockhash,
    instructions,
  }).compileToV0Message(lookupTables);
  return new web3.VersionedTransaction(msg);
}

function txToBase64(tx) {
  return Buffer.from(tx.serialize()).toString('base64');
}

// --------------------------------------------------------- Jupiter ----------
async function jupQuote({ inputMint, outputMint, amount, slippageBps = 100 }) {
  const url = `${JUP_BASE}/quote?inputMint=${inputMint}&outputMint=${outputMint}` +
    `&amount=${amount}&slippageBps=${slippageBps}&restrictIntermediateTokens=true`;
  const r = await fetch(url, { headers: { accept: 'application/json' } });
  if (!r.ok) throw new Error(`jup quote ${r.status}: ${await r.text()}`);
  return r.json();
}

// Returns a base64 VersionedTransaction (Jupiter builds the whole swap tx).
async function jupSwapTx({ quoteResponse, userPublicKey }) {
  const r = await fetch(`${JUP_BASE}/swap`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', accept: 'application/json' },
    body: JSON.stringify({
      quoteResponse,
      userPublicKey,
      wrapAndUnwrapSol: true,
      dynamicComputeUnitLimit: true,
      prioritizationFeeLamports: 'auto',
    }),
  });
  if (!r.ok) throw new Error(`jup swap ${r.status}: ${await r.text()}`);
  const j = await r.json();
  if (!j.swapTransaction) throw new Error('jup swap: no swapTransaction');
  return j.swapTransaction; // already base64 of a VersionedTransaction
}

// ------------------------------------------------------- AMM adapters -------
// Each adapter exposes:
//   findPool(ctx, mintA, mintB) -> { poolId, lpMint, lpProgram, ... } | null
//   buildDeposit(ctx, pool, owner, amountA, amountB) -> { instructions, lpMint, estLp }
//   buildWithdraw(ctx, pool, owner, lpAmount) -> { instructions }
// Returning null from findPool lets the caller fall through to the next AMM.

// --- Meteora dynamic-amm -----------------------------------------------------
const meteora = {
  name: 'meteora',
  async findPool(ctx, mintA, mintB) {
    // Meteora exposes a public pool search API. Use it to locate a pool for the
    // pair without needing on-chain scans.
    try {
      const r = await fetch(
        `https://amm-v2.meteora.ag/pools/search?include_token_mints=${mintA}&include_token_mints=${mintB}`,
        { headers: { accept: 'application/json' } });
      if (!r.ok) return null;
      const j = await r.json();
      const list = Array.isArray(j) ? j : (j.data || []);
      const hit = list.find((p) => {
        const mints = p.pool_token_mints || p.token_mints || [];
        return mints.includes(mintA) && mints.includes(mintB);
      });
      if (!hit) return null;
      return {
        poolId: hit.pool_address || hit.address || hit.pool_id,
        lpMint: hit.lp_mint || hit.pool_token_mint,
        lpProgram: 'spl-token',
      };
    } catch (_) { return null; }
  },
  async buildDeposit(ctx, pool, owner, amountA, amountB) {
    const { web3 } = ctx;
    const AmmImpl = require('@meteora-ag/dynamic-amm-sdk').default;
    const amm = await AmmImpl.create(ctx.conn, new web3.PublicKey(pool.poolId));
    // Quote the deposit; tolerate field-name differences across SDK versions.
    const quote = amm.getDepositQuote(
      new (require('bn.js'))(String(amountA)),
      new (require('bn.js'))(String(amountB)),
      true, // balanced
      0.5,  // 0.5% slippage
    );
    const tx = await amm.deposit(
      new web3.PublicKey(owner),
      quote.tokenAInAmount,
      quote.tokenBInAmount,
      quote.poolTokenAmountOut,
    );
    return {
      instructions: tx.instructions,
      lpMint: amm.address ? amm.poolState.lpMint.toBase58() : pool.lpMint,
      estLp: quote.poolTokenAmountOut ? quote.poolTokenAmountOut.toString() : '0',
    };
  },
  async buildWithdraw(ctx, pool, owner, lpAmount) {
    const { web3 } = ctx;
    const AmmImpl = require('@meteora-ag/dynamic-amm-sdk').default;
    const amm = await AmmImpl.create(ctx.conn, new web3.PublicKey(pool.poolId));
    const BN = require('bn.js');
    const quote = amm.getWithdrawQuote(new BN(String(lpAmount)), 0.5);
    const tx = await amm.withdraw(
      new web3.PublicKey(owner),
      new BN(String(lpAmount)),
      quote.tokenAOutAmount,
      quote.tokenBOutAmount,
    );
    return { instructions: tx.instructions };
  },
};

// --- Raydium CPMM ------------------------------------------------------------
const raydiumCpmm = {
  name: 'raydium-cpmm',
  async findPool(ctx, mintA, mintB) {
    // Use Raydium's public pool API to find a CPMM pool for the pair.
    try {
      const r = await fetch(
        `https://api-v3.raydium.io/pools/info/mint?mint1=${mintA}&mint2=${mintB}` +
        `&poolType=standard&poolSortField=liquidity&sortType=desc&pageSize=20&page=1`,
        { headers: { accept: 'application/json' } });
      if (!r.ok) return null;
      const j = await r.json();
      const list = (j.data && j.data.data) || [];
      // CPMM pools are programId Cpmm... ; "standard" covers CPMM + v4.
      // Match ONLY real CPMM pools by program id — Raydium v4 pools are also
      // type "Standard" and must go to the v4 adapter, not be loaded as CPMM.
      const hit = list.find((p) => p.programId === 'CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C');
      if (!hit) return null;
      return {
        poolId: hit.id,
        lpMint: hit.lpMint && hit.lpMint.address,
        lpProgram: 'spl-token',
      };
    } catch (_) { return null; }
  },
  async buildDeposit(ctx, pool, owner, amountA, amountB) {
    const { web3 } = ctx;
    const { Raydium, Percent } = require('@raydium-io/raydium-sdk-v2');
    const BN = require('bn.js');
    const raydium = await Raydium.load({
      connection: ctx.conn,
      owner: new web3.PublicKey(owner),
      cluster: 'mainnet', disableLoadToken: true,
    });
    const data = await raydium.cpmm.getPoolInfoFromRpc(pool.poolId);
    const { poolInfo, poolKeys } = data;
    const res = await raydium.cpmm.addLiquidity({
      poolInfo, poolKeys,
      inputAmount: new BN(String(amountA)),
      baseIn: true,
      slippage: new Percent(1, 100),
      txVersion: 0,
    });
    const tx = res.transaction || (res.builder && (await res.builder.build()).transaction);
    const instructions = res.instructions ||
      (res.builder && res.builder.allInstructions) || [];
    return {
      instructions: instructions.length ? instructions : (tx && tx.message ? [] : []),
      lpMint: poolInfo.lpMint && poolInfo.lpMint.address,
      estLp: '0',
      _tx: tx,
    };
  },
  async buildWithdraw(ctx, pool, owner, lpAmount) {
    const { web3 } = ctx;
    const { Raydium, Percent } = require('@raydium-io/raydium-sdk-v2');
    const BN = require('bn.js');
    const raydium = await Raydium.load({
      connection: ctx.conn,
      owner: new web3.PublicKey(owner),
      cluster: 'mainnet', disableLoadToken: true,
    });
    const data = await raydium.cpmm.getPoolInfoFromRpc(pool.poolId);
    const { poolInfo, poolKeys } = data;
    const res = await raydium.cpmm.withdrawLiquidity({
      poolInfo, poolKeys,
      lpAmount: new BN(String(lpAmount)),
      slippage: new Percent(1, 100),
      txVersion: 0,
    });
    const instructions = res.instructions ||
      (res.builder && res.builder.allInstructions) || [];
    return { instructions };
  },
};

// --- Raydium AMM v4 ----------------------------------------------------------
const RAYDIUM_V4_PROGRAM = '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8';
const raydiumV4 = {
  name: 'raydium-v4',
  async findPool(ctx, mintA, mintB) {
    try {
      const r = await fetch(
        `https://api-v3.raydium.io/pools/info/mint?mint1=${mintA}&mint2=${mintB}` +
        `&poolType=standard&poolSortField=liquidity&sortType=desc&pageSize=20&page=1`,
        { headers: { accept: 'application/json' } });
      if (!r.ok) return null;
      const j = await r.json();
      const list = (j.data && j.data.data) || [];
      const hit = list.find((p) => p.programId === RAYDIUM_V4_PROGRAM);
      if (!hit) return null;
      return { poolId: hit.id, lpMint: hit.lpMint && hit.lpMint.address, lpProgram: 'spl-token' };
    } catch (_) { return null; }
  },
  async buildDeposit(ctx, pool, owner, amountA, amountB) {
    const { web3 } = ctx;
    const { Raydium, Percent } = require('@raydium-io/raydium-sdk-v2');
    const BN = require('bn.js');
    const raydium = await Raydium.load({ connection: ctx.conn, owner: new web3.PublicKey(owner), cluster: 'mainnet', disableLoadToken: true });
    const { poolInfo, poolKeys } = await raydium.liquidity.getPoolInfoFromRpc({ poolId: pool.poolId });
    const res = await raydium.liquidity.addLiquidity({
      poolInfo, poolKeys,
      amountInA: new BN(String(amountA)),
      amountInB: new BN(String(amountB)),
      otherAmountMin: new BN(0),
      fixedSide: 'a',
      txVersion: 0,
    });
    const instructions = res.instructions || (res.builder && res.builder.allInstructions) || [];
    return { instructions, lpMint: poolInfo.lpMint && poolInfo.lpMint.address, estLp: '0', _tx: res.transaction };
  },
  async buildWithdraw(ctx, pool, owner, lpAmount) {
    const { web3 } = ctx;
    const { Raydium, Percent } = require('@raydium-io/raydium-sdk-v2');
    const BN = require('bn.js');
    const raydium = await Raydium.load({ connection: ctx.conn, owner: new web3.PublicKey(owner), cluster: 'mainnet', disableLoadToken: true });
    const { poolInfo, poolKeys } = await raydium.liquidity.getPoolInfoFromRpc({ poolId: pool.poolId });
    const res = await raydium.liquidity.removeLiquidity({
      poolInfo, poolKeys, amountIn: new BN(String(lpAmount)), txVersion: 0,
    });
    const instructions = res.instructions || (res.builder && res.builder.allInstructions) || [];
    return { instructions };
  },
};

// --- PumpSwap ----------------------------------------------------------------
const PUMP_PROGRAM = 'pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA';
const pumpSwap = {
  name: 'pumpswap',
  async findPool(ctx, mintA, mintB) {
    const { web3 } = ctx;
    const PUMP = new web3.PublicKey(PUMP_PROGRAM);
    // Pump Pool: base_mint@43, quote_mint@75, lp_mint@107. Try both orderings.
    for (const [base, quote] of [[mintA, mintB], [mintB, mintA]]) {
      try {
        const accts = await ctx.conn.getProgramAccounts(PUMP, {
          filters: [{ memcmp: { offset: 43, bytes: base } }, { memcmp: { offset: 75, bytes: quote } }],
          dataSlice: { offset: 107, length: 32 },
        });
        if (accts.length) {
          const lpMint = new web3.PublicKey(accts[0].account.data).toBase58();
          // baseIsA records whether mintA is the pool's BASE mint, so buildDeposit
          // can map amountA/amountB to the base/quote sides correctly.
          return { poolId: accts[0].pubkey.toBase58(), lpMint, lpProgram: 'spl-token', base, quote, baseIsA: base === mintA };
        }
      } catch (_) {}
    }
    return null;
  },
  async buildDeposit(ctx, pool, owner, amountA, amountB) {
    const { web3 } = ctx;
    const { OnlinePumpAmmSdk, PumpAmmSdk } = require('@pump-fun/pump-swap-sdk');
    const BN = require('bn.js');
    const online = new OnlinePumpAmmSdk(ctx.conn);
    const ownerPk = new web3.PublicKey(owner);
    const state = await online.liquiditySolanaState(new web3.PublicKey(pool.poolId), ownerPk);
    const pump = new PumpAmmSdk();
    // Deposit amount for the pool's BASE mint (amountA maps to mintA).
    const base = new BN(String(pool.baseIsA === false ? amountB : amountA));
    // depositBaseInput returns { quote, lpToken, maxBase, maxQuote }; slippage is a percent.
    const dep = pump.depositBaseInput(state, base, 1);
    const instructions = await pump.depositInstructionsInternal(state, dep.lpToken, dep.maxBase, dep.maxQuote);
    return { instructions, lpMint: pool.lpMint, estLp: dep.lpToken ? dep.lpToken.toString() : '0' };
  },
  async buildWithdraw(ctx, pool, owner, lpAmount) {
    const { web3 } = ctx;
    const { OnlinePumpAmmSdk, PumpAmmSdk } = require('@pump-fun/pump-swap-sdk');
    const BN = require('bn.js');
    const online = new OnlinePumpAmmSdk(ctx.conn);
    const ownerPk = new web3.PublicKey(owner);
    const state = await online.liquiditySolanaState(new web3.PublicKey(pool.poolId), ownerPk);
    const pump = new PumpAmmSdk();
    // withdrawInstructionsInternal(state, lpTokenAmountIn, minBaseOut, minQuoteOut)
    const instructions = await pump.withdrawInstructionsInternal(state, new BN(String(lpAmount)), new BN(0), new BN(0));
    return { instructions };
  },
};

// Fallback order: deepest/most-reliable liquidity first.
const AMMS = [meteora, raydiumCpmm, raydiumV4, pumpSwap];

async function findAmm(ctx, mintA, mintB) {
  for (const amm of AMMS) {
    let pool = null;
    try { pool = await amm.findPool(ctx, mintA, mintB); } catch (_) { pool = null; }
    if (pool && pool.poolId) return { amm, pool };
  }
  return null;
}

// ----------------------------------------------------- wrap ix builder ------
// Mirrors the client `buildWrap`: returns ATA-idempotent ix + the Wrap ix.
function buildWrapInstructions(ctx, owner, wrappedMint, lpMint, pool, amount, creator, escrows, lpProgram) {
  const { web3, splToken } = ctx;
  const programId = ctx.programId;
  const wtp = splToken.TOKEN_2022_PROGRAM_ID;
  const lpProg = lpProgram || splToken.TOKEN_PROGRAM_ID; // actual LP token program
  const auth = authorityPda(web3, programId, wrappedMint);
  const cfg = configPda(web3, programId, wrappedMint);
  const ata = (mint, ownerPk, prog, allowOff) =>
    splToken.getAssociatedTokenAddressSync(mint, ownerPk, !!allowOff, prog);

  const recipient = ata(wrappedMint, owner, wtp, true);
  const sourceLp = ata(lpMint, owner, lpProg, false);
  const escrow = ata(lpMint, auth, lpProg, true);
  const creatorShare = ata(wrappedMint, new web3.PublicKey(creator), wtp, true);
  const deployerShare = ata(wrappedMint, ctx.deployer, wtp, true);

  const others = (escrows || [])
    .filter((e) => e.lpMint !== lpMint.toBase58())
    .map((e) => ata(new web3.PublicKey(e.lpMint), auth,
      e.program === 'token-2022' ? wtp : lpProg, true));

  const keys = [
    { pubkey: recipient, isSigner: false, isWritable: true },
    { pubkey: wrappedMint, isSigner: false, isWritable: true },
    { pubkey: auth, isSigner: false, isWritable: false },
    { pubkey: cfg, isSigner: false, isWritable: true },
    { pubkey: wtp, isSigner: false, isWritable: false },
    { pubkey: lpProg, isSigner: false, isWritable: false },
    { pubkey: sourceLp, isSigner: false, isWritable: true },
    { pubkey: lpMint, isSigner: false, isWritable: false },
    { pubkey: escrow, isSigner: false, isWritable: true },
    { pubkey: new web3.PublicKey(pool), isSigner: false, isWritable: false },
    { pubkey: owner, isSigner: true, isWritable: false },
    { pubkey: creatorShare, isSigner: false, isWritable: true },
    { pubkey: deployerShare, isSigner: false, isWritable: true },
    ...others.map((o) => ({ pubkey: o, isSigner: false, isWritable: true })),
  ];

  const mk = (addr, ownerPk, mint, prog) =>
    splToken.createAssociatedTokenAccountIdempotentInstruction(owner, addr, ownerPk, mint, prog);

  return [
    mk(recipient, owner, wrappedMint, wtp),
    mk(creatorShare, new web3.PublicKey(creator), wrappedMint, wtp),
    mk(deployerShare, ctx.deployer, wrappedMint, wtp),
    mk(escrow, auth, lpMint, lpProg),
    new web3.TransactionInstruction({ programId, keys, data: packWrap(amount) }),
  ];
}

function buildUnwrapInstructions(ctx, owner, wrappedMint, lpMint, pool, shares, creator, escrows) {
  const { web3, splToken } = ctx;
  const programId = ctx.programId;
  const wtp = splToken.TOKEN_2022_PROGRAM_ID;
  const lpProg = splToken.TOKEN_PROGRAM_ID;
  const auth = authorityPda(web3, programId, wrappedMint);
  const cfg = configPda(web3, programId, wrappedMint);
  const ata = (mint, ownerPk, prog, allowOff) =>
    splToken.getAssociatedTokenAddressSync(mint, ownerPk, !!allowOff, prog);

  const sourceShare = ata(wrappedMint, owner, wtp, true);
  const destLp = ata(lpMint, owner, lpProg, false);
  const escrow = ata(lpMint, auth, lpProg, true);
  const creatorShare = ata(wrappedMint, new web3.PublicKey(creator), wtp, true);
  const deployerShare = ata(wrappedMint, ctx.deployer, wtp, true);

  const others = (escrows || [])
    .filter((e) => e.lpMint !== lpMint.toBase58())
    .map((e) => ata(new web3.PublicKey(e.lpMint), auth,
      e.program === 'token-2022' ? wtp : lpProg, true));

  const keys = [
    { pubkey: sourceShare, isSigner: false, isWritable: true },
    { pubkey: wrappedMint, isSigner: false, isWritable: true },
    { pubkey: auth, isSigner: false, isWritable: false },
    { pubkey: cfg, isSigner: false, isWritable: true },
    { pubkey: wtp, isSigner: false, isWritable: false },
    { pubkey: lpProg, isSigner: false, isWritable: false },
    { pubkey: destLp, isSigner: false, isWritable: true },
    { pubkey: lpMint, isSigner: false, isWritable: false },
    { pubkey: escrow, isSigner: false, isWritable: true },
    { pubkey: owner, isSigner: true, isWritable: false },
    { pubkey: creatorShare, isSigner: false, isWritable: true },
    { pubkey: deployerShare, isSigner: false, isWritable: true },
    ...others.map((o) => ({ pubkey: o, isSigner: false, isWritable: true })),
  ];

  const mk = (addr, ownerPk, mint, prog) =>
    splToken.createAssociatedTokenAccountIdempotentInstruction(owner, addr, ownerPk, mint, prog);

  return [
    mk(destLp, owner, lpMint, lpProg),
    new web3.TransactionInstruction({ programId, keys, data: packUnwrap(shares) }),
  ];
}

// CreatePairMint ix (used when the wrapped pair doesn't exist yet).
function buildCreatePairMintInstructions(ctx, owner, mintA, mintB, decimals, name, symbol, uri) {
  const { web3, splToken } = ctx;
  const programId = ctx.programId;
  const wtp = splToken.TOKEN_2022_PROGRAM_ID;
  const [x, y] = sortPair(web3, mintA, mintB);
  const wm = wrappedMintPda(web3, programId, mintA, mintB, wtp);
  const auth = authorityPda(web3, programId, wm);
  const cfg = configPda(web3, programId, wm);
  const sys = new web3.PublicKey('11111111111111111111111111111111');
  const fundMint = web3.SystemProgram.transfer({ fromPubkey: owner, toPubkey: wm, lamports: 20_000_000 });
  const fundCfg = web3.SystemProgram.transfer({ fromPubkey: owner, toPubkey: cfg, lamports: 6_000_000 });
  const keys = [
    { pubkey: owner, isSigner: true, isWritable: true },
    { pubkey: wm, isSigner: false, isWritable: true },
    { pubkey: cfg, isSigner: false, isWritable: true },
    { pubkey: auth, isSigner: false, isWritable: false },
    { pubkey: x, isSigner: false, isWritable: false },
    { pubkey: y, isSigner: false, isWritable: false },
    { pubkey: sys, isSigner: false, isWritable: false },
    { pubkey: wtp, isSigner: false, isWritable: false },
  ];
  const ix = new web3.TransactionInstruction({
    programId, keys, data: packCreate(decimals, name, symbol, uri),
  });
  return [fundMint, fundCfg, ix];
}

// Does a PairConfig already exist for this wrapped mint?
async function pairExists(ctx, wrappedMint) {
  try {
    const cfg = configPda(ctx.web3, ctx.programId, wrappedMint);
    const info = await ctx.conn.getAccountInfo(cfg, 'confirmed');
    return !!info;
  } catch (_) { return false; }
}

// Read on-chain escrows for a wrapped pair (for the Wrap "other escrows" tail).
async function readEscrows(ctx, wrappedMint) {
  const { web3, splToken } = ctx;
  try {
    const cfg = configPda(web3, ctx.programId, wrappedMint);
    const info = await ctx.conn.getAccountInfo(cfg, 'confirmed');
    if (!info) return [];
    const data = info.data;
    const MAX = 8;
    const count = Number(data.readBigUInt64LE(128));
    const auth = authorityPda(web3, ctx.programId, wrappedMint);
    const out = [];
    for (let i = 0; i < count && i < MAX; i++) {
      const lp = new web3.PublicKey(data.subarray(136 + i * 32, 168 + i * 32));
      out.push({ lpMint: lp.toBase58(), program: 'spl-token' });
    }
    return out;
  } catch (_) { return []; }
}

// --------------------------------------------------------- endpoints --------
function makeCtx(web3, splToken, conn) {
  return {
    web3, splToken, conn,
    programId: new web3.PublicKey(process.env.PROGRAM_ID || PROGRAM_ID_DEFAULT),
    deployer: new web3.PublicKey(process.env.DEPLOYER || DEPLOYER_DEFAULT),
  };
}

// POST /api/zap/quote {mintA, mintB, solLamports}
//   -> {expectedA, expectedB, amm, estLp, estShares, route}
async function quote(req, res) {
  try {
    const { web3, splToken } = lazy();
    const { mintA, mintB, solLamports } = req.body || {};
    if (!mintA || !mintB || !solLamports) {
      return res.status(400).json({ error: 'mintA, mintB, solLamports required' });
    }
    const conn = await getConn(web3);
    const ctx = makeCtx(web3, splToken, conn);

    const half = Math.floor(Number(solLamports) / 2);
    const otherHalf = Number(solLamports) - half;

    // Jupiter quotes for each non-wSOL side.
    let expectedA = null, expectedB = null, routeA = null, routeB = null;
    if (mintA !== WSOL) {
      const q = await jupQuote({ inputMint: WSOL, outputMint: mintA, amount: half });
      expectedA = q.outAmount; routeA = (q.routePlan || []).map((r) => r.swapInfo && r.swapInfo.label);
    } else { expectedA = String(half); }
    if (mintB !== WSOL) {
      const q = await jupQuote({ inputMint: WSOL, outputMint: mintB, amount: otherHalf });
      expectedB = q.outAmount; routeB = (q.routePlan || []).map((r) => r.swapInfo && r.swapInfo.label);
    } else { expectedB = String(otherHalf); }

    // Locate a supported AMM pool.
    const found = await findAmm(ctx, mintA, mintB);

    res.json({
      expectedA, expectedB,
      amm: found ? found.amm.name : null,
      pool: found ? found.pool.poolId : null,
      estLp: null,     // exact LP depends on live reserves at deposit time
      estShares: null, // shares depend on vault NAV at wrap time
      route: { a: routeA, b: routeB },
      note: found ? undefined : 'no supported (Meteora/Raydium-CPMM) pool found for pair',
    });
  } catch (e) {
    res.status(500).json({ error: String(e.message || e) });
  }
}

// POST /api/zap/in {owner, mintA, mintB, solLamports} -> ordered steps
async function zapIn(req, res) {
  try {
    const { web3, splToken } = lazy();
    const { owner, mintA, mintB, solLamports } = req.body || {};
    if (!owner || !mintA || !mintB || !solLamports) {
      return res.status(400).json({ error: 'owner, mintA, mintB, solLamports required' });
    }
    const conn = await getConn(web3);
    const ctx = makeCtx(web3, splToken, conn);
    const ownerPk = new web3.PublicKey(owner);
    const mA = new web3.PublicKey(mintA);
    const mB = new web3.PublicKey(mintB);

    const steps = [];
    const half = Math.floor(Number(solLamports) / 2);
    const otherHalf = Number(solLamports) - half;

    // 1) Jupiter swaps SOL -> mintA / mintB (skip a wSOL side). Jupiter returns a
    //    fully-built versioned tx (base64) for each.
    if (mintA !== WSOL) {
      const q = await jupQuote({ inputMint: WSOL, outputMint: mintA, amount: half });
      const txB64 = await jupSwapTx({ quoteResponse: q, userPublicKey: owner });
      steps.push({ label: `Swap SOL -> A (${mintA.slice(0, 4)}…)`, txBase64: txB64 });
    }
    if (mintB !== WSOL) {
      const q = await jupQuote({ inputMint: WSOL, outputMint: mintB, amount: otherHalf });
      const txB64 = await jupSwapTx({ quoteResponse: q, userPublicKey: owner });
      steps.push({ label: `Swap SOL -> B (${mintB.slice(0, 4)}…)`, txBase64: txB64 });
    }

    // 2) Add liquidity on a supported AMM. The deposit amounts are read by the
    //    CLIENT between steps (balances change after the swaps), so the client
    //    must re-request /api/zap/deposit with the actual amounts. To keep this
    //    endpoint self-contained we build the deposit using the *expected* swap
    //    outputs from a fresh quote; the client can re-read and re-build via the
    //    /api/zap/deposit helper for exactness.
    const found = await findAmm(ctx, mintA, mintB);
    if (!found) {
      return res.json({
        steps,
        warning: 'No supported AMM pool (Meteora/Raydium CPMM) for this pair. ' +
          'Swaps were built; add-liquidity + wrap steps are unavailable.',
        amm: null,
      });
    }

    res.json({
      steps,
      amm: found.amm.name,
      pool: found.pool.poolId,
      lpMint: found.pool.lpMint || null,
      next: {
        deposit: '/api/zap/deposit',
        wrap: '/api/zap/wrap',
        note: 'After the swaps confirm, re-read token balances and call ' +
          '/api/zap/deposit {owner,mintA,mintB,amountA,amountB} then ' +
          '/api/zap/wrap {owner,mintA,mintB,lpAmount} -- this keeps the flow ' +
          'stepwise so deposit/wrap use real post-swap amounts.',
      },
    });
  } catch (e) {
    res.status(500).json({ error: String(e.message || e) });
  }
}

// POST /api/zap/deposit {owner, mintA, mintB, amountA, amountB} -> {steps}
async function deposit(req, res) {
  try {
    const { web3, splToken } = lazy();
    const { owner, mintA, mintB, amountA, amountB } = req.body || {};
    if (!owner || !mintA || !mintB || amountA == null || amountB == null) {
      return res.status(400).json({ error: 'owner, mintA, mintB, amountA, amountB required' });
    }
    const conn = await getConn(web3);
    const ctx = makeCtx(web3, splToken, conn);
    const found = await findAmm(ctx, mintA, mintB);
    if (!found) return res.status(404).json({ error: 'no supported AMM pool for pair' });

    const built = await found.amm.buildDeposit(ctx, found.pool, owner, amountA, amountB);
    let txB64;
    if (built._tx) {
      txB64 = built._tx.serialize ? Buffer.from(built._tx.serialize()).toString('base64') : null;
    }
    if (!txB64) {
      const tx = await buildV0(web3, conn, new web3.PublicKey(owner), built.instructions);
      txB64 = txToBase64(tx);
    }
    res.json({
      steps: [{ label: `Add liquidity on ${found.amm.name}`, txBase64: txB64 }],
      lpMint: built.lpMint, estLp: built.estLp, amm: found.amm.name,
    });
  } catch (e) {
    res.status(500).json({ error: String(e.message || e) });
  }
}

// POST /api/zap/wrap {owner, mintA, mintB, lpMint, lpAmount, name, symbol, uri}
async function wrap(req, res) {
  try {
    const { web3, splToken } = lazy();
    const { owner, mintA, mintB, lpAmount } = req.body || {};
    let { lpMint } = req.body || {};
    if (!owner || !mintA || !mintB || lpAmount == null) {
      return res.status(400).json({ error: 'owner, mintA, mintB, lpAmount required' });
    }
    const conn = await getConn(web3);
    const ctx = makeCtx(web3, splToken, conn);
    const ownerPk = new web3.PublicKey(owner);
    const mA = new web3.PublicKey(mintA);
    const mB = new web3.PublicKey(mintB);
    const wtp = splToken.TOKEN_2022_PROGRAM_ID;
    const wm = wrappedMintPda(web3, ctx.programId, mA, mB, wtp);

    if (!lpMint) {
      const found = await findAmm(ctx, mintA, mintB);
      if (!found || !found.pool.lpMint) {
        return res.status(400).json({ error: 'lpMint required (could not infer from AMM)' });
      }
      lpMint = found.pool.lpMint;
    }
    const found = await findAmm(ctx, mintA, mintB);
    const poolId = found ? found.pool.poolId : (req.body.pool || lpMint);

    const steps = [];

    // CreatePairMint first if the wrapped pair doesn't exist yet.
    const exists = await pairExists(ctx, wm);
    if (!exists) {
      const name = req.body.name || 'Zapped wLP';
      const symbol = req.body.symbol || 'wLP';
      const uri = req.body.uri || '';
      const createIx = buildCreatePairMintInstructions(ctx, ownerPk, mA, mB, 9, name, symbol, uri);
      const tx = await buildV0(web3, conn, ownerPk, createIx);
      steps.push({ label: 'Create wrapped pair mint', txBase64: txToBase64(tx) });
    }

    const escrows = await readEscrows(ctx, wm);
    const creator = exists
      ? await readCreator(ctx, wm).catch(() => owner)
      : owner;
    // Detect the LP mint's actual token program (SPL Token vs Token-2022) so the
    // wrap's CPIs use the correct program id (else InstructionError IncorrectProgramId).
    let lpProgram = splToken.TOKEN_PROGRAM_ID;
    try { const li = await conn.getAccountInfo(new web3.PublicKey(lpMint)); if (li && li.owner) lpProgram = li.owner; } catch (_) {}
    const wrapIx = buildWrapInstructions(
      ctx, ownerPk, wm, new web3.PublicKey(lpMint), poolId, lpAmount, creator, escrows, lpProgram);
    const tx = await buildV0(web3, conn, ownerPk, wrapIx);
    steps.push({ label: 'Wrap LP -> shares', txBase64: txToBase64(tx) });

    res.json({ steps, wrappedMint: wm.toBase58(), lpMint });
  } catch (e) {
    res.status(500).json({ error: String(e.message || e) });
  }
}

async function readCreator(ctx, wrappedMint) {
  const cfg = configPda(ctx.web3, ctx.programId, wrappedMint);
  const info = await ctx.conn.getAccountInfo(cfg, 'confirmed');
  if (!info) throw new Error('no config');
  return new ctx.web3.PublicKey(info.data.subarray(96, 128)).toBase58();
}

// POST /api/zap/out {owner, wrappedMint, shares} -> ordered steps
async function zapOut(req, res) {
  try {
    const { web3, splToken } = lazy();
    const { owner, wrappedMint, shares } = req.body || {};
    if (!owner || !wrappedMint || shares == null) {
      return res.status(400).json({ error: 'owner, wrappedMint, shares required' });
    }
    const conn = await getConn(web3);
    const ctx = makeCtx(web3, splToken, conn);
    const ownerPk = new web3.PublicKey(owner);
    const wm = new web3.PublicKey(wrappedMint);

    // Read config to learn (mintA, mintB), lp mint(s), and creator.
    const cfg = configPda(web3, ctx.programId, wm);
    const info = await conn.getAccountInfo(cfg, 'confirmed');
    if (!info) return res.status(404).json({ error: 'wrapped pair config not found' });
    const data = info.data;
    const mintA = new web3.PublicKey(data.subarray(0, 32)).toBase58();
    const mintB = new web3.PublicKey(data.subarray(32, 64)).toBase58();
    const creator = new web3.PublicKey(data.subarray(96, 128)).toBase58();
    const lpCount = Number(data.readBigUInt64LE(128));
    const lpMints = [];
    for (let i = 0; i < lpCount && i < 8; i++) {
      lpMints.push(new web3.PublicKey(data.subarray(136 + i * 32, 168 + i * 32)).toBase58());
    }
    if (!lpMints.length) return res.status(400).json({ error: 'no registered LP mint to unwrap' });
    const lpMint = lpMints[0];

    const escrows = lpMints.map((m) => ({ lpMint: m, program: 'spl-token' }));
    const found = await findAmm(ctx, mintA, mintB);
    const poolId = found ? found.pool.poolId : lpMint;

    const steps = [];

    // 1) Unwrap shares -> LP tokens.
    const unwrapIx = buildUnwrapInstructions(
      ctx, ownerPk, wm, new web3.PublicKey(lpMint), poolId, shares, creator, escrows);
    const tx1 = await buildV0(web3, conn, ownerPk, unwrapIx);
    steps.push({ label: 'Unwrap shares -> LP', txBase64: txToBase64(tx1) });

    res.json({
      steps,
      mintA, mintB, lpMint, amm: found ? found.amm.name : null,
      pool: poolId,
      next: {
        withdraw: '/api/zap/withdraw',
        swapOut: 'jupiter (client-side, A->SOL and B->SOL)',
        note: 'After Unwrap confirms, read your LP balance then call ' +
          '/api/zap/withdraw {owner,wrappedMint,lpAmount} to remove liquidity, ' +
          'then read mintA/mintB balances and call /api/zap/swapout to swap each ' +
          'back to SOL via Jupiter -- stepwise so amounts are real.',
      },
    });
  } catch (e) {
    res.status(500).json({ error: String(e.message || e) });
  }
}

// POST /api/zap/withdraw {owner, wrappedMint, lpAmount} -> {steps}
async function withdraw(req, res) {
  try {
    const { web3, splToken } = lazy();
    const { owner, wrappedMint, lpAmount } = req.body || {};
    if (!owner || !wrappedMint || lpAmount == null) {
      return res.status(400).json({ error: 'owner, wrappedMint, lpAmount required' });
    }
    const conn = await getConn(web3);
    const ctx = makeCtx(web3, splToken, conn);
    const wm = new web3.PublicKey(wrappedMint);
    const cfg = configPda(web3, ctx.programId, wm);
    const info = await conn.getAccountInfo(cfg, 'confirmed');
    if (!info) return res.status(404).json({ error: 'wrapped pair config not found' });
    const mintA = new web3.PublicKey(info.data.subarray(0, 32)).toBase58();
    const mintB = new web3.PublicKey(info.data.subarray(32, 64)).toBase58();

    const found = await findAmm(ctx, mintA, mintB);
    if (!found) return res.status(404).json({ error: 'no supported AMM pool for pair' });
    const built = await found.amm.buildWithdraw(ctx, found.pool, owner, lpAmount);
    const tx = await buildV0(web3, conn, new web3.PublicKey(owner), built.instructions);
    res.json({
      steps: [{ label: `Remove liquidity on ${found.amm.name}`, txBase64: txToBase64(tx) }],
      mintA, mintB, amm: found.amm.name,
    });
  } catch (e) {
    res.status(500).json({ error: String(e.message || e) });
  }
}

// POST /api/zap/swapout {owner, mint, amount} -> {steps} (mintX -> SOL via Jupiter)
async function swapOut(req, res) {
  try {
    const { owner, mint, amount } = req.body || {};
    if (!owner || !mint || amount == null) {
      return res.status(400).json({ error: 'owner, mint, amount required' });
    }
    if (mint === WSOL) return res.json({ steps: [] });
    const q = await jupQuote({ inputMint: mint, outputMint: WSOL, amount });
    const txB64 = await jupSwapTx({ quoteResponse: q, userPublicKey: owner });
    res.json({ steps: [{ label: `Swap ${mint.slice(0, 4)}… -> SOL`, txBase64: txB64 }], expectedSol: q.outAmount });
  } catch (e) {
    res.status(500).json({ error: String(e.message || e) });
  }
}

// ----------------------------------------------------------- Jito -----------
const JITO_BE = process.env.JITO_BE || 'https://mainnet.block-engine.jito.wtf/api/v1/bundles';
const JITO_TIPS = [
  '96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5','HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe',
  'Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY','ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt6iGPaS49',
  'DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh','ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt',
  'DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL','3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT',
];
function jitoTipIx(web3, ownerPk, lamports) {
  const tip = JITO_TIPS[Math.floor(Math.random() * JITO_TIPS.length)];
  return web3.SystemProgram.transfer({ fromPubkey: ownerPk, toPubkey: new web3.PublicKey(tip), lamports });
}
// Wrap native SOL into the owner's WSOL ATA (Jupiter can't swap SOL->SOL).
function wrapSolIxs(ctx, ownerPk, lamports) {
  const { web3, splToken } = ctx;
  const wsol = new web3.PublicKey(WSOL);
  const ata = splToken.getAssociatedTokenAddressSync(wsol, ownerPk, true);
  return [
    splToken.createAssociatedTokenAccountIdempotentInstruction(ownerPk, ata, ownerPk, wsol),
    web3.SystemProgram.transfer({ fromPubkey: ownerPk, toPubkey: ata, lamports }),
    splToken.createSyncNativeInstruction(ata),
  ];
}

// POST /api/zap/bundle {owner, mintA, mintB, solLamports, tipLamports?}
// Returns the unsigned txs for the ATOMIC part (WSOL-wrap / Jupiter swaps +
// AMM deposit + Jito tip). The client signs them with ONE signAllTransactions
// and submits via /api/zap/submit-bundle. The wrap (which needs the realized LP
// balance) is a separate quick step afterwards.
async function zapBundle(req, res) {
  try {
    const { web3, splToken } = lazy();
    const { owner, mintA, mintB, solLamports, tipLamports } = req.body || {};
    if (!owner || !mintA || !mintB || !solLamports) return res.status(400).json({ error: 'owner, mintA, mintB, solLamports required' });
    const conn = await getConn(web3);
    const ctx = makeCtx(web3, splToken, conn);
    const ownerPk = new web3.PublicKey(owner);
    const half = Math.floor(Number(solLamports) / 2), otherHalf = Number(solLamports) - half;

    const found = await findAmm(ctx, mintA, mintB);
    if (!found) return res.status(400).json({ error: 'no supported AMM pool (Meteora / Raydium CPMM / v4 / Pump) for this pair' });

    const txs = []; let amountA, amountB;
    // Leg A
    if (mintA === WSOL) { txs.push({ label: 'Wrap SOL (A)', tx: await buildV0(web3, conn, ownerPk, wrapSolIxs(ctx, ownerPk, half)) }); amountA = String(half); }
    else { const q = await jupQuote({ inputMint: WSOL, outputMint: mintA, amount: half }); txs.push({ label: 'Swap SOL→A', b64: await jupSwapTx({ quoteResponse: q, userPublicKey: owner }) }); amountA = q.otherAmountThreshold || q.outAmount; }
    // Leg B
    if (mintB === WSOL) { txs.push({ label: 'Wrap SOL (B)', tx: await buildV0(web3, conn, ownerPk, wrapSolIxs(ctx, ownerPk, otherHalf)) }); amountB = String(otherHalf); }
    else { const q = await jupQuote({ inputMint: WSOL, outputMint: mintB, amount: otherHalf }); txs.push({ label: 'Swap SOL→B', b64: await jupSwapTx({ quoteResponse: q, userPublicKey: owner }) }); amountB = q.otherAmountThreshold || q.outAmount; }
    // Deposit (uses conservative min-out amounts so it can't over-spend post-swap)
    // Deposit slightly less than the realized amounts so the ATOMIC deposit
    // can't fail simulation on a swap slippage shortfall (leaves tiny dust).
    const depA = Math.floor(Number(amountA) * 0.97).toString();
    const depB = Math.floor(Number(amountB) * 0.97).toString();
    const dep = await found.amm.buildDeposit(ctx, found.pool, owner, depA, depB);
    // tipLamports:0 -> no Jito tip (for the plain signAll + rapid-fire RPC path).
    const tipL = tipLamports !== undefined ? Number(tipLamports) : 500000;
    const depIxs = tipL > 0 ? [...dep.instructions, jitoTipIx(web3, ownerPk, tipL)] : dep.instructions;
    txs.push({ label: `Deposit ${found.amm.name}${tipL > 0 ? ' + tip' : ''}`, tx: await buildV0(web3, conn, ownerPk, depIxs) });

    res.json({
      steps: txs.map(t => ({ label: t.label, txBase64: t.b64 || txToBase64(t.tx) })),
      amm: found.amm.name, pool: found.pool.poolId, lpMint: found.pool.lpMint || (dep.lpMint),
    });
  } catch (e) { res.status(500).json({ error: String(e.message || e) }); }
}

// POST /api/zap/submit-bundle {signed: [base64,...]} -> Jito sendBundle
async function submitBundle(req, res) {
  try {
    const signed = (req.body && req.body.signed) || [];
    if (!signed.length) return res.status(400).json({ error: 'signed[] required' });
    const r = await fetch(JITO_BE, {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'sendBundle', params: [signed, { encoding: 'base64' }] }),
    });
    const j = await r.json();
    if (j.error) return res.status(502).json({ error: j.error.message || JSON.stringify(j.error) });
    res.json({ bundleId: j.result });
  } catch (e) { res.status(500).json({ error: String(e.message || e) }); }
}

function register(app) {
  app.post('/api/zap/quote', quote);
  app.post('/api/zap/in', zapIn);
  app.post('/api/zap/deposit', deposit);
  app.post('/api/zap/wrap', wrap);
  app.post('/api/zap/out', zapOut);
  app.post('/api/zap/withdraw', withdraw);
  app.post('/api/zap/swapout', swapOut);
  app.post('/api/zap/bundle', zapBundle);
  app.post('/api/zap/submit-bundle', submitBundle);
}

module.exports = { register };
