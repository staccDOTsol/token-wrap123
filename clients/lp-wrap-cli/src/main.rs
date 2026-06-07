//! Command-line utility for the SPL LP Wrap program.
//!
//! Subcommands:
//!   * `find-pdas`        derive the wrapped mint / authority / config / escrow
//!   * `create-pair-mint` create the wrapped share mint + config for a pair
//!   * `wrap`             deposit an AMM LP token, mint shares
//!   * `unwrap`           burn shares, withdraw a chosen LP token
//!   * `nav`              read on-chain state and print spot NAV as JSON

use {
    anyhow::{anyhow, Context, Result},
    clap::{Parser, Subcommand},
    solana_account::Account,
    solana_client::nonblocking::rpc_client::RpcClient,
    solana_commitment_config::CommitmentConfig,
    solana_instruction::Instruction,
    solana_keypair::{read_keypair_file, Keypair},
    solana_pubkey::Pubkey,
    solana_signer::Signer,
    solana_transaction::Transaction,
    spl_associated_token_account_interface::{
        address::get_associated_token_address_with_program_id,
        instruction::create_associated_token_account_idempotent,
    },
    spl_lp_wrap::{
        get_escrow_address, get_pair_config_address, get_wrapped_mint_address,
        get_wrapped_mint_authority, instruction as lp_ix, state::PairConfig, to_common_units,
    },
    spl_pod::optional_keys::OptionalNonZeroPubkey,
    spl_token_2022::{
        extension::{ExtensionType, PodStateWithExtensions},
        pod::{PodAccount, PodMint},
        state::Mint,
    },
    spl_token_metadata_interface::state::TokenMetadata,
    std::str::FromStr,
};

#[derive(Parser)]
#[clap(name = "spl-lp-wrap", about = "SPL LP Wrap CLI", version)]
struct Cli {
    /// RPC URL.
    #[clap(long, short = 'u', global = true, default_value = "http://127.0.0.1:8899")]
    url: String,
    /// Fee payer / signer keypair path.
    #[clap(long, short = 'k', global = true, default_value = "~/.config/solana/id.json")]
    keypair: String,
    /// Program id of the LP Wrap deployment.
    #[clap(long, global = true)]
    program_id: Option<String>,
    #[clap(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Derive all PDAs for a pair.
    FindPdas {
        mint_a: String,
        mint_b: String,
        /// Wrapped token program (defaults to Token-2022).
        #[clap(long)]
        wrapped_token_program: Option<String>,
    },
    /// Create the wrapped share mint and pair config.
    CreatePairMint {
        mint_a: String,
        mint_b: String,
        #[clap(long, default_value_t = 9)]
        decimals: u8,
        #[clap(long)]
        name: String,
        #[clap(long)]
        symbol: String,
        #[clap(long, default_value = "")]
        uri: String,
    },
    /// Wrap an AMM LP token into shares.
    Wrap {
        mint_a: String,
        mint_b: String,
        /// The LP mint to deposit.
        lp_mint: String,
        /// The AMM pool account the LP belongs to.
        amm_pool: String,
        /// Amount of LP (base units) to deposit.
        amount: u64,
        /// LP token program (defaults to SPL Token).
        #[clap(long)]
        lp_token_program: Option<String>,
    },
    /// Burn shares and withdraw a chosen LP token.
    Unwrap {
        mint_a: String,
        mint_b: String,
        /// The LP mint to withdraw.
        lp_mint: String,
        /// Amount of shares (base units) to burn.
        shares: u64,
        /// LP token program (defaults to SPL Token).
        #[clap(long)]
        lp_token_program: Option<String>,
    },
    /// Print spot NAV for a pair as JSON.
    Nav { mint_a: String, mint_b: String },
}

fn pk(s: &str) -> Result<Pubkey> {
    Pubkey::from_str(s).with_context(|| format!("invalid pubkey: {s}"))
}

fn load_keypair(path: &str) -> Result<Keypair> {
    let expanded = if let Some(rest) = path.strip_prefix("~/") {
        format!("{}/{}", std::env::var("HOME").unwrap_or_default(), rest)
    } else {
        path.to_string()
    };
    read_keypair_file(&expanded).map_err(|e| anyhow!("failed to read keypair {expanded}: {e}"))
}

