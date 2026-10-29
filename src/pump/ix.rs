//! Builds today's Pump.fun trade instructions: `buy_exact_quote_in_v3` and `sell_v3`, which
//! work for tokens paired with SOL, USDC or any other whitelisted quote and for classic and
//! Token-2022 mints alike. Account order follows the official IDL.
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::pubkey;
use solana_sdk::pubkey::Pubkey;
use spl_associated_token_account::get_associated_token_address_with_program_id;
use spl_associated_token_account::instruction::create_associated_token_account_idempotent;

use super::accounts::BondingCurve;
use super::{disc, TOKEN_2022_PROGRAM, WSOL_MINT};
use crate::types::PUMP_FUN_PROGRAM_ID;

pub const FEE_PROGRAM: Pubkey = pubkey!("pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ");
pub const SYSTEM_PROGRAM: Pubkey = pubkey!("11111111111111111111111111111111");
pub const TOKEN_PROGRAM: Pubkey = pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
/// Second seed of the fee-config account (a constant from the IDL).
const FEE_CONFIG_SEED: [u8; 32] = [
    1, 86, 224, 246, 147, 102, 90, 207, 68, 219, 21, 104, 191, 23, 91, 170, 81, 137, 203, 151, 245,
    210, 255, 59, 101, 93, 43, 182, 253, 109, 24, 176,
];

fn pda(seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, &PUMP_FUN_PROGRAM_ID).0
}

pub fn global() -> Pubkey {
    pda(&[b"global"])
}
pub fn bonding_curve(mint: &Pubkey) -> Pubkey {
    pda(&[b"bonding-curve", mint.as_ref()])
}
pub fn event_authority() -> Pubkey {
    pda(&[b"__event_authority"])
}
pub fn user_volume_accumulator(user: &Pubkey) -> Pubkey {
    pda(&[b"user_volume_accumulator", user.as_ref()])
}
pub fn fee_config() -> Pubkey {
    Pubkey::find_program_address(&[b"fee_config", &FEE_CONFIG_SEED], &FEE_PROGRAM).0
}

pub fn ata(owner: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
    get_associated_token_address_with_program_id(owner, mint, token_program)
}

/// Everything about a token that its trade instructions depend on, read from the chain.
#[derive(Clone, Debug)]
pub struct Market {
    pub mint: Pubkey,
    pub curve: BondingCurve,
    pub base_token_program: Pubkey,
    pub quote_token_program: Pubkey,
    /// One of the program's buyback wallets (from the global settings).
    pub buyback_recipient: Pubkey,
}

impl Market {
    pub fn quote_mint(&self) -> Pubkey {
        self.curve.quote_mint
    }

    fn is_sol(&self) -> bool {
        self.curve.quote_mint == WSOL_MINT
    }

