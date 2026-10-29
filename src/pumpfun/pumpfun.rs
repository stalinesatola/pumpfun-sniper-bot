use crate::costs::{COMPUTE_UNIT_LIMIT, CU_PRICE_MICROLAMPORTS};
use crate::pump::ix::{self, Market};
use crate::pump::market::{load_market, GlobalCache};
use crate::pump::parse::Quote;
use crate::pumpfun::error::{PumpFunError, PumpFunResult};
use crate::pumpswap::constants::PriorityFeePump;
use crate::types::sol_to_lamports;

use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_config::RpcSendTransactionConfig;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_sdk::hash::Hash;
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::message::{v0::Message as MessageV0, VersionedMessage};
use solana_sdk::pubkey;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;
use solana_sdk::transaction::VersionedTransaction;
use solana_system_interface::instruction as system_instruction;

use log::info;
use rustls::crypto::{ring::default_provider, CryptoProvider};
use std::sync::Arc;

const ASTRALANE_TIP: Pubkey = pubkey!("astra4uejePWneqNaJKuFFA8oonqCE1sqF6b45kDMZm");
/// Read-only marker account that keeps Jito bundles from placing the transaction after a
/// front-running one ("don't front").
const JITO_DONT_FRONT: Pubkey = pubkey!("jitodontfront111111111111111111111111111111");

/// The part of the transaction both directions share: compute budget, with the priority fee.
fn compute_budget() -> Vec<Instruction> {
    let mut limit = ComputeBudgetInstruction::set_compute_unit_limit(COMPUTE_UNIT_LIMIT);
    limit.accounts.push(AccountMeta {
        pubkey: JITO_DONT_FRONT,
        is_signer: false,
        is_writable: false,
    });
    vec![
        limit,
        ComputeBudgetInstruction::set_compute_unit_price(CU_PRICE_MICROLAMPORTS),
    ]
}

fn sign(
    payer: &Keypair,
    instructions: &[Instruction],
    blockhash: Hash,
) -> PumpFunResult<VersionedTransaction> {
    let message = MessageV0::try_compile(&payer.pubkey(), instructions, &[], blockhash)
        .map_err(|e| PumpFunError::TransactionError(e.to_string()))?;
    VersionedTransaction::try_new(VersionedMessage::V0(message), &[payer])
        .map_err(|e| PumpFunError::TransactionError(e.to_string()))
}

/// The instructions of a buy: compute budget, the accounts it needs, spending exactly
/// `spend` quote units for at least `min_tokens`, and the landing tip.
pub fn buy_instructions(
    market: &Market,
    user: &Pubkey,
    spend: u64,
    min_tokens: u64,
    tip_lamports: u64,
) -> Vec<Instruction> {
    let mut instructions = compute_budget();
    instructions.extend(market.prepare_buy(user));
    instructions.push(market.buy_exact_quote_in(user, spend, min_tokens));
    instructions.push(system_instruction::transfer(
        user,
        &ASTRALANE_TIP,
        tip_lamports,
    ));
    instructions
}

/// The instructions of a sell of `amount` tokens, optionally closing the token account
/// afterwards (when it sells everything), which returns its rent.
pub fn sell_instructions(
    market: &Market,
    user: &Pubkey,
    amount: u64,
    tip_lamports: u64,
    close_account: bool,
) -> Vec<Instruction> {
    let mut instructions = compute_budget();
    instructions.push(system_instruction::transfer(
        user,
        &ASTRALANE_TIP,
        tip_lamports,
    ));
    instructions.push(market.sell(user, amount, 0));
    if close_account {
        instructions.push(market.close_token_account(user));
    }
    instructions
}

/// A signed buy transaction. Pure: no network.
pub fn buy_transaction(
    market: &Market,
    payer: &Keypair,
    blockhash: Hash,
    spend: u64,
    min_tokens: u64,
    tip_lamports: u64,
) -> PumpFunResult<VersionedTransaction> {
    let ixs = buy_instructions(market, &payer.pubkey(), spend, min_tokens, tip_lamports);
    sign(payer, &ixs, blockhash)
}

/// A signed sell transaction. Pure: no network.
pub fn sell_transaction(
    market: &Market,
    payer: &Keypair,
    blockhash: Hash,
    amount: u64,
    tip_lamports: u64,
    close_account: bool,
) -> PumpFunResult<VersionedTransaction> {
    let ixs = sell_instructions(market, &payer.pubkey(), amount, tip_lamports, close_account);
    sign(payer, &ixs, blockhash)
}

/// The least tokens a buy must give: what the curve returns now, less `slippage_percent`.
pub fn min_tokens_out(expected: u64, slippage_percent: u64) -> u64 {
    let keep = 100u128.saturating_sub(slippage_percent as u128);
    (expected as u128 * keep / 100) as u64
}

#[derive(Clone)]
pub struct PumpFunSDK {
    pub client: Arc<RpcClient>,
    pub astralane_client: Arc<RpcClient>,
    pub priority_fee: PriorityFeePump,
    globals: Arc<GlobalCache>,
}

