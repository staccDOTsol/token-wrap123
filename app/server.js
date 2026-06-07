'use strict';
// Off-chain NAV crank + API for the SPL LP Wrap program.
//
// Every POLL_MS it scans the program's PairConfig accounts, reconstructs each
// wrapped pair's NAV (= total normalized reserves / share supply) directly from
// on-chain state, and serves the current snapshot + a NAV time-series to the
// degen UI in public/.

const path = require('path');
const express = require('express');
const { Connection, PublicKey } = require('@solana/web3.js');
const { getAssociatedTokenAddressSync, TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID } = require('@solana/spl-token');

const PORT = parseInt(process.env.PORT || '8080', 10);
const RPC_URL = process.env.RPC_URL || 'https://api.devnet.solana.com';
const PROGRAM_ID = new PublicKey(process.env.PROGRAM_ID || 'EbmEELwtg3iqHdtNCKcRwZCKmWzGpF11ZTvc9sPxBQJB');
const CLUSTER = process.env.CLUSTER || 'devnet';
const POLL_MS = parseInt(process.env.POLL_MS || '20000', 10);

// PairConfig layout (repr(C), see lp-wrap/src/state.rs). LEN = 408.
const MAX_LP_MINTS = 8;
const PAIR_CONFIG_LEN = 32 * 4 + 8 + 32 * MAX_LP_MINTS + MAX_LP_MINTS + 1 + 7;
const SEED_MINT = Buffer.from('lp_mint');
const SEED_AUTH = Buffer.from('authority');

const conn = new Connection(RPC_URL, 'confirmed');

function sortPair(a, b) {
  return Buffer.compare(a.toBuffer(), b.toBuffer()) <= 0 ? [a, b] : [b, a];
}
function wrappedMintPda(mintA, mintB, wtp) {
  const [a, b] = sortPair(mintA, mintB);
  return PublicKey.findProgramAddressSync([SEED_MINT, a.toBuffer(), b.toBuffer(), wtp.toBuffer()], PROGRAM_ID)[0];
}
function authorityPda(wrappedMint) {
  return PublicKey.findProgramAddressSync([SEED_AUTH, wrappedMint.toBuffer()], PROGRAM_ID)[0];
}
function pk(buf, off) { return new PublicKey(buf.subarray(off, off + 32)); }

function parsePairConfig(data) {
  return {
    mintA: pk(data, 0),
    mintB: pk(data, 32),
    wrappedTokenProgram: pk(data, 64),
    creator: pk(data, 96),
    lpMintCount: Number(data.readBigUInt64LE(128)),
    lpMints: Array.from({ length: MAX_LP_MINTS }, (_, i) => pk(data, 136 + i * 32)),
    lpDecimals: Array.from({ length: MAX_LP_MINTS }, (_, i) => data[392 + i]),
    shareDecimals: data[400],
  };
}

function toCommon(amount, fromDec, commonDec) {
  amount = BigInt(amount);
  if (commonDec >= fromDec) return amount * (10n ** BigInt(commonDec - fromDec));
  return amount / (10n ** BigInt(fromDec - commonDec));
}

// Quote tokens that get dropped from the card title in favour of the long tail.
const QUOTES = {
  'So11111111111111111111111111111111111111112': 'SOL',
  'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v': 'USDC',
  'Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB': 'USDT',
};
const metaCache = new Map();

// Resolve token metadata (name/symbol/image) via Helius DAS getAssetBatch.
async function resolveMeta(mints) {
  const need = mints.filter((m) => !metaCache.has(m));
  if (need.length) {
    try {
      const r = await fetch(RPC_URL, {
        method: 'POST', headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ jsonrpc: '2.0', id: 'das', method: 'getAssetBatch', params: { ids: need } }),
      });
      const j = await r.json();
      for (const a of (j.result || [])) {
        if (!a || !a.id) continue;
        const c = a.content || {}, md = c.metadata || {};
        metaCache.set(a.id, {
          name: md.name || '', symbol: md.symbol || '',
          image: (c.links && c.links.image) || (c.files && c.files[0] && c.files[0].uri) || null,
        });
      }
    } catch (_) {}
    for (const m of need) if (!metaCache.has(m)) metaCache.set(m, { name: '', symbol: '', image: null });
  }
}

