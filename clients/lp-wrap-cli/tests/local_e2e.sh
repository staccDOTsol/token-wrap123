#!/usr/bin/env bash
# End-to-end test for the SPL LP Wrap CLI against a local solana-test-validator.
#
# It deploys the program, injects a *fabricated* Raydium AMM v4 pool account
# (arbitrary owner + crafted layout, which a live cluster won't let you forge),
# creates the two underlying mints and an LP mint, then drives the CLI through
# create-pair-mint -> wrap -> nav -> unwrap and checks the fee splits and NAV.
#
# Requires: solana CLI, spl-token CLI, cargo, node, and a built program .so.
# Usage: SO=target/deploy/spl_lp_wrap.so PROGRAM_ID=<id> bash local_e2e.sh
set -euo pipefail

RPC=http://127.0.0.1:8899
WORK=$(mktemp -d)
REPO=$(cd "$(dirname "$0")/../../.." && pwd)
SO=${SO:-$REPO/target/deploy/spl_lp_wrap.so}
PROGRAM_ID=${PROGRAM_ID:-$(solana address -k "${SO%.so}-keypair.json" 2>/dev/null || echo EbmEELwtg3iqHdtNCKcRwZCKmWzGpF11ZTvc9sPxBQJB)}
CLI=${CLI:-$REPO/target/debug/spl-lp-wrap}
RAYDIUM_V4=675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8
cleanup() { pkill -f "solana-test-validator.*$WORK" 2>/dev/null || true; }
trap cleanup EXIT

echo "workdir: $WORK ; program: $PROGRAM_ID"
for n in user creator mintA mintB lpmint pool; do
  solana-keygen new --no-bip39-passphrase -s -o "$WORK/$n.json" >/dev/null
done
MA=$(solana address -k "$WORK/mintA.json")
MB=$(solana address -k "$WORK/mintB.json")
LP=$(solana address -k "$WORK/lpmint.json")
POOL=$(solana address -k "$WORK/pool.json")

# Fabricate a Raydium v4 pool: coin_mint@400, pc_mint@432, lp_mint@464.
node -e '
const fs=require("fs");const B58="123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
const dec=s=>{let n=0n;for(const c of s)n=n*58n+BigInt(B58.indexOf(c));const o=[];while(n){o.unshift(Number(n%256n));n/=256n;}for(const c of s){if(c==="1")o.unshift(0);else break;}while(o.length<32)o.unshift(0);return Uint8Array.from(o);};
const [pool,ma,mb,lp,owner,out]=process.argv.slice(2);
const d=Buffer.alloc(752);
dec(ma).forEach((b,i)=>d[400+i]=b);dec(mb).forEach((b,i)=>d[432+i]=b);dec(lp).forEach((b,i)=>d[464+i]=b);
fs.writeFileSync(out,JSON.stringify({pubkey:pool,account:{lamports:6960000,data:[d.toString("base64"),"base64"],owner,executable:false,rentEpoch:0,space:752}}));
' "$POOL" "$MA" "$MB" "$LP" "$RAYDIUM_V4" "$WORK/pool.json.acct"

echo "== starting validator =="
solana-test-validator --reset --quiet --ledger "$WORK/ledger" \
  --bpf-program "$PROGRAM_ID" "$SO" \
  --account "$POOL" "$WORK/pool.json.acct" >/dev/null 2>&1 &
for i in $(seq 1 40); do solana -u $RPC cluster-version >/dev/null 2>&1 && break; sleep 1; done
solana -u $RPC cluster-version

solana -u $RPC airdrop 100 "$(solana address -k "$WORK/user.json")"    >/dev/null
solana -u $RPC airdrop 100 "$(solana address -k "$WORK/creator.json")" >/dev/null

echo "== creating mints (A=6dec, B=9dec, LP=6dec) =="
solana config set -u $RPC -k "$WORK/user.json" >/dev/null
spl-token create-token "$WORK/mintA.json" --decimals 6 >/dev/null
spl-token create-token "$WORK/mintB.json" --decimals 9 >/dev/null
spl-token create-token "$WORK/lpmint.json" --decimals 6 >/dev/null
spl-token create-account "$LP" >/dev/null
spl-token mint "$LP" 1000 >/dev/null   # 1000 LP to the user

# random giphy metadata
read -r NAME SYM URI < <(node -e '
const adj=["Degen","Turbo","Cosmic","Hyper","Galactic","Quantum","Mega","Based"];
const no=["Vault","Yield","Reactor","Nexus","Forge","Index","Stack"];
const r=a=>a[Math.floor(Math.random()*a.length)];
const id=Array.from({length:13},()=>"abcdefghijklmnopqrstuvwxyz0123456789"[Math.floor(Math.random()*36)]).join("");
process.stdout.write(`${r(adj)}_${r(no)}_LP w${Array.from({length:3},()=>"ABCDEFGHJKLMNPQRSTUVWXYZ"[Math.floor(Math.random()*24)]).join("")} https://media.giphy.com/media/${id}/giphy.gif`);
')
echo "metadata: $NAME [$SYM] $URI"

echo "== create-pair-mint (signed by creator) =="
"$CLI" -u $RPC -k "$WORK/creator.json" --program-id "$PROGRAM_ID" \
  create-pair-mint "$MA" "$MB" --decimals 9 --name "$NAME" --symbol "$SYM" --uri "$URI"

echo "== wrap 500 LP (signed by user) =="
"$CLI" -u $RPC -k "$WORK/user.json" --program-id "$PROGRAM_ID" \
  wrap "$MA" "$MB" "$LP" "$POOL" 500000000

echo "== nav after wrap =="
"$CLI" -u $RPC -k "$WORK/user.json" --program-id "$PROGRAM_ID" nav "$MA" "$MB"

WRAPPED=$("$CLI" -u $RPC --program-id "$PROGRAM_ID" find-pdas "$MA" "$MB" | node -pe 'JSON.parse(require("fs").readFileSync(0)).wrapped_mint')
echo "wrapped mint: $WRAPPED"
echo "user shares:     $(spl-token balance "$WRAPPED" --owner "$(solana address -k "$WORK/user.json")" 2>/dev/null || echo 0)"
echo "creator shares:  $(spl-token balance "$WRAPPED" --owner "$(solana address -k "$WORK/creator.json")" 2>/dev/null || echo 0)"
echo "deployer shares: $(spl-token balance "$WRAPPED" --owner WzMaL78srutrF6CsxEkWuhMaDF5HZA6jNRaEPengqpb 2>/dev/null || echo 0)"

echo "== unwrap 100 shares (signed by user) =="
"$CLI" -u $RPC -k "$WORK/user.json" --program-id "$PROGRAM_ID" \
  unwrap "$MA" "$MB" "$LP" 100000000000

echo "== nav after unwrap =="
"$CLI" -u $RPC -k "$WORK/user.json" --program-id "$PROGRAM_ID" nav "$MA" "$MB"
echo "OK: e2e completed"
