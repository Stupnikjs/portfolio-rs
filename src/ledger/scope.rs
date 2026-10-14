//! Restriction du wallet à un compte fiscal (CTO, PEA, Binance...).

use crate::ledger::realized::{tax_account, TaxAccount};
use crate::store::serialize::TxStore;

/// Sous-wallet ne contenant que les transactions de `account`.
///
/// `add_transactions` recrée les actifs depuis les clones portés par les
/// transactions : les tickers résolus plus tard sur `tx_store.assets`
/// seraient perdus, et `historical_price_eur` renvoie 0.0 pour une action
/// sans ticker. On recopie donc les actifs d'origine.
pub fn scope_to_account(tx_store: &TxStore, account: TaxAccount) -> TxStore {
    scope_to_accounts(tx_store, &[account])
}

/// Comme `scope_to_account`, pour plusieurs comptes vus comme un seul
/// (ex. CTO + PEA quand les virements entre les deux sont ignorés).
pub fn scope_to_accounts(tx_store: &TxStore, accounts: &[TaxAccount]) -> TxStore {
    let mut scoped = TxStore::new();
    scoped.add_transactions(
        tx_store.transactions.iter().filter(|tx| accounts.contains(&tax_account(tx))).cloned().collect(),
    );
    for (symbol, asset) in scoped.assets.iter_mut() {
        if let Some(original) = tx_store.assets.get(symbol) {
            *asset = original.clone();
        }
    }
    scoped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::TransactionKind::Buy;
    use crate::schema::{AssetKind, Platform};
    use crate::testutil::{store, tx};

    const CTO_FILE: &str = "./data/raw/accounts/account.xlsx";
    const PEA_FILE: &str = "./data/raw/accounts/account_pea.xlsx";

    fn xtb(file: &str, symbol: &str, id: &str) -> crate::schema::Transaction {
        let mut t = tx(Buy, symbol, AssetKind::Stock, 1.0, 100.0, "2025-01-01T10:00:00Z", id);
        t.platform = Platform::Xtb;
        t.source_file = file.to_string();
        t
    }

    #[test]
    fn keeps_only_the_transactions_of_the_requested_account() {
        let s = store(vec![xtb(CTO_FILE, "AIR", "a"), xtb(PEA_FILE, "SAN", "b")]);
        let scoped = scope_to_account(&s, TaxAccount::Cto);

        assert_eq!(scoped.transactions.len(), 1);
        assert!(scoped.assets.contains_key("AIR"));
        assert!(!scoped.assets.contains_key("SAN"));
    }

    #[test]
    fn keeps_the_identifiers_resolved_on_the_original_store() {
        let mut s = store(vec![xtb(CTO_FILE, "AIR", "a")]);
        s.assets.get_mut("AIR").unwrap().identifiers.ticker = Some("AIR.XX".to_string());

        let scoped = scope_to_account(&s, TaxAccount::Cto);

        assert_eq!(scoped.assets["AIR"].identifiers.ticker.as_deref(), Some("AIR.XX"));
    }
}
