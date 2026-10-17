//! Outils de développement (hors métier) : concatène les sources du projet
//! dans un .txt à donner à un LLM.
//!
//! Sorti de `main.rs` : ce n'est pas du pipeline portefeuille.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;

const SEPARATOR: &str = "################################################################################";

/// Fichiers d'extension `extension` sous `dir`, récursivement.
fn collect_files(dir: &Path, extension: &str, out: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_files(&path, extension, out)?;
        } else if path.extension().map_or(false, |ext| ext == extension) {
            out.push(path);
        }
    }
    Ok(())
}

/// (dossier, chemin relatif) affichés dans l'en-tête d'un fichier.
/// Un fichier hors de `root_dir` (Cargo.toml...) est rattaché à ".".
fn header_parts(path: &str, root_dir: &str) -> (String, String) {
    match path.strip_prefix(&format!("{root_dir}/")) {
        Some(rel) => {
            let dir = rel.rsplit_once('/').map_or_else(|| root_dir.to_string(), |(d, _)| format!("{root_dir}/{d}"));
            (dir, rel.to_string())
        }
        None => (".".to_string(), path.to_string()),
    }
}

fn render_file(path: &Path, root_dir: &str) -> Result<String> {
    let normalized = path.to_string_lossy().replace('\\', "/");
    let (dossier, relatif) = header_parts(&normalized, root_dir);
    let fichier = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let content = fs::read_to_string(path)?;

    Ok(format!(
        "{SEPARATOR}\n# Dossier : {dossier}\n# Fichier : {fichier}\n# Chemin relatif : {relatif}\n{SEPARATOR}\n\n{content}\n\n"
    ))
}

/// Concatène `prepend_files` (dans l'ordre donné), puis les fichiers
/// `extension` de `root_dir` triés par chemin.
pub fn render_bundle(root_dir: &str, extension: &str, prepend_files: &[&str]) -> Result<String> {
    let mut files: Vec<PathBuf> = Vec::new();

    for prepend in prepend_files {
        let p = Path::new(prepend);
        if p.exists() {
            files.push(p.to_path_buf());
        } else {
            println!("[Omis] Fichier à inclure introuvable : {prepend}");
        }
    }

    let mut sources = Vec::new();
    if Path::new(root_dir).exists() {
        collect_files(Path::new(root_dir), extension, &mut sources)?;
    } else {
        println!("[Omis] Dossier introuvable pour la génération : {root_dir}");
    }
    sources.sort();
    files.extend(sources);

    let mut output = String::new();
    for file in &files {
        output.push_str(&render_file(file, root_dir)?);
    }
    Ok(output)
}

pub fn write_bundle(root_dir: &str, extension: &str, output_file: &str, prepend_files: &[&str]) -> Result<()> {
    fs::write(output_file, render_bundle(root_dir, extension, prepend_files)?)?;
    println!("{output_file} généré avec succès.");
    Ok(())
}

pub fn dump_for_llm() -> Result<()> {
    let mut rust_bundle = String::new();

    // 1. Tous les Cargo.toml du workspace (racine + crates)
    let cargo_files = [
        "Cargo.toml",
        "portfolio-rs/Cargo.toml", // Au cas où tu en as laissé un dans le sous-dossier
        "portfolio-rs/core/Cargo.toml",
        "portfolio-rs/pricer/Cargo.toml",
        "portfolio-rs/model/Cargo.toml",
        "portfolio-rs/parse/Cargo.toml",
    ];
    for file in &cargo_files {
        let path = Path::new(file);
        if path.exists() {
            rust_bundle.push_str(&render_file(path, ".")?);
        }
    }

    // 2. Tout le code source (.rs) de chaque crate
    for crate_name in &["core", "pricer", "model", "parse"] {
        let src_dir = format!("portfolio-rs/{crate_name}/src");
        if Path::new(&src_dir).exists() {
            rust_bundle.push_str(&render_bundle(&src_dir, "rs", &[])?);
        }
    }

    std::fs::write("forllm.txt", rust_bundle)?;
    println!("forllm.txt généré avec succès (Workspace complet).");

    // 3. Dashboard Python (si le dossier dashboard est aussi dans le dossier parent)
    let py_bundle = render_bundle("dashboard", "py", &[])?;
    std::fs::write("forllmdashboard.txt", py_bundle)?;
    println!("forllmdashboard.txt généré avec succès.");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_of_a_nested_file_keeps_its_subfolder() {
        assert_eq!(
            header_parts("core/src/ledger/history.rs", "core/src"),
            ("core/src/ledger".to_string(), "ledger/history.rs".to_string())
        );
    }

    #[test]
    fn header_of_a_file_at_the_root_of_the_dump_folder() {
        assert_eq!(header_parts("core/src/lib.rs", "core/src"), ("core/src".to_string(), "lib.rs".to_string()));
    }

    #[test]
    fn header_of_a_file_outside_the_dump_folder_is_attached_to_dot() {
        assert_eq!(header_parts("Cargo.toml", "core/src"), (".".to_string(), "Cargo.toml".to_string()));
    }

    #[test]
    fn a_sibling_folder_sharing_the_prefix_is_not_confused_with_root() {
        // "core2/src/x.rs" ne commence pas par "core/src/".
        assert_eq!(header_parts("core2/src/x.rs", "core/src"), (".".to_string(), "core2/src/x.rs".to_string()));
    }
}