// "Drop SOL/USDC, feature the long tail; if neither is a quote, feature both."
function buildDisplay(a, b) {
  const nonQuote = [a, b].filter((t) => !t.quote);
  if (nonQuote.length === 1) return { featured: nonQuote, quote: a.quote || b.quote };
  return { featured: [a, b], quote: null };
}

const state = { pairs: [], history: {}, lastUpdate: 0, error: null };

async function readTokenAmount(addr) {
  try {
    const r = await conn.getTokenAccountBalance(addr, 'confirmed');
    return BigInt(r.value.amount);
  } catch (_) { return null; }
}

async function crank() {
  try {
    const accounts = await conn.getProgramAccounts(PROGRAM_ID, {
      filters: [{ dataSize: PAIR_CONFIG_LEN }],
    });
    const pairs = [];
    for (const { account } of accounts) {
      const cfg = parsePairConfig(account.data);
      const wrappedMint = wrappedMintPda(cfg.mintA, cfg.mintB, cfg.wrappedTokenProgram);
      const authority = authorityPda(wrappedMint);

      let supply = 0n;
      try { supply = BigInt((await conn.getTokenSupply(wrappedMint, 'confirmed')).value.amount); } catch (_) {}

      const escrows = [];
      let reserves = 0n;
      for (let i = 0; i < cfg.lpMintCount; i++) {
        const lp = cfg.lpMints[i];
        const dec = cfg.lpDecimals[i];
        let amount = null, prog = 'spl-token';
        for (const tp of [TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID]) {
          const escrow = getAssociatedTokenAddressSync(lp, authority, true, tp);
          const a = await readTokenAmount(escrow);
          if (a !== null) { amount = a; prog = tp.equals(TOKEN_2022_PROGRAM_ID) ? 'token-2022' : 'spl-token'; break; }
        }
        if (amount === null) amount = 0n;
        reserves += toCommon(amount, dec, cfg.shareDecimals);
        escrows.push({ lpMint: lp.toBase58(), decimals: dec, amount: amount.toString(), program: prog });
      }

      const navPerShare = supply > 0n ? Number(reserves) / Number(supply) : 0;
      const wm = wrappedMint.toBase58();
      const pair = {
        wrappedMint: wm,
        mintA: cfg.mintA.toBase58(),
        mintB: cfg.mintB.toBase58(),
        creator: cfg.creator.toBase58(),
        shareDecimals: cfg.shareDecimals,
        shareSupply: supply.toString(),
        reservesCommon: reserves.toString(),
        navPerShare,
        ammCount: escrows.length,
        escrows,
      };
      pairs.push(pair);

      const hist = state.history[wm] || (state.history[wm] = []);
      hist.push({ t: Date.now(), nav: navPerShare, supply: supply.toString(), reserves: reserves.toString() });
      if (hist.length > 1000) hist.shift();
    }
    // attach token metadata + featured-token display rule
    await resolveMeta([...new Set(pairs.flatMap((p) => [p.mintA, p.mintB]))]);
    for (const p of pairs) {
      const a = { mint: p.mintA, ...metaCache.get(p.mintA), quote: QUOTES[p.mintA] || null };
      const b = { mint: p.mintB, ...metaCache.get(p.mintB), quote: QUOTES[p.mintB] || null };
      p.tokens = { a, b };
      p.display = buildDisplay(a, b);
    }
    pairs.sort((a, b) => Number(b.reservesCommon) - Number(a.reservesCommon));
    state.pairs = pairs;
    state.lastUpdate = Date.now();
    state.error = null;
    console.log(`[crank] ${pairs.length} pair(s) updated @ ${new Date().toISOString()}`);
  } catch (e) {
    state.error = String(e.message || e);
    console.error('[crank] error:', state.error);
  }
}

const app = express();
app.use(express.static(path.join(__dirname, 'public')));
app.get('/api/config', (_req, res) => res.json({ programId: PROGRAM_ID.toBase58(), cluster: CLUSTER }));
app.get('/api/pairs', (_req, res) => res.json({ lastUpdate: state.lastUpdate, error: state.error, pairs: state.pairs }));
app.get('/api/history/:mint', (req, res) => res.json({ history: state.history[req.params.mint] || [] }));
app.get('/healthz', (_req, res) => res.send('ok'));

app.listen(PORT, () => {
  console.log(`NAV engine on :${PORT}  cluster=${CLUSTER}  program=${PROGRAM_ID.toBase58()}`);
  crank();
  setInterval(crank, POLL_MS);
});