/// Token program to assume for AMM LP mints / their escrows when not specified.
/// Raydium AMM v4 / CPMM / PumpSwap / Meteora LP mints are classic SPL Token.
fn default_lp_token_program() -> Pubkey {
    spl_token::id()
}

async fn send(rpc: &RpcClient, payer: &Keypair, ixs: &[Instruction]) -> Result<()> {
    let blockhash = rpc.get_latest_blockhash().await?;
    let tx = Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &[payer], blockhash);
    let sig = rpc.send_and_confirm_transaction(&tx).await?;
    println!("signature: {sig}");
    Ok(())
}

fn unpack_amount(account: &Account) -> Result<u64> {
    let state = PodStateWithExtensions::<PodAccount>::unpack(&account.data)?;
    Ok(u64::from(state.base.amount))
}

fn unpack_supply(account: &Account) -> Result<u64> {
    let state = PodStateWithExtensions::<PodMint>::unpack(&account.data)?;
    Ok(u64::from(state.base.supply))
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let rpc = RpcClient::new_with_commitment(cli.url.clone(), CommitmentConfig::confirmed());
    let program_id = match &cli.program_id {
        Some(s) => pk(s)?,
        None => spl_lp_wrap::id(),
    };
    let token_2022 = spl_token_2022::id();

    match cli.command {
        Command::FindPdas {
            mint_a,
            mint_b,
            wrapped_token_program,
        } => {
            let wtp = match wrapped_token_program {
                Some(s) => pk(&s)?,
                None => token_2022,
            };
            let (a, b) = (pk(&mint_a)?, pk(&mint_b)?);
            let wrapped_mint = get_wrapped_mint_address(&a, &b, &wtp);
            let authority = get_wrapped_mint_authority(&wrapped_mint);
            let config = get_pair_config_address(&wrapped_mint);
            println!(
                "{}",
                serde_json::json!({
                    "wrapped_mint": wrapped_mint.to_string(),
                    "mint_authority": authority.to_string(),
                    "pair_config": config.to_string(),
                    "wrapped_token_program": wtp.to_string(),
                })
            );
        }

        Command::CreatePairMint {
            mint_a,
            mint_b,
            decimals,
            name,
            symbol,
            uri,
        } => {
            let payer = load_keypair(&cli.keypair)?;
            let (a, b) = (pk(&mint_a)?, pk(&mint_b)?);
            let wrapped_mint = get_wrapped_mint_address(&a, &b, &token_2022);
            let authority = get_wrapped_mint_authority(&wrapped_mint);
            let config = get_pair_config_address(&wrapped_mint);

            // Compute exact rent: the mint carries TransferFee + MetadataPointer
            // fixed extensions plus the variable TokenMetadata.
            let base_space = ExtensionType::try_calculate_account_len::<Mint>(&[
                ExtensionType::TransferFeeConfig,
                ExtensionType::MetadataPointer,
            ])?;
            let metadata = TokenMetadata {
                update_authority: OptionalNonZeroPubkey::try_from(Some(authority))?,
                mint: wrapped_mint,
                name: name.clone(),
                symbol: symbol.clone(),
                uri: uri.clone(),
                additional_metadata: vec![],
            };
            let mint_space = base_space + metadata.tlv_size_of()?;
            let mint_rent = rpc.get_minimum_balance_for_rent_exemption(mint_space).await?;
            let config_rent = rpc
                .get_minimum_balance_for_rent_exemption(PairConfig::LEN)
                .await?;

            let ixs = vec![
                solana_system_interface::instruction::transfer(
                    &payer.pubkey(),
                    &wrapped_mint,
                    mint_rent,
                ),
                solana_system_interface::instruction::transfer(
                    &payer.pubkey(),
                    &config,
                    config_rent,
                ),
                lp_ix::create_pair_mint(
                    &program_id,
                    &payer.pubkey(),
                    &wrapped_mint,
                    &config,
                    &authority,
                    &a,
                    &b,
                    &token_2022,
                    decimals,
                    name,
                    symbol,
                    uri,
                ),
            ];
            send(&rpc, &payer, &ixs).await?;
            println!("wrapped_mint: {wrapped_mint}");
        }

        Command::Wrap {
            mint_a,
            mint_b,
            lp_mint,
            amm_pool,
            amount,
            lp_token_program,
        } => {
            let payer = load_keypair(&cli.keypair)?;
            let (a, b) = (pk(&mint_a)?, pk(&mint_b)?);
            let lp_mint = pk(&lp_mint)?;
            let amm_pool = pk(&amm_pool)?;
            let lp_prog = match lp_token_program {
                Some(s) => pk(&s)?,
                None => default_lp_token_program(),
            };
            let wrapped_mint = get_wrapped_mint_address(&a, &b, &token_2022);
            let authority = get_wrapped_mint_authority(&wrapped_mint);
            let config = get_pair_config_address(&wrapped_mint);

            let recipient = get_associated_token_address_with_program_id(
                &payer.pubkey(),
                &wrapped_mint,
                &token_2022,
            );
            let source_lp =
                get_associated_token_address_with_program_id(&payer.pubkey(), &lp_mint, &lp_prog);
            let lp_escrow =
                get_associated_token_address_with_program_id(&authority, &lp_mint, &lp_prog);

            let (other, cfg) = other_escrows(&rpc, &config, &authority, &lp_mint).await?;
            let other_refs: Vec<&Pubkey> = other.iter().collect();

            let creator_share = get_associated_token_address_with_program_id(
                &cfg.creator,
                &wrapped_mint,
                &token_2022,
            );
            let deployer_share = get_associated_token_address_with_program_id(
                &spl_lp_wrap::DEPLOYER,
                &wrapped_mint,
                &token_2022,
            );

            let ixs = vec![
                // ensure recipient + fee + escrow ATAs exist
                create_associated_token_account_idempotent(
                    &payer.pubkey(),
                    &payer.pubkey(),
                    &wrapped_mint,
                    &token_2022,
                ),
                create_associated_token_account_idempotent(
                    &payer.pubkey(),
                    &cfg.creator,
                    &wrapped_mint,
                    &token_2022,
                ),
                create_associated_token_account_idempotent(
                    &payer.pubkey(),
                    &spl_lp_wrap::DEPLOYER,
                    &wrapped_mint,
                    &token_2022,
                ),
                create_associated_token_account_idempotent(
                    &payer.pubkey(),
                    &authority,
                    &lp_mint,
                    &lp_prog,
                ),
                lp_ix::wrap(
                    &program_id,
                    &recipient,
                    &wrapped_mint,
                    &authority,
                    &config,
                    &token_2022,
                    &lp_prog,
                    &source_lp,
                    &lp_mint,
                    &lp_escrow,
                    &amm_pool,
                    &payer.pubkey(),
                    &creator_share,
                    &deployer_share,
                    &other_refs,
                    amount,
                ),
            ];
            send(&rpc, &payer, &ixs).await?;
        }

        Command::Unwrap {
            mint_a,
            mint_b,
            lp_mint,
            shares,
            lp_token_program,
        } => {
            let payer = load_keypair(&cli.keypair)?;
            let (a, b) = (pk(&mint_a)?, pk(&mint_b)?);
            let lp_mint = pk(&lp_mint)?;
            let lp_prog = match lp_token_program {
                Some(s) => pk(&s)?,
                None => default_lp_token_program(),
            };
            let wrapped_mint = get_wrapped_mint_address(&a, &b, &token_2022);
            let authority = get_wrapped_mint_authority(&wrapped_mint);
            let config = get_pair_config_address(&wrapped_mint);

            let source_share = get_associated_token_address_with_program_id(
                &payer.pubkey(),
                &wrapped_mint,
                &token_2022,
            );
            let dest_lp =
                get_associated_token_address_with_program_id(&payer.pubkey(), &lp_mint, &lp_prog);
            let lp_escrow =
                get_associated_token_address_with_program_id(&authority, &lp_mint, &lp_prog);

            let (other, cfg) = other_escrows(&rpc, &config, &authority, &lp_mint).await?;
            let other_refs: Vec<&Pubkey> = other.iter().collect();

            let creator_share = get_associated_token_address_with_program_id(
                &cfg.creator,
                &wrapped_mint,
                &token_2022,
            );
            let deployer_share = get_associated_token_address_with_program_id(
                &spl_lp_wrap::DEPLOYER,
                &wrapped_mint,
                &token_2022,
            );

            let ixs = vec![
                create_associated_token_account_idempotent(
                    &payer.pubkey(),
                    &payer.pubkey(),
                    &lp_mint,
                    &lp_prog,
                ),
                create_associated_token_account_idempotent(
                    &payer.pubkey(),
                    &cfg.creator,
                    &wrapped_mint,
                    &token_2022,
                ),
                create_associated_token_account_idempotent(
                    &payer.pubkey(),
                    &spl_lp_wrap::DEPLOYER,
                    &wrapped_mint,
                    &token_2022,
                ),
                lp_ix::unwrap(
                    &program_id,
                    &source_share,
                    &wrapped_mint,
                    &authority,
                    &config,
                    &token_2022,
                    &lp_prog,
                    &dest_lp,
                    &lp_mint,
                    &lp_escrow,
                    &payer.pubkey(),
                    &creator_share,
                    &deployer_share,
                    &other_refs,
                    shares,
                ),
            ];
            send(&rpc, &payer, &ixs).await?;
        }

        Command::Nav { mint_a, mint_b } => {
            let (a, b) = (pk(&mint_a)?, pk(&mint_b)?);
            let wrapped_mint = get_wrapped_mint_address(&a, &b, &token_2022);
            let authority = get_wrapped_mint_authority(&wrapped_mint);
            let config_addr = get_pair_config_address(&wrapped_mint);

            let config_acc = rpc
                .get_account(&config_addr)
                .await
                .context("pair config not found; create-pair-mint first")?;
            let config: PairConfig = *bytemuck::try_from_bytes::<PairConfig>(&config_acc.data)
                .map_err(|_| anyhow!("bad pair config data"))?;

            let supply = unpack_supply(&rpc.get_account(&wrapped_mint).await?)?;

            let mut reserves_common: u128 = 0;
            let mut escrows = vec![];
            for (i, lp_mint) in config.registered().iter().enumerate() {
                let decimals = config.lp_decimals[i];
                // try SPL Token escrow first, then Token-2022.
                let mut amount = 0u64;
                for prog in [spl_token::id(), spl_token_2022::id()] {
                    let escrow = get_escrow_address(&authority, lp_mint, &prog);
                    if let Ok(acc) = rpc.get_account(&escrow).await {
                        if let Ok(amt) = unpack_amount(&acc) {
                            amount = amt;
                            break;
                        }
                    }
                }
                let normalized = to_common_units(amount, decimals, config.share_decimals)
                    .ok_or_else(|| anyhow!("overflow normalizing reserves"))?;
                reserves_common += normalized;
                escrows.push(serde_json::json!({
                    "lp_mint": lp_mint.to_string(),
                    "decimals": decimals,
                    "amount": amount,
                    "normalized": normalized.to_string(),
                }));
            }

            // NAV per share = reserves_common / supply, both in share-decimal scale.
            let nav_per_share = if supply == 0 {
                0.0
            } else {
                reserves_common as f64 / supply as f64
            };

            println!(
                "{}",
                serde_json::json!({
                    "wrapped_mint": wrapped_mint.to_string(),
                    "mint_a": config.mint_a.to_string(),
                    "mint_b": config.mint_b.to_string(),
                    "share_decimals": config.share_decimals,
                    "share_supply": supply,
                    "reserves_common": reserves_common.to_string(),
                    "nav_per_share": nav_per_share,
                    "escrows": escrows,
                })
            );
        }
    }
    Ok(())
}

/// Derive the escrow addresses for every registered LP mint except
/// `current_lp_mint`, reading the on-chain pair config. Escrows are assumed to
/// be SPL Token ATAs (the AMMs' LP mints are classic SPL Token); falls back to
/// Token-2022 if the SPL Token escrow does not exist.
async fn other_escrows(
    rpc: &RpcClient,
    config_addr: &Pubkey,
    authority: &Pubkey,
    current_lp_mint: &Pubkey,
) -> Result<(Vec<Pubkey>, PairConfig)> {
    let acc = rpc
        .get_account(config_addr)
        .await
        .context("pair config not found")?;
    let config: PairConfig =
        *bytemuck::try_from_bytes::<PairConfig>(&acc.data).map_err(|_| anyhow!("bad config"))?;
    let mut escrows = vec![];
    for lp_mint in config.registered() {
        if lp_mint == current_lp_mint {
            continue;
        }
        let spl = get_escrow_address(authority, lp_mint, &spl_token::id());
        let escrow = if rpc.get_account(&spl).await.is_ok() {
            spl
        } else {
            get_escrow_address(authority, lp_mint, &spl_token_2022::id())
        };
        escrows.push(escrow);
    }
    Ok((escrows, config))
}
