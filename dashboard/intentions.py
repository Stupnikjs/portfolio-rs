"""Intentions de trade : on note l'idée d'abord, on n'exécute qu'après un délai de réflexion.

Persistant dans data/intentions.json (écrit par le dashboard, jamais par le pipeline Rust).
Le rapprochement avec les vrais trades (dashboard.json) est recalculé à chaque affichage :
l'état « exécutée » n'est jamais stocké.

Une intention : {id, created_at (UTC, ISO), symbol, kind ("Buy"|"Sell"), reason,
delay_hours, cancelled_at (None ou ISO)}.
"""
from __future__ import annotations

import json
import uuid
from datetime import datetime, timezone
from pathlib import Path

import pandas as pd

from config import INTENTION_MAX_AGE_DAYS, ROOT_DIR

INTENTIONS_PATH = ROOT_DIR / "data" / "intentions.json"

DONE_ON_TIME = "Exécutée à temps"
DONE_TOO_EARLY = "Exécutée trop tôt"
WAITING = "En attente"
READY = "Prête"
CANCELLED = "Annulée"
EXPIRED = "Expirée"


# --- Persistance ---------------------------------------------------------------------------

def load_intentions(path: Path = INTENTIONS_PATH) -> list[dict]:
    if not path.exists():
        return []
    return json.loads(path.read_text(encoding="utf-8"))


def save_intentions(items: list[dict], path: Path = INTENTIONS_PATH) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(items, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


def add_intention(items: list[dict], symbol: str, kind: str, reason: str, delay_hours: int,
                  now: datetime | None = None) -> dict:
    symbol, reason = symbol.strip(), reason.strip()
    if not symbol:
        raise ValueError("Choisis un actif.")
    if kind not in ("Buy", "Sell"):
        raise ValueError(f"Sens inconnu : {kind!r}")
    if not reason:
        raise ValueError("Une raison est obligatoire : c'est elle que tu relis avant d'exécuter.")
    item = {
        "id": uuid.uuid4().hex[:8],
        "created_at": (now or datetime.now(timezone.utc)).isoformat(),
        "symbol": symbol,
        "kind": kind,
        "reason": reason,
        "delay_hours": int(delay_hours),
        "cancelled_at": None,
    }
    items.append(item)
    return item


def cancel_intention(items: list[dict], intention_id: str, now: datetime | None = None) -> None:
    for it in items:
        if it["id"] == intention_id:
            it["cancelled_at"] = (now or datetime.now(timezone.utc)).isoformat()
            return
    raise ValueError(f"Intention {intention_id} introuvable.")


# --- Rapprochement intentions / trades réels -----------------------------------------------

def reconcile(items: list[dict], trades: pd.DataFrame, now: datetime | None = None,
              max_age_days: int = INTENTION_MAX_AGE_DAYS) -> tuple[pd.DataFrame, pd.DataFrame]:
    """Rapproche un pour un les intentions et les trades réels.

    `trades` : colonnes time (datetime avec fuseau), symbol, kind ("Buy"|"Sell").
    Un trade est rattaché à la plus ancienne intention non annulée et encore libre, de même actif et
    même sens, créée avant lui et au plus `max_age_days` jours plus tôt.
    Le suivi commence à la première intention : les trades antérieurs ne sont pas jugés.

    Renvoie (intentions avec statut, trades sans intention).
    """
    now_ts = pd.Timestamp(now or datetime.now(timezone.utc))
    if now_ts.tzinfo is None:
        now_ts = now_ts.tz_localize("UTC")
    max_age = pd.Timedelta(days=max_age_days)

    rows = []
    for it in items:
        created = pd.Timestamp(it["created_at"])
        rows.append({
            **it,
            "created": created,
            "unlock": created + pd.Timedelta(hours=it["delay_hours"]),
            "executed_at": None,
            "status": "",
        })
    rows.sort(key=lambda r: r["created"])

    unplanned = []
    if rows:
        start = rows[0]["created"]
        for _, t in trades.sort_values("time").iterrows():
            if t["time"] < start:
                continue
            match = next(
                (r for r in rows
                 if r["executed_at"] is None and not r["cancelled_at"]
                 and r["symbol"].upper() == str(t["symbol"]).upper() and r["kind"] == t["kind"]
                 and r["created"] <= t["time"] <= r["created"] + max_age),
                None,
            )
            if match is not None:
                match["executed_at"] = t["time"]
            else:
                unplanned.append(t)

    for r in rows:
        if r["cancelled_at"]:
            r["status"] = CANCELLED
        elif r["executed_at"] is not None:
            r["status"] = DONE_ON_TIME if r["executed_at"] >= r["unlock"] else DONE_TOO_EARLY
        elif now_ts < r["unlock"]:
            r["status"] = WAITING
        elif now_ts <= r["created"] + max_age:
            r["status"] = READY
        else:
            r["status"] = EXPIRED

    table = pd.DataFrame(rows)
    unplanned_df = pd.DataFrame(unplanned) if unplanned else trades.iloc[0:0]
    return table, unplanned_df
