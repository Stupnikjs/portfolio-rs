//! Indices régionaux et matières premières suivis par défaut dans la
//! matrice de corrélation, indépendamment de ce qui est détenu en
//! portefeuille -- servent de référence pour juger la diversification
//! réelle des positions.

/// Un benchmark suivi dans la matrice de corrélation.
pub struct Benchmark {
    /// Nom affiché comme ligne/colonne dans dashboard.json -- à garder
    /// synchronisé avec `BENCHMARK_LABELS` côté dashboard.py.
    pub label: &'static str,
    /// Ticker Yahoo Finance.
    pub ticker: &'static str,
    /// `true` : la série est convertie en EUR (point de vue d'un investisseur
    /// en euros, comme les actifs détenus). `false` : série brute, pour les
    /// indices qui ne sont pas un prix d'actif -- convertir l'indice dollar
    /// (DX-Y.NYB) en euros n'a aucun sens et brouille ses rendements.
    pub in_eur: bool,
}

const fn eur(label: &'static str, ticker: &'static str) -> Benchmark {
    Benchmark { label, ticker, in_eur: true }
}

const fn raw(label: &'static str, ticker: &'static str) -> Benchmark {
    Benchmark { label, ticker, in_eur: false }
}

pub const BENCHMARKS: &[Benchmark] = &[
    eur("MSCI China", "MCHI"),
    eur("CAC 40", "^FCHI"),
    eur("S&P 500", "^GSPC"),
    eur("Or", "GC=F"),
    eur("Argent", "SI=F"),
    eur("NVIDIA", "NVDA"),
    raw("Indice Dollar", "DX-Y.NYB"),
    eur("NASDAQ", "^NDX"),
    eur("SU.FR", "SU.PA"),
];
