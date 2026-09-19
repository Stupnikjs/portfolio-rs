//! Indices régionaux et matières premières suivis par défaut dans la
//! matrice de corrélation, indépendamment de ce qui est détenu en
//! portefeuille -- servent de référence pour juger la diversification
//! réelle des positions.

/// (label affiché, ticker Yahoo Finance). Le label est ce qui apparaît
/// comme nom de ligne/colonne dans dashboard.json -- à garder synchronisé
/// avec la liste `BENCHMARK_LABELS` côté dashboard.py si tu changes les
/// noms ici.
pub const BENCHMARKS: &[(&str, &str)] = &[
      ("MSTR.US", "MSTR"),
       ("PRIM.US", "PRIM"),
        ("XFVT.DE", "XFVT.DE"), // plusieurs classes de parts homonymes -> ambigu pour la recherche Yahoo
        ("STM.FR", "STMPA.PA"), // STMicroelectronics a changé de symbole Euronext Paris en 2023
        ("CNYA.DE", "CNYA"),  

];
