# Audit footprint — v0.3.0 (alerting)

> Gate d'acceptation de la phase F (plan `docs/superpowers/plans/2026-06-14-alerting.md`).
> v0.3.0 ajoute l'alerting (`_levels` par champ, `/api/5/alert`, hystérésis
> `min_duration`) et un champ de parité réseau (`bytes_speed_rate_per_sec`).
> Le mandat footprint impose que le **comportement par défaut** (aucun seuil
> configuré — la majorité des utilisateurs) reste indistinct de v0.2.0.

Mesures avec `scripts/footprint.sh` sur `/api/5/all` (neuf plugins), binaire
release, même méthode que l'audit v0.2.0. Rappel : la RSS varie de quelques
centaines de Ko d'un run à l'autre (l'audit v0.2.0 lui-même relevait 5.5 et
6.4 MiB @100 req/s sur des sous-runs différents) — lire les chiffres à ±0.5 MiB.

## Axe 1 — config par défaut (aucun seuil) : le chemin qui doit rester gratuit

| charge | RSS v0.2.0 (ref) | RSS v0.3.0 défaut | CPU v0.3.0 |
|---|---|---|---|
| repos | 3.8 MiB | **3.3 MiB** | ≈ 0 % |
| 2 req/s | 3.8 MiB | 3.9 MiB | 0.20 % |
| 10 req/s | 4.0 MiB | 3.9 MiB | 0.40 % |
| 100 req/s | 5.5 MiB | **6.1 MiB** | 1.60 % |

**Verdict : aucune régression mesurable.** Repos et charge modérée sont dans
le bruit (repos même plus bas). Le +0.6 MiB @100 req/s est dans la variance
run-à-run (cf. la fourchette 5.5–6.4 de v0.2.0), CPU identique (1.60 %). C'est
le résultat attendu : `Alerts::observe` **retourne tôt** quand le plugin n'a
aucun seuil configuré (`src/alerts.rs`), donc le cycle de collecte par défaut
ne prend pas le lock, n'alloue pas de map `_levels`, ne clone aucune clé et ne
purge aucun état — le chemin est (quasi-)gratuit, comme l'exige le §9.

L'unique ajout **inconditionnel** (collecté même sans alerting) est la lecture
de `/sys/class/net/<iface>/speed` par le plugin `network` pour
`bytes_speed_rate_per_sec` : un `read_to_string` par interface par cycle, noyé
dans les chiffres ci-dessus (invisible).

## Axe 2 — pire cas (seuils sur les neuf champs alertables)

Config de test : seuils sur `mem.percent`, `cpu.total`, `load.min15`,
`memswap.percent`, `fs.percent`, `diskio.read_bytes`/`write_bytes`,
`network.bytes_recv`/`bytes_sent`.

| charge | RSS défaut | RSS pire cas | CPU pire cas |
|---|---|---|---|
| repos | 3.3 MiB | 3.8 MiB | ≈ 0 % |
| 2 req/s | 3.9 MiB | 4.0 MiB | 0.30 % |
| 10 req/s | 3.9 MiB | 4.4 MiB | 0.40 % |
| 100 req/s | 6.1 MiB | **7.7 MiB** | 1.80 % |

Le surcoût alerting activé partout : **+~1.6 MiB @100 req/s, +0.2 pt CPU**.
Il vient du travail par cycle (calcul des niveaux + réécriture de `_levels`)
et de la sérialisation de `_levels` par requête. Modéré et proportionné à la
fonctionnalité ; un déploiement typique ne configure des seuils que sur
quelques champs, pas les neuf.

## Binaire

| | v0.2.0 | v0.3.0 |
|---|---|---|
| taille release | 2.1 MiB | **2.2 MiB** |

+~0.1 MiB pour le module `alerts` (aucune nouvelle dépendance — l'horodatage
ISO-8601 est fait main pour éviter `chrono`/`time`).

## Parité fonctionnelle (vérifiée sur le binaire release)

- `mem._levels = {"percent": {"level": "careful", "prominent": true}}` — scalaire, top-level.
- `fs._levels = {"/": {"percent": {"level": "ok", "prominent": false}}, …}` — collection, keyé par `mnt_point` ; forme **identique** à un serveur Glances v5 (develop-v5) de référence.
- `network` expose `bytes_speed_rate_per_sec` ; `_levels` vide quand la vitesse de lien est inconnue (diviseur 0 → skip), exactement la sémantique `normalize_by` documentée.

## Conclusion

Le gate §9 est tenu : **config par défaut indistincte de v0.2.0**, surcoût
alerting borné et opt-in, binaire quasi inchangé, aucune nouvelle dépendance.
Aucune régression bloquante.
