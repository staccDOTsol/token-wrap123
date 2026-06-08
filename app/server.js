'use strict';
// Off-chain NAV crank + API for the SPL LP Wrap program.
//
// Every POLL_MS it scans the program's PairConfig accounts, reconstructs each
// wrapped pair's NAV (= total normalized reserves / share supply) directly from
// on-chain state, and serves the current snapshot + a NAV time-series to the
// degen UI in public/.

const path = require('path');
const fs = require('fs');
const crypto = require('crypto');
const express = require('express');
const multer = require('multer');
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
          description: md.description || '',
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

// USD valuation: LP unit price = pool TVL(USD) / LP supply, from each AMM's API.
const lpPxCache = new Map();
async function lpUsdPriceMap(a, b) {
  const key = a < b ? a + b : b + a;
  const c = lpPxCache.get(key);
  if (c && Date.now() - c.t < 60000) return c.map;
  const map = {};
  try {
    const r = await fetch(`https://api-v3.raydium.io/pools/info/mint?mint1=${a}&mint2=${b}&poolType=all&poolSortField=liquidity&sortType=desc&pageSize=30&page=1`, { headers: { accept: 'application/json' } });
    const j = await r.json();
    for (const p of ((j.data && j.data.data) || [])) {
      const lp = p.lpMint && p.lpMint.address, tvl = Number(p.tvl || 0), amt = Number(p.lpAmount || 0);
      if (lp && tvl > 0 && amt > 0) map[lp] = tvl / amt; // USD per UI LP
    }
  } catch (_) {}
  try {
    const r = await fetch(`https://amm-v2.meteora.ag/pools/search?include_token_mints=${a}&include_token_mints=${b}`, { headers: { accept: 'application/json' } });
    const j = await r.json();
    for (const p of (Array.isArray(j) ? j : (j.data || []))) {
      const lp = p.lp_mint || p.pool_token_mint, tvl = Number(p.pool_tvl || p.tvl || 0);
      const dec = Number(p.lp_decimal ?? p.lp_mint_decimals ?? 0);
      const supRaw = Number(p.lp_supply || 0);
      const supUi = dec ? supRaw / 10 ** dec : supRaw; // normalize to UI LP units
      if (lp && tvl > 0 && supUi > 0) map[lp] = tvl / supUi; // USD per UI LP
    }
  } catch (_) {}
  lpPxCache.set(key, { map, t: Date.now() });
  return map;
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
      // USD valuation of the vault's underlying (Σ escrow LP × LP USD price).
      let usdTvl = 0;
      try {
        const lpPx = await lpUsdPriceMap(cfg.mintA.toBase58(), cfg.mintB.toBase58());
        for (const e of escrows) { const px = lpPx[e.lpMint]; if (px) usdTvl += (Number(e.amount) / 10 ** e.decimals) * px; }
      } catch (_) {}
      const supplyUi = Number(supply) / 10 ** cfg.shareDecimals;
      const navUsd = supplyUi > 0 ? usdTvl / supplyUi : 0;
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
        usdTvl,
        navUsd,
        ammCount: escrows.length,
        escrows,
      };
      pairs.push(pair);

      const hist = state.history[wm] || (state.history[wm] = []);
      hist.push({ t: Date.now(), nav: navPerShare, supply: supply.toString(), reserves: reserves.toString(), usd: usdTvl, navUsd });
      if (hist.length > 1000) hist.shift();
    }
    // attach token metadata + featured-token display rule. We also resolve the
    // WRAPPED mint's own UGC metadata (creator-uploaded image/name/description),
    // which is the hero of the card + vault page.
    await resolveMeta([...new Set(pairs.flatMap((p) => [p.mintA, p.mintB, p.wrappedMint]))]);
    for (const p of pairs) {
      const a = { mint: p.mintA, ...metaCache.get(p.mintA), quote: QUOTES[p.mintA] || null };
      const b = { mint: p.mintB, ...metaCache.get(p.mintB), quote: QUOTES[p.mintB] || null };
      p.tokens = { a, b };
      p.display = buildDisplay(a, b);
      p.meta = metaCache.get(p.wrappedMint) || { name: '', symbol: '', description: '', image: null };
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

// UGC storage on a Fly volume (no external auth). Falls back to a local dir
// when /data isn't mounted (dev).
const DATA_DIR = process.env.DATA_DIR || (fs.existsSync('/data') ? '/data' : path.join(__dirname, 'data'));
const UP_DIR = path.join(DATA_DIR, 'uploads');
fs.mkdirSync(UP_DIR, { recursive: true });
const uploadStore = multer.diskStorage({
  destination: UP_DIR,
  filename: (_req, file, cb) => {
    const ext = (String(file.originalname).match(/\.(png|jpe?g|gif|webp|svg)$/i) || ['.png'])[0].toLowerCase();
    cb(null, crypto.randomBytes(8).toString('hex') + ext);
  },
});
const upload = multer({
  storage: uploadStore,
  limits: { fileSize: 8 * 1024 * 1024 },
  // Block SVG (can carry inline JS) and any non-raster image.
  fileFilter: (_req, file, cb) => cb(null, /^image\/(png|jpe?g|gif|webp)$/.test(file.mimetype)),
});

const app = express();
app.set('trust proxy', true);
app.disable('x-powered-by');
// Security headers (defense-in-depth alongside output escaping).
app.use((_req, res, next) => {
  res.set({
    'Strict-Transport-Security': 'max-age=31536000; includeSubDomains',
    'X-Content-Type-Options': 'nosniff',
    'X-Frame-Options': 'DENY',
    'Referrer-Policy': 'no-referrer',
    'Permissions-Policy': 'geolocation=(), microphone=(), camera=()',
    'Content-Security-Policy': [
      "default-src 'self'",
      "script-src 'self' 'unsafe-inline' https://esm.sh",
      "style-src 'self' 'unsafe-inline' https://fonts.googleapis.com",
      "font-src https://fonts.gstatic.com",
      "img-src 'self' data: https:",
      "connect-src 'self' https://esm.sh",
      "frame-ancestors 'none'",
      "base-uri 'self'",
      "object-src 'none'",
    ].join('; '),
  });
  next();
});
app.use(express.json({ limit: '2mb' }));

// SOL zap endpoints (lazy-require heavy SDKs inside handlers; boots offline).
try {
  require('./zap').register(app);
} catch (e) {
  console.error('zap module failed to load (dashboard unaffected):', e.message);
}

// Accept an image + name/symbol, persist the image and a Metaplex-standard JSON
// manifest to the volume, and return the manifest URL to use as the token uri.
app.post('/api/upload', upload.single('image'), (req, res) => {
  if (!req.file) return res.status(400).json({ error: 'image file required' });
  const id = path.parse(req.file.filename).name;
  const base = process.env.PUBLIC_URL || `${req.protocol}://${req.get('host')}`;
  const image = `${base}/u/${req.file.filename}`;
  const manifest = {
    name: (req.body.name || 'Wrapped LP').slice(0, 64),
    symbol: (req.body.symbol || 'wLP').slice(0, 12),
    description: (req.body.description || 'LP Wrap vault share — one token, every AMM’s LP.').slice(0, 512),
    image,
  };
  fs.writeFileSync(path.join(UP_DIR, id + '.json'), JSON.stringify(manifest));
  res.json({ uri: `${base}/u/${id}.json`, image, manifest });
});
app.use('/u', express.static(UP_DIR, { maxAge: '365d', immutable: true }));
// Same-origin RPC proxy so the browser hits mainnet (via Helius) without ever
// seeing the API key. Only allow a read/submit allowlist (no admin/airdrop/etc).
const RPC_METHODS = new Set([
  'getAccountInfo','getMultipleAccounts','getProgramAccounts','getBalance','getTokenAccountBalance',
  'getTokenAccountsByOwner','getParsedTokenAccountsByOwner','getTokenSupply','getLatestBlockhash',
  'getSignatureStatuses','getSignatureStatus','sendTransaction','simulateTransaction','getSlot',
  'getMinimumBalanceForRentExemption','getFeeForMessage','getAsset','getAssetBatch','getTransaction',
  'isBlockhashValid','getRecentPrioritizationFees','getEpochInfo','getBlockHeight',
]);
app.post('/rpc', async (req, res) => {
  try {
    const body = req.body || {};
    const method = body.method;
    if (!method || !RPC_METHODS.has(method)) return res.status(400).json({ error: `method not allowed: ${method}` });
    const r = await fetch(RPC_URL, {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify(body),
    });
    res.type('application/json').send(await r.text());
  } catch (e) { res.status(502).json({ error: String(e) }); }
});
app.get('/api/config', (_req, res) => res.json({ programId: PROGRAM_ID.toBase58(), cluster: CLUSTER }));
app.get('/api/pairs', (_req, res) => res.json({ lastUpdate: state.lastUpdate, error: state.error, pairs: state.pairs }));
app.get('/api/history/:mint', (req, res) => res.json({ history: state.history[req.params.mint] || [] }));
app.get('/healthz', (_req, res) => res.send('ok'));

// --- SSE live NAV stream ---
const sseClients = new Set();
app.get('/api/stream', (req, res) => {
  res.set({ 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache', Connection: 'keep-alive' });
  res.flushHeaders();
  res.write(`data: ${JSON.stringify({ lastUpdate: state.lastUpdate, pairs: state.pairs })}\n\n`);
  sseClients.add(res);
  const ping = setInterval(() => res.write(': ping\n\n'), 25000);
  req.on('close', () => { clearInterval(ping); sseClients.delete(res); });
});
function broadcast() {
  const payload = `data: ${JSON.stringify({ lastUpdate: state.lastUpdate, pairs: state.pairs })}\n\n`;
  for (const c of sseClients) { try { c.write(payload); } catch (_) {} }
}

// --- SSR landing: inject the current snapshot so the page paints instantly ---
const INDEX = fs.readFileSync(path.join(__dirname, 'public', 'index.html'), 'utf8');
function renderApp(res) {
  const initial = JSON.stringify({ config: { programId: PROGRAM_ID.toBase58(), cluster: CLUSTER }, pairs: state.pairs, lastUpdate: state.lastUpdate });
  res.type('html').send(INDEX.replace('</head>', `<script>window.__INITIAL__=${initial}</script></head>`));
}
app.get('/', (_req, res) => renderApp(res));
// Deep-linkable vault page; client router reads the path.
app.get('/vault/:mint', (_req, res) => renderApp(res));
app.use(express.static(path.join(__dirname, 'public')));

app.listen(PORT, () => {
  console.log(`NAV engine on :${PORT}  cluster=${CLUSTER}  program=${PROGRAM_ID.toBase58()}`);
  (async () => { await crank(); broadcast(); })();
  setInterval(async () => { await crank(); broadcast(); }, POLL_MS);
});
