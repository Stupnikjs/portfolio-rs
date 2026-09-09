#!/usr/bin/env python3
"""
Parcourt récursivement un dossier "src" et concatène tous les fichiers .py
trouvés dans un seul fichier de sortie, en ajoutant avant chaque fichier
un en-tête indiquant son dossier et son nom.

Usage :
    python concat_src.py [dossier_src] [fichier_sortie]

Par défaut :
    dossier_src    = "/src"
    fichier_sortie = "forllm.txt"
"""

import os
import sys


def concatener(src_dir: str, output_file: str) -> None:
    if not os.path.isdir(src_dir):
        print(f"Erreur : le dossier '{src_dir}' n'existe pas.")
        sys.exit(1)

    fichiers_py = []
    for racine, dossiers, fichiers in os.walk(src_dir):
        for nom in sorted(fichiers):
            if nom.endswith(".rs") or nom.endswith(".py"):
                chemin_complet = os.path.join(racine, nom)
                fichiers_py.append(chemin_complet)

    fichiers_py.sort()

    with open(output_file, "w", encoding="utf-8") as out:
        for chemin in fichiers_py:
            dossier = os.path.dirname(chemin)
            nom = os.path.basename(chemin)
            chemin_relatif = os.path.relpath(chemin, src_dir)

            out.write("\n")
            out.write("#" * 80 + "\n")
            out.write(f"# Dossier : {dossier}\n")
            out.write(f"# Fichier : {nom}\n")
            out.write(f"# Chemin relatif : {chemin_relatif}\n")
            out.write("#" * 80 + "\n\n")

            with open(chemin, "r", encoding="utf-8") as f:
                out.write(f.read())
                out.write("\n")

    print(f"{len(fichiers_py)} fichier(s) .py concaténé(s) dans '{output_file}'.")


if __name__ == "__main__":
    dossier_src = sys.argv[1] if len(sys.argv) > 1 else "src"
    fichier_sortie = sys.argv[2] if len(sys.argv) > 2 else "forllm.txt"
    concatener(dossier_src, fichier_sortie)