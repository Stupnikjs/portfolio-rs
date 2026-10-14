#!/bin/bash

# Nom du fichier de sortie
OUTPUT_FILE="forllm.txt"

# On vide le fichier de sortie s'il existe déjà
> "$OUTPUT_FILE"

# Fonction pour ajouter un fichier avec un en-tête propre
add_file() {
    local file_path="$1"
    if [ -f "$file_path" ]; then
        echo "################################################################################
# Fichier : $file_path
################################################################################" >> "$OUTPUT_FILE"
        cat "$file_path" >> "$OUTPUT_FILE"
        echo -e "\n\n" >> "$OUTPUT_FILE"
    fi
}

echo "Génération de $OUTPUT_FILE..."

# 1. On ajoute d'abord le Cargo.toml (à la racine)
add_file "Cargo.toml"

# 2. On trouve tous les .rs dans src/, on les trie par nom, et on les ajoute
find src -name "*.rs" | sort | while read -r file; do
    add_file "$file"
done

echo "✅ Terminus ! Tout le code Rust et le Cargo.toml ont été fusionnés dans $OUTPUT_FILE"