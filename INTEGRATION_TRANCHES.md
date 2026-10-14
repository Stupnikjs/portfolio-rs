# Intégration : retour par tranche de cash

Fichiers fournis : `ledger/tranches.rs`, `ledger/scope.rs`, `schema/dashboard.rs`
(remplace l'existant).

## 1. `ledger/mod.rs`

```rust
pub mod scope;
pub mod tranches;
```

## 2. `schema/types.rs`

Ajouter le type :

```rust
/// Nature d'un mouvement de cash (posée par les parsers qui la connaissent : XTB).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CashFlowKind {
    ExternalDeposit,    // virement entrant
    ExternalWithdrawal, // virement sortant
    Income,             // dividende, intérêts
    TradeLeg,           // jambe cash d'un achat / d'une vente
    Tax,                // retenue à la source, taxe
}
```

et le champ dans `Transaction` (à la fin) :

```rust
    #[serde(default)]
    pub cash_flow: Option<CashFlowKind>,
```

Le compilateur listera ensuite chaque littéral `Transaction { .. }` auquel il
manque le champ. Ajouter `cash_flow: None,` dans : `xtb.rs` (closed x2, open x1),
`binance.rs` (x4), `manual.rs` (x1), `testutil.rs` (x1).

## 3. `parse/xtb.rs`

Remplacer `cash_kind_for` :

```rust
use crate::schema::CashFlowKind;

fn cash_kind_for(op_type: &str) -> Option<(TransactionKind, CashFlowKind)> {
    use CashFlowKind::*;
    use TransactionKind as K;
    match op_type {
        "Deposit" => Some((K::Deposit, ExternalDeposit)),
        "Withdrawal" => Some((K::Withdraw, ExternalWithdrawal)), // nom à confirmer sur ton export
        "Free funds interest" | "Dividend" => Some((K::Deposit, Income)),
        "Stock purchase" => Some((K::Withdraw, TradeLeg)),
        "Stock sell" | "Fractional shares" => Some((K::Deposit, TradeLeg)),
        "Withholding tax" | "Tax IFTT" => Some((K::Fee, Tax)),
        _ => None,
    }
}
```

Dans `parse_cash_operations` :

```rust
let (kind, cash_flow) = cash_kind_for(&op_type)
    .ok_or_else(|| anyhow!("Type d'opération Cash non géré: {op_type:?}"))?;
```

et dans le littéral `Transaction` de cette fonction : `cash_flow: Some(cash_flow),`.

## 4. `main.rs`

Imports à ajouter :

```rust
use chrono::{DateTime, Utc};
use portfolio_rs::ledger::tranches::tranche_report;
use portfolio_rs::market::prices::historical_price_eur;
use portfolio_rs::schema::TrancheReport;
```

Nouvelles fonctions :

```rust
/// CTO + PEA regroupés en UN compte : les virements CTO -> PEA sont ignorés par
/// le parser (`PEA deposit`), donc seuls les deux comptes réunis ont un cash cohérent.
fn compute_tranches(tx_store: &TxStore) -> Vec<TrancheReport> {
    println!("\n=== RETOUR PAR TRANCHE DE CASH (CTO + PEA regroupés) ===");
    let live_prices = |symbol: &str, kind: AssetKind, ticker: Option<&str>, at: DateTime<Utc>| {
        historical_price_eur(symbol, at, kind, ticker)
    };

    let report = tranche_report(
        tx_store,
        &[TaxAccount::Cto, TaxAccount::Pea],
        "XTB (CTO + PEA)",
        &live_prices,
        Utc::now(),
    );
    print_tranche_report(&report);
    vec![report]
}

fn print_tranche_report(r: &TrancheReport) {
    println!(
        "--- {} : apports nets {:.2} EUR, valeur {:.2} EUR, gain {:+.2} EUR, TWR {:+.2}%",
        r.account, r.net_contributions_eur, r.current_value_eur, r.total_gain_eur, r.time_weighted_return_pct
    );
    println!("  {:<12} {:>12} {:>10} {:>10} {:>12}", "Date", "Apporté", "ROI", "Annualisé", "Gain total");
    for t in &r.tranches {
        let annualized = t.annualized_pct.map_or("—".to_string(), |a| format!("{a:>+9.1}%"));
        println!(
            "  {:<12} {:>12.2} {:>+9.2}% {:>10} {:>+12.2}{}",
            t.date,
            t.invested_eur,
            t.roi_pct,
            annualized,
            t.realized_gain_eur + t.unrealized_gain_eur,
            if t.inferred { "  (déduit)" } else { "" }
        );
    }
    for warning in &r.warnings {
        println!("  ⚠ {warning}");
    }
}
```

Dans `main`, après l'affichage de `realized_summary` :

```rust
let tranches = compute_tranches(&tx_store);
```

puis passer `tranches` à `build_dashboard` : ajouter le paramètre
`tranches: Vec<TrancheReport>` et le champ `tranches,` dans `DashboardData { .. }`.

## 5. Dashboard Streamlit

Nouvelle page (par exemple `pages/tranches.py`) ou fonction appelée depuis
`app.py`. `data` est le contenu de `dashboard.json` déjà chargé.

```python
import altair as alt
import pandas as pd
import streamlit as st


def render_tranches(data: dict) -> None:
    reports = data.get("tranches", [])
    if not reports:
        st.info("Aucun rapport par tranche : relance `cargo run`.")
        return

    account = st.radio("Compte", [r["account"] for r in reports], horizontal=True)
    report = next(r for r in reports if r["account"] == account)

    c1, c2, c3 = st.columns(3)
    c1.metric("Apports nets", f"{report['net_contributions_eur']:,.0f} €")
    c2.metric(
        "Valeur actuelle",
        f"{report['current_value_eur']:,.0f} €",
        delta=f"{report['total_gain_eur']:+,.0f} €",
    )
    c3.metric("Rendement du compte (TWR)", f"{report['time_weighted_return_pct']:+.1f} %")

    for warning in report["warnings"]:
        st.warning(warning)

    df = pd.DataFrame(report["tranches"])
    if df.empty:
        return
    df["date"] = pd.to_datetime(df["date"])
    df["gain_eur"] = df["realized_gain_eur"] + df["unrealized_gain_eur"]

    st.subheader("ROI de chaque apport depuis sa date d'entrée")
    roi = (
        alt.Chart(df)
        .mark_bar(size=14)
        .encode(
            x=alt.X("date:T", title="Date de l'apport"),
            y=alt.Y("roi_pct:Q", title="ROI (%)"),
            color=alt.condition(alt.datum.roi_pct >= 0, alt.value("#2e7d32"), alt.value("#c62828")),
            tooltip=[
                alt.Tooltip("date:T", title="Apport"),
                alt.Tooltip("invested_eur:Q", title="Apporté (€)", format=",.0f"),
                alt.Tooltip("roi_pct:Q", title="ROI (%)", format="+.2f"),
                alt.Tooltip("annualized_pct:Q", title="Annualisé (%)", format="+.1f"),
                alt.Tooltip("gain_eur:Q", title="Gain (€)", format="+,.0f"),
            ],
        )
    )
    st.altair_chart(roi, use_container_width=True)

    st.subheader("Apporté vs valeur actuelle")
    long = df.melt(
        id_vars="date",
        value_vars=["invested_eur", "current_value_eur"],
        var_name="mesure",
        value_name="eur",
    )
    long["mesure"] = long["mesure"].map({"invested_eur": "Apporté", "current_value_eur": "Valeur actuelle"})
    compare = (
        alt.Chart(long)
        .mark_bar()
        .encode(
            x=alt.X("date:T", title="Date de l'apport"),
            y=alt.Y("eur:Q", title="EUR"),
            color="mesure:N",
            xOffset="mesure:N",
        )
    )
    st.altair_chart(compare, use_container_width=True)

    st.dataframe(
        df[["date", "invested_eur", "current_value_eur", "gain_eur", "roi_pct", "annualized_pct", "inferred"]]
        .sort_values("date", ascending=False),
        use_container_width=True,
        hide_index=True,
    )
```

(`xOffset` demande Altair >= 5, fourni avec les Streamlit récents.)

## À vérifier avant de se fier aux chiffres

- **Nom du type de retrait XTB** : `"Withdrawal"` est une supposition. Un type
  inconnu fait échouer tout l'onglet Cash avec « Type d'opération Cash non géré ».
- **`PEA deposit`** est ignoré volontairement par le parser (tu fais comme si tu
  n'avais qu'un CTO). C'est cohérent seulement si CTO et PEA sont calculés
  ENSEMBLE, ce que fait `compute_tranches` : le cash sorti du CTO et la jambe
  cash d'achat côté PEA s'annulent. Ne calcule jamais le rapport du CTO seul
  ou du PEA seul (le test `cto_alone_keeps_phantom_cash...` documente le piège).
- **Plus-values fiscales** : sans rapport avec cette page. `realized_summary`
  continue de séparer CTO et PEA, ce qui est nécessaire pour la déclaration.
- **Premier run lent** : une valorisation par date d'apport (prix Yahoo), puis
  tout est en cache.
- **Tickers** : `scope_to_account` recopie les actifs du wallet principal. Le
  `main` doit donc résoudre les tickers AVANT d'appeler `compute_tranches`
  (c'est déjà l'ordre actuel).
