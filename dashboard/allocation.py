#!/usr/bin/env python3
"""
Extrait la répartition du portefeuille en % (aucune donnée de valeur)
et l'imprime en texte/markdown, prêt à coller dans un prompt LLM.

Usage:
    python portfolio_allocation_text.py data/dashboard.json
"""

import json
import sys
from pathlib import Path

GROUP_THRESHOLD_PCT = 2.0
EXCLUDE_KINDS = {"Cash"}


def load_allocations(dashboard_path: Path):
    data = json.loads(dashboard_path.read_text())
    assets = data["assets"]
    total = sum(a["value_eur"] for a in assets if a["kind"] not in EXCLUDE_KINDS)

    rows = []
    for a in assets:
        if a["kind"] in EXCLUDE_KINDS or a["value_eur"] <= 0:
            continue
        pct = a["value_eur"] / total * 100.0
        rows.append({"symbol": a["symbol"], "kind": a["kind"], "pct": pct})

    rows.sort(key=lambda r: r["pct"], reverse=True)

    main = [r for r in rows if r["pct"] >= GROUP_THRESHOLD_PCT]
    small = [r for r in rows if r["pct"] < GROUP_THRESHOLD_PCT]
    if small:
        main.append({"symbol": "Autres", "kind": "-", "pct": sum(r["pct"] for r in small)})

    return main


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print("Usage: python portfolio_allocation_text.py <chemin/vers/dashboard.json>")
        sys.exit(1)

    rows = load_allocations(Path(sys.argv[1]))

    print("Répartition du portefeuille (%, aucune donnée de valeur) :")
    for r in rows:
        kind_str = f" [{r['kind']}]" if r["kind"] != "-" else ""
        print(f"- {r['symbol']}{kind_str} : {r['pct']:.1f}%")