impl PumpFunSDK {
    pub async fn new(rpc_url: &str, astralane_url: &str, priority_fee: PriorityFeePump) -> Self {
        if CryptoProvider::get_default().is_none() {
            // Another thread may install a provider between the check and here; that is fine.
            let _ = default_provider().install_default();
        }
        PumpFunSDK {
            client: Arc::new(RpcClient::new(String::from(rpc_url))),
            astralane_client: Arc::new(RpcClient::new(astralane_url.to_string())),
            priority_fee,
            globals: Arc::new(GlobalCache::default()),
        }
    }

    /// Reads the program's global settings now, so the first trade does not wait for them.
    pub async fn warm_up(&self) -> PumpFunResult<()> {
        self.globals
            .refresh(&self.client)
            .await
            .map(|_| ())
            .map_err(|e| PumpFunError::TransactionError(format!("{e:#}")))
    }

    async fn market(&self, mint: &Pubkey) -> PumpFunResult<Market> {
        load_market(&self.client, &self.globals, mint)
            .await
            .map_err(|e| PumpFunError::TransactionError(format!("{e:#}")))
    }

    /// Token balance (raw units) of `owner`'s associated token account for `mint`, whichever
    /// token program the mint uses. Returns 0 when the account does not exist yet (e.g. the
    /// buy has not landed).
    pub async fn token_balance(&self, owner: &Pubkey, mint: &Pubkey) -> PumpFunResult<u64> {
        let program = self.client.get_account(mint).await?.owner;
        let ata = ix::ata(owner, mint, &program);
        match self.client.get_token_account_balance(&ata).await {
            Ok(balance) => balance
                .amount
                .parse::<u64>()
                .map_err(|e| PumpFunError::TransactionError(format!("bad token balance: {e}"))),
            Err(e) if e.to_string().contains("could not find account") => Ok(0),
            Err(e) => Err(e.into()),
        }
    }

    pub async fn latest_blockhash(&self) -> PumpFunResult<Hash> {
        Ok(self.client.get_latest_blockhash().await?)
    }

    async fn place_tx(&self, tx: VersionedTransaction) -> PumpFunResult<String> {
        let txconfig = RpcSendTransactionConfig {
            skip_preflight: true,
            ..Default::default()
        };
        self.astralane_client
            .send_transaction_with_config(&tx, txconfig)
            .await
            .map(|s| s.to_string())
            .map_err(|e| PumpFunError::TransactionError(e.to_string()))
    }

    /// Sells the whole `token_amount` on the bonding curve.
    pub async fn sell(
        &self,
        mint: &Pubkey,
        token_amount: u64,
        user_keypair: Keypair,
        blockhash: Hash,
    ) -> PumpFunResult<String> {
        info!(
            "SELL REQUEST: mint {} user {} amount {}",
            mint,
            user_keypair.pubkey(),
            token_amount
        );
        let market = self.market(mint).await?;
        if market.curve.complete {
            return Err(PumpFunError::TransactionError(
                "the bonding curve is complete: the token moved to PumpSwap, which the bot cannot sell on yet; sell it by hand".into(),
            ));
        }
        let tx = sell_transaction(
            &market,
            &user_keypair,
            blockhash,
            token_amount,
            sol_to_lamports(self.priority_fee.jito_tip_fee),
            true,
        )?;
        self.place_tx(tx).await
    }

    /// Buys `sol_to_buy` SOL worth of `mint` on the bonding curve, asking for the tokens the
    /// curve returns right now less `slippage_percent`. Returns the signature and the
    /// tokens expected.
    pub async fn buy(
        &self,
        mint: &Pubkey,
        sol_to_buy: f64,
        user_keypair: Keypair,
        blockhash: Hash,
        slippage_percent: u64,
    ) -> PumpFunResult<(String, u64)> {
        let market = self.market(mint).await?;
        if market.curve.complete {
            return Err(PumpFunError::TransactionError(
                "the bonding curve is complete: the token moved to PumpSwap".into(),
            ));
        }
        if market.curve.quote != Quote::Sol {
            return Err(PumpFunError::TransactionError(format!(
                "token is priced in {:?}; buying it needs a budget in that asset, which is not configured yet",
                market.curve.quote
            )));
        }
        let spend = sol_to_lamports(sol_to_buy);
        let expected = market.curve.tokens_for_quote(spend).ok_or_else(|| {
            PumpFunError::TransactionError("the bonding curve has no liquidity".into())
        })?;
        info!(
            "BUY REQUEST: mint {} user {} spend {} SOL, expecting {} tokens",
            mint,
            user_keypair.pubkey(),
            sol_to_buy,
            expected
        );
        let tx = buy_transaction(
            &market,
            &user_keypair,
            blockhash,
            spend,
            min_tokens_out(expected, slippage_percent),
            sol_to_lamports(self.priority_fee.jito_tip_fee),
        )?;
        Ok((self.place_tx(tx).await?, expected))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slippage_lowers_the_minimum_tokens() {
        assert_eq!(min_tokens_out(1_000, 5), 950);
        assert_eq!(min_tokens_out(1_000, 0), 1_000);
        assert_eq!(min_tokens_out(1_000, 100), 0);
        assert_eq!(min_tokens_out(1_000, 250), 0, "never underflows");
        assert_eq!(
            min_tokens_out(u64::MAX, 5),
            (u64::MAX as u128 * 95 / 100) as u64
        );
    }
}