    fn trade_accounts(&self, user: &Pubkey) -> Vec<AccountMeta> {
        let curve = bonding_curve(&self.mint);
        let quote_mint = self.quote_mint();
        let buyback = if self.is_sol() {
            self.buyback_recipient
        } else {
            ata(
                &self.buyback_recipient,
                &quote_mint,
                &self.quote_token_program,
            )
        };
        vec![
            AccountMeta::new_readonly(global(), false),
            AccountMeta::new_readonly(self.mint, false),
            AccountMeta::new_readonly(quote_mint, false),
            AccountMeta::new_readonly(self.base_token_program, false),
            AccountMeta::new_readonly(self.quote_token_program, false),
            AccountMeta::new(curve, false),
            AccountMeta::new(ata(&curve, &self.mint, &self.base_token_program), false),
            AccountMeta::new(ata(&curve, &quote_mint, &self.quote_token_program), false),
            AccountMeta::new(*user, true),
            AccountMeta::new(self.user_base_ata(user), false),
            AccountMeta::new(ata(user, &quote_mint, &self.quote_token_program), false),
            AccountMeta::new(user_volume_accumulator(user), false),
            AccountMeta::new_readonly(fee_config(), false),
            AccountMeta::new(buyback, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(event_authority(), false),
            AccountMeta::new_readonly(PUMP_FUN_PROGRAM_ID, false),
        ]
    }

    pub fn user_base_ata(&self, user: &Pubkey) -> Pubkey {
        ata(user, &self.mint, &self.base_token_program)
    }

    /// Creates (if missing) the accounts a buy needs: the token account and, for tokens not
    /// paired with SOL, the account of the quote asset.
    pub fn prepare_buy(&self, user: &Pubkey) -> Vec<Instruction> {
        let mut ixs = vec![create_associated_token_account_idempotent(
            user,
            user,
            &self.mint,
            &self.base_token_program,
        )];
        if !self.is_sol() {
            ixs.push(create_associated_token_account_idempotent(
                user,
                user,
                &self.quote_mint(),
                &self.quote_token_program,
            ));
        }
        ixs
    }

    /// Spend exactly `spend` quote units and receive at least `min_tokens`.
    pub fn buy_exact_quote_in(&self, user: &Pubkey, spend: u64, min_tokens: u64) -> Instruction {
        let mut data = disc::BUY_EXACT_QUOTE_IN_V3.to_vec();
        data.extend_from_slice(&spend.to_le_bytes());
        data.extend_from_slice(&min_tokens.to_le_bytes());
        data.push(0); // partial_fill: off
        Instruction {
            program_id: PUMP_FUN_PROGRAM_ID,
            accounts: self.trade_accounts(user),
            data,
        }
    }

    /// Sell `amount` tokens for at least `min_quote` quote units.
    pub fn sell(&self, user: &Pubkey, amount: u64, min_quote: u64) -> Instruction {
        let mut data = disc::SELL_V3.to_vec();
        data.extend_from_slice(&amount.to_le_bytes());
        data.extend_from_slice(&min_quote.to_le_bytes());
        Instruction {
            program_id: PUMP_FUN_PROGRAM_ID,
            accounts: self.trade_accounts(user),
            data,
        }
    }

    /// Closes the (empty) token account after selling everything, returning its rent.
    pub fn close_token_account(&self, user: &Pubkey) -> Instruction {
        close_account(&self.base_token_program, &self.user_base_ata(user), user)
    }
}

/// SPL `CloseAccount` for either token program (the helper in `spl-token` refuses
/// Token-2022). Same instruction layout in both.
pub fn close_account(token_program: &Pubkey, account: &Pubkey, owner: &Pubkey) -> Instruction {
    debug_assert!(*token_program == TOKEN_PROGRAM || *token_program == TOKEN_2022_PROGRAM);
    Instruction {
        program_id: *token_program,
        accounts: vec![
            AccountMeta::new(*account, false),
            AccountMeta::new(*owner, false),
            AccountMeta::new_readonly(*owner, true),
        ],
        data: vec![9],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pump::parse::{self, Op, Quote};
    use crate::pump::USDC_MINT;
    use solana_sdk::message::compiled_instruction::CompiledInstruction;

    fn market(quote_mint: Pubkey) -> Market {
        Market {
            mint: Pubkey::new_unique(),
            curve: BondingCurve {
                virtual_token_reserves: 1,
                virtual_quote_reserves: 1,
                real_token_reserves: 1,
                real_quote_reserves: 1,
                token_total_supply: 1,
                complete: false,
                creator: Pubkey::new_unique(),
                quote: Quote::from_mint(quote_mint),
                quote_mint,
            },
            base_token_program: TOKEN_2022_PROGRAM,
            quote_token_program: TOKEN_PROGRAM,
            buyback_recipient: Pubkey::new_unique(),
        }
    }

    #[test]
    fn well_known_addresses_match_the_program() {
        assert_eq!(
            global().to_string(),
            "4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf"
        );
        assert_eq!(
            fee_config().to_string(),
            "8Wf5TiAheLUqBrKXeYg2JtAFFMWtKdG2BSFgqUcPVwTt"
        );
        assert_eq!(
            event_authority().to_string(),
            "Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1"
        );
    }

    #[test]
    fn buy_and_sell_have_the_idl_shape() {
        let user = Pubkey::new_unique();
        for quote in [WSOL_MINT, USDC_MINT] {
            let m = market(quote);
            let buy = m.buy_exact_quote_in(&user, 100, 5);
            let sell = m.sell(&user, 7, 1);
            for ix in [&buy, &sell] {
                assert_eq!(ix.accounts.len(), 17);
                // signer and writable flags of the IDL: user is the only signer
                assert_eq!(ix.accounts.iter().filter(|a| a.is_signer).count(), 1);
                assert!(ix.accounts[8].is_signer && ix.accounts[8].pubkey == user);
                assert_eq!(ix.accounts[1].pubkey, m.mint);
                assert_eq!(ix.accounts[2].pubkey, quote);
                assert_eq!(ix.accounts[16].pubkey, PUMP_FUN_PROGRAM_ID);
                for (i, writable) in [5, 6, 7, 8, 9, 10, 11, 13].iter().map(|i| (*i, true)) {
                    assert_eq!(ix.accounts[i].is_writable, writable, "account {i}");
                }
            }
            assert_eq!(buy.data.len(), 8 + 8 + 8 + 1);
            assert_eq!(sell.data.len(), 8 + 8 + 8);
        }
    }

    #[test]
    fn buyback_account_is_the_wallet_for_sol_and_its_quote_account_otherwise() {
        let user = Pubkey::new_unique();
        let sol = market(WSOL_MINT);
        assert_eq!(
            sol.sell(&user, 1, 0).accounts[13].pubkey,
            sol.buyback_recipient
        );
        let usdc = market(USDC_MINT);
        assert_eq!(
            usdc.sell(&user, 1, 0).accounts[13].pubkey,
            ata(&usdc.buyback_recipient, &USDC_MINT, &TOKEN_PROGRAM)
        );
    }

    #[test]
    fn what_we_build_is_what_the_feed_parser_reads_back() {
        let user = Pubkey::new_unique();
        let m = market(USDC_MINT);
        let ix = m.buy_exact_quote_in(&user, 100, 5);
        let mut keys = vec![PUMP_FUN_PROGRAM_ID];
        keys.extend(ix.accounts.iter().map(|a| a.pubkey));
        let compiled = CompiledInstruction::new_from_raw_parts(
            0,
            ix.data.clone(),
            (1..=ix.accounts.len() as u8).collect(),
        );
        let Some(Op::Trade(t)) = parse::parse_instruction(&keys, &compiled) else {
            panic!("own buy not parsed")
        };
        assert_eq!((t.mint, t.user, t.quote), (m.mint, user, Quote::Usdc));
        assert_eq!((t.quote_amount, t.token_amount), (100, 5));
    }

    #[test]
    fn closing_works_for_both_token_programs() {
        let (a, o) = (Pubkey::new_unique(), Pubkey::new_unique());
        for p in [TOKEN_PROGRAM, TOKEN_2022_PROGRAM] {
            let ix = close_account(&p, &a, &o);
            assert_eq!(ix.program_id, p);
            assert_eq!(ix.data, vec![9]);
            assert!(ix.accounts[2].is_signer);
        }
    }
}
