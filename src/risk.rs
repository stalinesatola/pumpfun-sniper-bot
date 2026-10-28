use std::collections::HashSet;
use std::env;
use std::str::FromStr;

use anyhow::{bail, Result};
use serde::Serialize;
use solana_sdk::pubkey::Pubkey;

use crate::config::optional_parsed;

const LAMPORTS_PER_SOL: f64 = 1_000_000_000.0;
const MAX_SLIPPAGE_PERCENT: u64 = 50;

/// Guard rails applied before every buy. A limit of `0` disables that limit.
#[derive(Clone, Debug)]
pub struct RiskConfig {
    /// Buys in flight or held at once (`Held` + `Selling` positions).
    pub max_open_positions: usize,
    /// Total SOL this run may spend on buys.
    pub max_session_spend_sol: f64,
    /// Stop buying once the wallet balance is this far below where it started. Open
    /// positions count as spent until sold, so this is deliberately conservative.
    pub max_session_drawdown_sol: f64,
    /// Skip tokens whose creator buys less / more than this at launch (in SOL).
    pub min_dev_buy_sol: f64,
    pub max_dev_buy_sol: f64,
    pub blocked_creators: HashSet<Pubkey>,
    /// Lower-case; matched as substrings of the token name and symbol.
    pub blocked_keywords: Vec<String>,
    /// Extra SOL allowed on top of the buy amount if the price moves before it lands.
    pub buy_slippage_percent: u64,
    /// SOL that must stay in the wallet after a buy (fees, rent, tips).
    pub min_balance_reserve_sol: f64,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_open_positions: 3,
            max_session_spend_sol: 1.0,
            max_session_drawdown_sol: 0.5,
            min_dev_buy_sol: 0.0,
            max_dev_buy_sol: 0.0,
            blocked_creators: HashSet::new(),
            blocked_keywords: Vec::new(),
            buy_slippage_percent: 5,
            min_balance_reserve_sol: 0.01,
        }
    }
}

/// What we know about a token at the moment it is created.
pub struct Candidate<'a> {
    pub creator: &'a Pubkey,
    pub name: Option<&'a str>,
    pub symbol: Option<&'a str>,
    /// The most the creator offered to pay for their own launch buy, in SOL.
    pub dev_buy_sol: f64,
}

