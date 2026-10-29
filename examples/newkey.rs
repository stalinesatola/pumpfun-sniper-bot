//! Makes a new wallet for the bot and prints what goes into `.env`:
//!
//!     cargo run --example newkey
//!
//! Keep the printed key secret (anyone with it controls the wallet) and send only an amount
//! you can afford to lose to the address.
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;

fn main() {
    let keypair = Keypair::new();
    println!("Address (send the SOL here): {}", keypair.pubkey());
    println!("Put this line in .env:\n");
    println!(
        "PVT_KEY={}\n",
        bs58::encode(keypair.to_bytes()).into_string()
    );
    println!("The key is shown only now. Save it somewhere safe and never share or commit it.");
}
