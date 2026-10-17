#!/usr/bin/env bash
# Fusionne le workspace Cargo (manifestes + code Rust) dans un seul fichier à fournir à un LLM.
# À lancer depuis la racine du workspace (là où se trouve le Cargo.toml avec [workspace]).
set -euo pipefail

OUTPUT_FILE="forllm.txt"
MEMBERS=(core pricer model parse)   # dossiers des crates du workspace

if ! grep -q '^\[workspace\]' Cargo.toml 2>/dev/null; then
    echo "Erreur : lance ce script depuis la racine du workspace (Cargo.toml avec [workspace] introuvable ici)." >&2
    exit 1
fi

: > "$OUTPUT_FILE"

# Ajoute un fichier avec un en-tête propre (ignoré s'il n'existe pas).
add_file() {
    local file_path="$1"
    [ -f "$file_path" ] || return 0
    {
        printf '%s\n' "################################################################################"
        printf '# Fichier : %s\n' "$file_path"
        printf '%s\n' "################################################################################"
        cat "$file_path"
        printf '\n\n'
    } >> "$OUTPUT_FILE"
}

echo "Génération de $OUTPUT_FILE..."

# 1. Manifeste racine du workspace
add_file "Cargo.toml"

# 2. Pour chaque crate : son Cargo.toml, puis tous ses .rs triés (hors dossier target/)
for member in "${MEMBERS[@]}"; do
    add_file "$member/Cargo.toml"
    while IFS= read -r -d '' file; do
        add_file "$file"
    done < <(find "$member" -name "*.rs" -not -path "*/target/*" -print0 | sort -z)
done

nb_files=$(grep -c '^# Fichier : ' "$OUTPUT_FILE")
echo "✅ Terminé : $nb_files fichiers fusionnés dans $OUTPUT_FILE ($(wc -c < "$OUTPUT_FILE") octets)"