/// What the bot currently holds and has spent, read from the shared state.
pub struct Exposure {
    pub open_positions: usize,
    pub session_spent_sol: f64,
    pub balance_sol: Option<f64>,
    pub start_balance_sol: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RiskView {
    pub max_open_positions: usize,
    pub max_session_spend_sol: f64,
    pub max_session_drawdown_sol: f64,
    pub min_dev_buy_sol: f64,
    pub max_dev_buy_sol: f64,
    pub blocked_creators: usize,
    pub blocked_keywords: Vec<String>,
    pub buy_slippage_percent: u64,
    pub min_balance_reserve_sol: f64,
}

fn non_negative(name: &str, v: f64) -> Result<f64> {
    if v.is_finite() && v >= 0.0 {
        Ok(v)
    } else {
        bail!("{name} must be 0 (off) or a positive number, got {v}")
    }
}

fn list(name: &str) -> Vec<String> {
    env::var(name)
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

impl RiskConfig {
    pub fn from_env() -> Result<Self> {
        let d = Self::default();
        let cfg = Self {
            max_open_positions: optional_parsed("MAX_OPEN_POSITIONS", d.max_open_positions)?,
            max_session_spend_sol: non_negative(
                "MAX_SESSION_SPEND_SOL",
                optional_parsed("MAX_SESSION_SPEND_SOL", d.max_session_spend_sol)?,
            )?,
            max_session_drawdown_sol: non_negative(
                "MAX_SESSION_DRAWDOWN_SOL",
                optional_parsed("MAX_SESSION_DRAWDOWN_SOL", d.max_session_drawdown_sol)?,
            )?,
            min_dev_buy_sol: non_negative(
                "MIN_DEV_BUY_SOL",
                optional_parsed("MIN_DEV_BUY_SOL", d.min_dev_buy_sol)?,
            )?,
            max_dev_buy_sol: non_negative(
                "MAX_DEV_BUY_SOL",
                optional_parsed("MAX_DEV_BUY_SOL", d.max_dev_buy_sol)?,
            )?,
            blocked_creators: list("BLOCKED_CREATORS")
                .iter()
                .map(|s| {
                    Pubkey::from_str(s).map_err(|_| {
                        anyhow::anyhow!("BLOCKED_CREATORS has an invalid address: {s}")
                    })
                })
                .collect::<Result<_>>()?,
            blocked_keywords: list("BLOCKED_KEYWORDS")
                .iter()
                .map(|s| s.to_lowercase())
                .collect(),
            buy_slippage_percent: optional_parsed("BUY_SLIPPAGE_PERCENT", d.buy_slippage_percent)?,
            min_balance_reserve_sol: non_negative(
                "MIN_BALANCE_RESERVE_SOL",
                optional_parsed("MIN_BALANCE_RESERVE_SOL", d.min_balance_reserve_sol)?,
            )?,
        };
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        if self.buy_slippage_percent > MAX_SLIPPAGE_PERCENT {
            bail!("BUY_SLIPPAGE_PERCENT must be at most {MAX_SLIPPAGE_PERCENT}");
        }
        if self.max_dev_buy_sol > 0.0 && self.min_dev_buy_sol > self.max_dev_buy_sol {
            bail!("MIN_DEV_BUY_SOL cannot be larger than MAX_DEV_BUY_SOL");
        }
        Ok(())
    }

    pub fn view(&self) -> RiskView {
        RiskView {
            max_open_positions: self.max_open_positions,
            max_session_spend_sol: self.max_session_spend_sol,
            max_session_drawdown_sol: self.max_session_drawdown_sol,
            min_dev_buy_sol: self.min_dev_buy_sol,
            max_dev_buy_sol: self.max_dev_buy_sol,
            blocked_creators: self.blocked_creators.len(),
            blocked_keywords: self.blocked_keywords.clone(),
            buy_slippage_percent: self.buy_slippage_percent,
            min_balance_reserve_sol: self.min_balance_reserve_sol,
        }
    }

    /// `Ok(())` if the buy may go ahead, otherwise the reason it was skipped.
    /// `buy_sol` is the amount about to be spent and `tip_sol` the landing tip.
    pub fn check(
        &self,
        candidate: &Candidate,
        exposure: &Exposure,
        buy_sol: f64,
        tip_sol: f64,
    ) -> Result<(), String> {
        if self.blocked_creators.contains(candidate.creator) {
            return Err(format!("creator {} is blocked", candidate.creator));
        }

        let text = format!(
            "{} {}",
            candidate.name.unwrap_or_default(),
            candidate.symbol.unwrap_or_default()
        )
        .to_lowercase();
        if let Some(word) = self
            .blocked_keywords
            .iter()
            .find(|w| text.contains(w.as_str()))
        {
            return Err(format!("name/symbol contains blocked keyword \"{word}\""));
        }

        if self.min_dev_buy_sol > 0.0 && candidate.dev_buy_sol < self.min_dev_buy_sol {
            return Err(format!(
                "creator buy {:.3} SOL is below the minimum {}",
                candidate.dev_buy_sol, self.min_dev_buy_sol
            ));
        }
        if self.max_dev_buy_sol > 0.0 && candidate.dev_buy_sol > self.max_dev_buy_sol {
            return Err(format!(
                "creator buy {:.3} SOL is above the maximum {}",
                candidate.dev_buy_sol, self.max_dev_buy_sol
            ));
        }

        if self.max_open_positions > 0 && exposure.open_positions >= self.max_open_positions {
            return Err(format!(
                "{} positions already open",
                exposure.open_positions
            ));
        }

        if self.max_session_spend_sol > 0.0
            && exposure.session_spent_sol + buy_sol > self.max_session_spend_sol + 1e-9
        {
            return Err(format!(
                "session spend limit reached ({:.4} of {} SOL)",
                exposure.session_spent_sol, self.max_session_spend_sol
            ));
        }

        if let (Some(balance), Some(start)) = (exposure.balance_sol, exposure.start_balance_sol) {
            let drawdown = start - balance;
            if self.max_session_drawdown_sol > 0.0 && drawdown >= self.max_session_drawdown_sol {
                return Err(format!(
                    "session drawdown limit reached ({drawdown:.4} of {} SOL)",
                    self.max_session_drawdown_sol
                ));
            }
        }

        if let Some(balance) = exposure.balance_sol {
            let needed = buy_sol + tip_sol + self.min_balance_reserve_sol;
            if balance < needed {
                return Err(format!(
                    "wallet balance {balance:.4} SOL is below the {needed:.4} SOL needed"
                ));
            }
        }

        Ok(())
    }
}

pub fn lamports_to_sol(lamports: u64) -> f64 {
    lamports as f64 / LAMPORTS_PER_SOL
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exposure() -> Exposure {
        Exposure {
            open_positions: 0,
            session_spent_sol: 0.0,
            balance_sol: Some(1.0),
            start_balance_sol: Some(1.0),
        }
    }

    fn candidate(creator: &Pubkey) -> Candidate<'_> {
        Candidate {
            creator,
            name: Some("Moon Cat"),
            symbol: Some("MCAT"),
            dev_buy_sol: 1.0,
        }
    }

    fn check(cfg: &RiskConfig, c: &Candidate, e: &Exposure) -> Result<(), String> {
        cfg.check(c, e, 0.01, 0.001)
    }

    #[test]
    fn defaults_allow_a_normal_buy() {
        let creator = Pubkey::new_unique();
        assert!(check(&RiskConfig::default(), &candidate(&creator), &exposure()).is_ok());
    }

    #[test]
    fn blocked_creators_and_keywords() {
        let creator = Pubkey::new_unique();
        let mut cfg = RiskConfig::default();
        cfg.blocked_creators.insert(creator);
        assert!(check(&cfg, &candidate(&creator), &exposure())
            .unwrap_err()
            .contains("blocked"));

        let other = Pubkey::new_unique();
        cfg.blocked_keywords = vec!["cat".into()];
        let err = check(&cfg, &candidate(&other), &exposure()).unwrap_err();
        assert!(err.contains("\"cat\""), "{err}");
        // Missing metadata never matches a keyword.
        let no_meta = Candidate {
            creator: &other,
            name: None,
            symbol: None,
            dev_buy_sol: 1.0,
        };
        assert!(check(&cfg, &no_meta, &exposure()).is_ok());
    }

    #[test]
    fn dev_buy_range() {
        let creator = Pubkey::new_unique();
        let cfg = RiskConfig {
            min_dev_buy_sol: 0.5,
            max_dev_buy_sol: 2.0,
            ..Default::default()
        };
        let mut c = candidate(&creator);
        c.dev_buy_sol = 0.1;
        assert!(check(&cfg, &c, &exposure()).unwrap_err().contains("below"));
        c.dev_buy_sol = 5.0;
        assert!(check(&cfg, &c, &exposure()).unwrap_err().contains("above"));
        c.dev_buy_sol = 1.0;
        assert!(check(&cfg, &c, &exposure()).is_ok());
    }

    #[test]
    fn exposure_limits() {
        let creator = Pubkey::new_unique();
        let c = candidate(&creator);
        let cfg = RiskConfig::default();

        let full = Exposure {
            open_positions: 3,
            ..exposure()
        };
        assert!(check(&cfg, &c, &full).unwrap_err().contains("positions"));

        let spent = Exposure {
            session_spent_sol: 0.995,
            ..exposure()
        };
        assert!(check(&cfg, &c, &spent).unwrap_err().contains("spend"));
        let ok = Exposure {
            session_spent_sol: 0.99,
            ..exposure()
        };
        assert!(
            check(&cfg, &c, &ok).is_ok(),
            "exactly at the limit is still allowed"
        );

        let drawdown = Exposure {
            balance_sol: Some(0.5),
            ..exposure()
        };
        assert!(check(&cfg, &c, &drawdown).unwrap_err().contains("drawdown"));

        let poor = Exposure {
            balance_sol: Some(0.015),
            start_balance_sol: Some(0.02),
            ..exposure()
        };
        assert!(check(&cfg, &c, &poor).unwrap_err().contains("balance"));
    }

    #[test]
    fn zero_disables_a_limit_and_unknown_balance_is_not_blocking() {
        let creator = Pubkey::new_unique();
        let c = candidate(&creator);
        let cfg = RiskConfig {
            max_open_positions: 0,
            max_session_spend_sol: 0.0,
            max_session_drawdown_sol: 0.0,
            ..Default::default()
        };
        let heavy = Exposure {
            open_positions: 99,
            session_spent_sol: 99.0,
            balance_sol: Some(0.5),
            start_balance_sol: Some(100.0),
        };
        assert!(check(&cfg, &c, &heavy).is_ok());

        let unknown = Exposure {
            balance_sol: None,
            start_balance_sol: None,
            ..exposure()
        };
        assert!(check(&RiskConfig::default(), &c, &unknown).is_ok());
    }

    #[test]
    fn validation() {
        assert!(RiskConfig {
            buy_slippage_percent: 51,
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(RiskConfig {
            min_dev_buy_sol: 3.0,
            max_dev_buy_sol: 2.0,
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(RiskConfig::default().validate().is_ok());
    }
}
