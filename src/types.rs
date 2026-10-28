use solana_sdk::hash::Hash;
use solana_sdk::instruction::CompiledInstruction;
use solana_sdk::pubkey;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::transaction::VersionedTransaction;

pub const PUMP_AMM_PROGRAM_ID_STR: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";
pub const PUMP_FUN_PROGRAM_ID_STR: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
pub const PUMP_FUN_PROGRAM_ID: Pubkey = pubkey!("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P");

#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]

pub struct BuyAmounts {
    pub base_amount_out: u64,
    pub max_quote_amount_in: u64,
}

impl BuyAmounts {
    pub fn new() -> Self {
        Self {
            base_amount_out: 0,
            max_quote_amount_in: 0,
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]

pub struct SellAmounts {
    base_amount_in: u64,
    max_quote_amount_out: u64,
}

impl SellAmounts {
    pub fn new() -> Self {
        Self {
            base_amount_in: 0,
            max_quote_amount_out: 0,
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
pub enum Amounts {
    #[default]
    Unknown,

    BuyAmounts(BuyAmounts),
    SellAmounts(SellAmounts),
}

#[derive(Debug)]
pub enum AmountParsingError {
    PassedBuyInSellFunction,
    PassedSellInBuyFunction,
    UnknownAmountPassed,
}

pub fn get_buy_amounts(amounts: Amounts) -> Result<BuyAmounts, AmountParsingError> {
    match amounts {
        Amounts::Unknown => Err(AmountParsingError::UnknownAmountPassed),
        Amounts::BuyAmounts(buy_amounts) => Ok(buy_amounts),
        Amounts::SellAmounts(_) => Err(AmountParsingError::PassedSellInBuyFunction),
    }
}

pub fn get_sell_amounts(amounts: Amounts) -> Result<SellAmounts, AmountParsingError> {
    match amounts {
        Amounts::Unknown => Err(AmountParsingError::UnknownAmountPassed),
        Amounts::BuyAmounts(_) => Err(AmountParsingError::PassedBuyInSellFunction),
        Amounts::SellAmounts(sell_amounts) => Ok(sell_amounts),
    }
}

#[derive(Debug)]
pub enum TXParserError {
    NotEnoughWallets,
    MissingAccountKeys,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TransactionType {
    Buy,
    Sell,
    Unknown,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct PumpSwapTXInfo {
    pub blockhash: Hash,
    pub pool: Pubkey,
    pub user: Pubkey,
    pub global_config: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub user_base_token_account: Pubkey,
    pub user_quote_token_account: Pubkey,
    pub pool_base_token_account: Pubkey,
    pub pool_quote_token_account: Pubkey,
    pub protocol_fee_recipient: Pubkey,
    pub protocol_fee_recipient_token_account: Pubkey,
    pub base_token_program: Pubkey,
    pub quote_token_program: Pubkey,
    pub system_program: Pubkey,
    pub associated_token_program: Pubkey,
    pub event_authority: Pubkey,
    pub program: Pubkey,
    pub amounts: Amounts,
}

fn create_base_tx_pumpswap(
    tx: &VersionedTransaction,
    ix: &CompiledInstruction,
) -> Result<PumpSwapTXInfo, TXParserError> {
    let blockhash = tx.message.recent_blockhash().to_owned();
    let account_keys = tx.message.static_account_keys();
    // let client = RpcClient::new(std::env::var("RPC_URL").unwrap());

    // let mut keys = Vec::new();
    // let lookup_table = tx.message.address_table_lookups();
    // if lookup_table.is_some() {
    //     info!("The lookup table has something! {:?}", lookup_table);

    //     for table in lookup_table.unwrap() {
    //         let raw_account = client.get_account(&table.account_key).unwrap();
    //         let address_lookup_table = AddressLookupTable::deserialize(&raw_account.data).unwrap();
    //         let address_lookup_table_account = AddressLookupTableAccount {
    //             key: table.account_key,
    //             addresses: address_lookup_table.addresses.to_vec(),
    //         };

    //         println!(
    //             "THE FUCKING LOOKUP TABLE: {:?}",
    //             address_lookup_table_account
    //         );
    //     }
    // } else {
    //     info!("The lookup table is gay")
    // }

    if account_keys.len() <= ix.accounts.iter().max().unwrap().to_owned() as usize {
        println!("Missing account keys!");
        println!(
            "Account Keys: {:?}\nIX Account keys: {:?}",
            account_keys, ix.accounts
        );
        return Err(TXParserError::NotEnoughWallets);
    }

    // 17 acccounts in there
    let pool = account_keys[ix.accounts[0] as usize];
    let user = account_keys[ix.accounts[1] as usize];
    let global_config = account_keys[ix.accounts[2] as usize];
    let base_mint = account_keys[ix.accounts[3] as usize];
    let quote_mint = account_keys[ix.accounts[4] as usize];
    let user_base_token_account = account_keys[ix.accounts[5] as usize];
    let user_quote_token_account = account_keys[ix.accounts[6] as usize];
    let pool_base_token_account = account_keys[ix.accounts[7] as usize];
    let pool_quote_token_account = account_keys[ix.accounts[8] as usize];
    let protocol_fee_recipient: Pubkey = account_keys[ix.accounts[9] as usize];
    let protocol_fee_recipient_token_account = account_keys[ix.accounts[10] as usize];
    let base_token_program = account_keys[ix.accounts[11] as usize];
    let quote_token_program = account_keys[ix.accounts[12] as usize];
    let system_program = account_keys[ix.accounts[13] as usize];
    let associated_token_program = account_keys[ix.accounts[14] as usize];
    let event_authority = account_keys[ix.accounts[15] as usize];
    let program = account_keys[ix.accounts[16] as usize];

    Ok(PumpSwapTXInfo {
        blockhash,
        pool,
        user,
        global_config,
        base_mint,
        quote_mint,
        user_base_token_account,
        user_quote_token_account,
        pool_base_token_account,
        pool_quote_token_account,
        protocol_fee_recipient,
        protocol_fee_recipient_token_account,
        base_token_program,
        quote_token_program,
        system_program,
        associated_token_program,
        event_authority,
        program,
        amounts: Amounts::default(),
    })
}

impl Default for PumpSwapTXInfo {
    fn default() -> Self {
        PumpSwapTXInfo::new()
    }
}

impl PumpSwapTXInfo {
    fn new() -> Self {
        Self {
            blockhash: Hash::new_unique(),
            pool: Pubkey::new_unique(),
            user: Pubkey::new_unique(),
            global_config: Pubkey::new_unique(),
            base_mint: Pubkey::new_unique(),
            quote_mint: Pubkey::new_unique(),
            user_base_token_account: Pubkey::new_unique(),
            user_quote_token_account: Pubkey::new_unique(),
            pool_base_token_account: Pubkey::new_unique(),
            pool_quote_token_account: Pubkey::new_unique(),
            protocol_fee_recipient: Pubkey::new_unique(),
            protocol_fee_recipient_token_account: Pubkey::new_unique(),
            base_token_program: Pubkey::new_unique(),
            quote_token_program: Pubkey::new_unique(),
            system_program: Pubkey::new_unique(),
            associated_token_program: Pubkey::new_unique(),
            event_authority: Pubkey::new_unique(),
            program: Pubkey::new_unique(),
            amounts: Amounts::default(),
        }
    }

    pub fn from_buy_tx(
        tx: &VersionedTransaction,
        ix: &CompiledInstruction,
    ) -> Result<Self, TXParserError> {
        let mut base_tx = create_base_tx_pumpswap(tx, ix)?;

        let base_amount_out = u64::from_le_bytes(
            ix.data[ix.data.len() - 16..ix.data.len() - 8]
                .try_into()
                .unwrap(),
        );
        let max_quote_amount_in = u64::from_le_bytes(
            ix.data[ix.data.len() - 8..ix.data.len()]
                .try_into()
                .unwrap(),
        );

        base_tx.amounts = Amounts::BuyAmounts(BuyAmounts {
            base_amount_out,
            max_quote_amount_in,
        });

        Ok(base_tx)
    }

    pub fn from_sell_tx(
        tx: &VersionedTransaction,
        ix: &CompiledInstruction,
    ) -> Result<Self, TXParserError> {
        let mut base_tx = create_base_tx_pumpswap(tx, ix)?;

        let base_amount_in = u64::from_le_bytes(
            ix.data[ix.data.len() - 16..ix.data.len() - 8]
                .try_into()
                .unwrap(),
        );
        let max_quote_amount_out = u64::from_le_bytes(
            ix.data[ix.data.len() - 8..ix.data.len()]
                .try_into()
                .unwrap(),
        );

        base_tx.amounts = Amounts::SellAmounts(SellAmounts {
            base_amount_in,
            max_quote_amount_out,
        });

        Ok(base_tx)
    }
}

#[derive(Debug)]
pub struct PumpFunTXInfo {
    pub blockhash: Hash,
    pub fee_recipient: Pubkey,
    pub mint: Pubkey,
    pub bonding_curve: Pubkey,
    pub associated_bonding_curve: Pubkey,
    pub associated_user: Pubkey,
    pub user: Pubkey,
    pub amounts: Amounts,
    pub creator: Pubkey,
}

fn create_base_tx_pumpfun(
    tx: &VersionedTransaction,
    ix: &CompiledInstruction,
) -> Result<PumpFunTXInfo, TXParserError> {
    let blockhash = tx.message.recent_blockhash().to_owned();
    let account_keys = tx.message.static_account_keys();

    if account_keys.len() < 12 {
        println!("This transaction doesn't even have enough wallets.");
        return Err(TXParserError::MissingAccountKeys);
    }

    if account_keys.len() < ix.accounts.iter().max().unwrap().to_owned() as usize {
        println!("Missing account keys!");
        println!(
            "Account Keys: {:?}\nIX Account keys: {:?}",
            account_keys, ix.accounts
        );
        return Err(TXParserError::NotEnoughWallets);
    }

    // 17 acccounts in there
    let fee_recipient = account_keys[ix.accounts[1] as usize];
    let mint = account_keys[ix.accounts[2] as usize];
    let bonding_curve = account_keys[ix.accounts[3] as usize];
    let associated_bonding_curve = account_keys[ix.accounts[4] as usize];
    let associated_user = account_keys[ix.accounts[5] as usize];
    let user = account_keys[ix.accounts[6] as usize];
    let creator = account_keys[ix.accounts[10] as usize];

    Ok(PumpFunTXInfo {
        blockhash,
        fee_recipient,
        mint,
        bonding_curve,
        associated_bonding_curve,
        associated_user,
        user,
        amounts: Amounts::default(),
        creator,
    })
}

fn create_base_create_tx_pumpfun(
    tx: &VersionedTransaction,
    ix: &CompiledInstruction,
) -> Result<PumpFunTXInfo, TXParserError> {
    let blockhash = tx.message.recent_blockhash().to_owned();
    let account_keys = tx.message.static_account_keys();

    // Resolve the n-th account of an instruction to its pubkey, without ever panicking on
    // malformed or lookup-table based transactions.
    let key_at = |ix: &CompiledInstruction, n: usize| -> Result<Pubkey, TXParserError> {
        ix.accounts
            .get(n)
            .and_then(|idx| account_keys.get(*idx as usize))
            .copied()
            .ok_or(TXParserError::MissingAccountKeys)
    };

    let mint = key_at(ix, 0)?;
    let bonding_curve = key_at(ix, 2)?;
    let associated_bonding_curve = key_at(ix, 4)?;
    let default_key = Pubkey::default();

    let mut creator: Pubkey = Pubkey::default();
    let mut amounts = Amounts::default();

    for instructions in tx.message.instructions() {
        let Some(program_id) = account_keys.get(instructions.program_id_index as usize) else {
            continue;
        };

        if *program_id != PUMP_FUN_PROGRAM_ID {
            continue;
        }

        // discriminator (8) + amount (8) + max_sol_cost (8); newer layouts may append more bytes.
        let data = &instructions.data;
        if data.len() < 24 || data[0..8] != PUMP_ORDER_BUY {
            continue;
        }

        let Ok(buyer_creator) = key_at(instructions, 9) else {
            continue;
        };
        creator = buyer_creator;

        let base_amount_out = u64::from_le_bytes(data[8..16].try_into().unwrap_or_default());
        let max_quote_amount_in = u64::from_le_bytes(data[16..24].try_into().unwrap_or_default());

        amounts = Amounts::BuyAmounts(BuyAmounts {
            base_amount_out,
            max_quote_amount_in,
        });
        break;
    }

    Ok(PumpFunTXInfo {
        blockhash,
        fee_recipient: default_key,
        mint,
        bonding_curve,
        associated_bonding_curve,
        associated_user: default_key,
        user: default_key,
        amounts,
        creator,
    })
}

/// Token name and symbol from a Pump.fun `create` instruction: after the 8-byte
/// discriminator come borsh strings (u32 little-endian length + UTF-8 bytes).
/// Returns `None` on anything malformed instead of panicking.
pub fn parse_create_metadata(data: &[u8]) -> Option<(String, String)> {
    fn read_string(data: &[u8], offset: usize) -> Option<(String, usize)> {
        let len_end = offset.checked_add(4)?;
        let len = u32::from_le_bytes(data.get(offset..len_end)?.try_into().ok()?) as usize;
        let end = len_end.checked_add(len)?;
        let text = std::str::from_utf8(data.get(len_end..end)?).ok()?;
        Some((text.to_string(), end))
    }

    let (name, next) = read_string(data, 8)?;
    let (symbol, _) = read_string(data, next)?;
    Some((name, symbol))
}

impl Default for PumpFunTXInfo {
    fn default() -> Self {
        PumpFunTXInfo::new()
    }
}

impl PumpFunTXInfo {
    fn new() -> Self {
        Self {
            blockhash: Hash::new_unique(),
            fee_recipient: Pubkey::new_unique(),
            mint: Pubkey::new_unique(),
            bonding_curve: Pubkey::new_unique(),
            associated_bonding_curve: Pubkey::new_unique(),
            associated_user: Pubkey::new_unique(),
            user: Pubkey::new_unique(),
            amounts: Amounts::default(),
            creator: Pubkey::new_unique(),
        }
    }

    pub fn from_create_tx(
        tx: &VersionedTransaction,
        ix: &CompiledInstruction,
    ) -> Result<Self, TXParserError> {
        create_base_create_tx_pumpfun(tx, ix)
    }

    // pub fn from_axiom_tx(
    //     tx: &VersionedTransaction,
    //     ix: &CompiledInstruction,
    // ) -> Result<Self, TXParserError> {
    // }

    pub fn from_buy_tx(
        tx: &VersionedTransaction,
        ix: &CompiledInstruction,
    ) -> Result<Self, TXParserError> {
        let mut base_tx = create_base_tx_pumpfun(tx, ix)?;

        let base_amount_out = u64::from_le_bytes(
            ix.data[ix.data.len() - 16..ix.data.len() - 8]
                .try_into()
                .unwrap_or_default(),
        );
        let max_quote_amount_in = u64::from_le_bytes(
            ix.data[ix.data.len() - 8..ix.data.len()]
                .try_into()
                .unwrap_or_default(),
        );

        base_tx.amounts = Amounts::BuyAmounts(BuyAmounts {
            base_amount_out,
            max_quote_amount_in,
        });

        Ok(base_tx)
    }

    pub fn from_sell_tx(
        tx: &VersionedTransaction,
        ix: &CompiledInstruction,
    ) -> Result<Self, TXParserError> {
        let mut base_tx = create_base_tx_pumpfun(tx, ix)?;

        let base_amount_in = u64::from_le_bytes(
            ix.data[ix.data.len() - 16..ix.data.len() - 8]
                .try_into()
                .unwrap(),
        );
        let max_quote_amount_out = u64::from_le_bytes(
            ix.data[ix.data.len() - 8..ix.data.len()]
                .try_into()
                .unwrap(),
        );

        base_tx.amounts = Amounts::SellAmounts(SellAmounts {
            base_amount_in,
            max_quote_amount_out,
        });

        Ok(base_tx)
    }
}

pub const PUMP_ORDER_BUY: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];
pub const PUMP_ORDER_SELL: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::message::{Message, VersionedMessage};

    const CREATE: [u8; 8] = [24, 30, 200, 40, 5, 28, 7, 119];

    fn tx_with(keys: Vec<Pubkey>, instructions: Vec<CompiledInstruction>) -> VersionedTransaction {
        let mut message = Message::new_with_blockhash(&[], None, &Hash::default());
        message.account_keys = keys;
        message.instructions = instructions;
        VersionedTransaction {
            signatures: vec![],
            message: VersionedMessage::Legacy(message),
        }
    }

    fn buy_data(amount: u64, max_cost: u64) -> Vec<u8> {
        let mut data = PUMP_ORDER_BUY.to_vec();
        data.extend_from_slice(&amount.to_le_bytes());
        data.extend_from_slice(&max_cost.to_le_bytes());
        data
    }

    #[test]
    fn create_tx_with_missing_accounts_is_an_error_not_a_panic() {
        let keys = vec![PUMP_FUN_PROGRAM_ID, Pubkey::new_unique()];
        let create = CompiledInstruction::new_from_raw_parts(0, CREATE.to_vec(), vec![1, 1]);
        let tx = tx_with(keys, vec![create.clone()]);
        assert!(PumpFunTXInfo::from_create_tx(&tx, &create).is_err());

        let empty = CompiledInstruction::new_from_raw_parts(0, CREATE.to_vec(), vec![]);
        let tx = tx_with(vec![PUMP_FUN_PROGRAM_ID], vec![empty.clone()]);
        assert!(PumpFunTXInfo::from_create_tx(&tx, &empty).is_err());
    }

    #[test]
    fn create_tx_extracts_creator_buy() {
        let keys: Vec<Pubkey> = std::iter::once(PUMP_FUN_PROGRAM_ID)
            .chain((0..11).map(|_| Pubkey::new_unique()))
            .collect();
        let accounts: Vec<u8> = (1..=10).collect();
        let create = CompiledInstruction::new_from_raw_parts(0, CREATE.to_vec(), accounts.clone());
        let buy = CompiledInstruction::new_from_raw_parts(0, buy_data(5, 7), accounts);
        let tx = tx_with(keys.clone(), vec![create.clone(), buy]);

        let info = PumpFunTXInfo::from_create_tx(&tx, &create).unwrap();
        assert_eq!(info.mint, keys[1]);
        assert_eq!(info.creator, keys[10]);
        assert_eq!(
            info.amounts,
            Amounts::BuyAmounts(BuyAmounts {
                base_amount_out: 5,
                max_quote_amount_in: 7
            })
        );
    }

    fn borsh_string(s: &str) -> Vec<u8> {
        let mut v = (s.len() as u32).to_le_bytes().to_vec();
        v.extend_from_slice(s.as_bytes());
        v
    }

    #[test]
    fn create_metadata_is_parsed_and_malformed_data_is_rejected() {
        let mut data = CREATE.to_vec();
        data.extend(borsh_string("Moon Cat"));
        data.extend(borsh_string("MCAT"));
        data.extend(borsh_string("https://example.com/m.json"));
        assert_eq!(
            parse_create_metadata(&data),
            Some(("Moon Cat".to_string(), "MCAT".to_string()))
        );

        assert_eq!(parse_create_metadata(&CREATE), None);
        assert_eq!(parse_create_metadata(&[]), None);
        // Length prefix larger than the remaining bytes.
        let mut lying = CREATE.to_vec();
        lying.extend_from_slice(&u32::MAX.to_le_bytes());
        lying.extend_from_slice(b"abc");
        assert_eq!(parse_create_metadata(&lying), None);
        // Invalid UTF-8.
        let mut bad = CREATE.to_vec();
        bad.extend_from_slice(&2u32.to_le_bytes());
        bad.extend_from_slice(&[0xff, 0xfe]);
        assert_eq!(parse_create_metadata(&bad), None);
    }

    #[test]
    fn short_buy_data_is_ignored() {
        let keys: Vec<Pubkey> = std::iter::once(PUMP_FUN_PROGRAM_ID)
            .chain((0..11).map(|_| Pubkey::new_unique()))
            .collect();
        let accounts: Vec<u8> = (1..=10).collect();
        let create = CompiledInstruction::new_from_raw_parts(0, CREATE.to_vec(), accounts.clone());
        let short_buy =
            CompiledInstruction::new_from_raw_parts(0, PUMP_ORDER_BUY.to_vec(), accounts);
        let tx = tx_with(keys, vec![create.clone(), short_buy]);

        let info = PumpFunTXInfo::from_create_tx(&tx, &create).unwrap();
        assert_eq!(info.amounts, Amounts::Unknown);
    }
}
