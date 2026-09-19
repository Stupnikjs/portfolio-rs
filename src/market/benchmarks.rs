//! Indices régionaux et matières premières suivis par défaut dans la
//! matrice de corrélation, indépendamment de ce qui est détenu en
//! portefeuille -- servent de référence pour juger la diversification
//! réelle des positions.

/// (label affiché, ticker Yahoo Finance). Le label est ce qui apparaît
/// comme nom de ligne/colonne dans dashboard.json -- à garder synchronisé
/// avec la liste `BENCHMARK_LABELS` côté dashboard.py si tu changes les
/// noms ici.
pub const BENCHMARKS: &[(&str, &str)] = &[
    ("MSCI China", "MCHI"),
    ("CAC 40", "^FCHI"),
    ("S&P 500", "^GSPC"),
    ("Or", "GC=F"),
    ("Argent", "SI=F"),
    ("NVIDIA", "NVDA"),
    ("Indice Dollar", "DX-Y.NYB"),
    ("NASDAQ", "^NDX"),
];
 