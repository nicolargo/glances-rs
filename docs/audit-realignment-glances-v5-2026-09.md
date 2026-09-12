# Audit de réalignement — glances-rs ↔ Glances `develop-v5`

**Date :** 2026-09-12 · **Portée :** API (routes + payloads + modèle d'alerte) des 9 plugins glances-rs.
**Fenêtre analysée :** `9167e21e` (dernier point de synchro) → `origin/develop-v5` tip `84c14da0` = **232 commits**.
**Méthode :** diff des modules Glances v5 pertinents + comparaison à `src/plugins/fields.rs`, `src/api/mod.rs`, `src/alerts.rs`, `docs/api.md`. Lecture seule côté Glances.

---

## 0. Contexte déterminant : deux stacks REST cohabitent

Le repo `develop-v5` porte **deux implémentations REST indépendantes**, toutes deux préfixées `/api/5` :

| Stack | Fichiers | Statut |
|---|---|---|
| **Legacy** | `glances/outputs/glances_restful_api.py` + `plugins/<p>/__init__.py` + `plugin/model.py` | ancien modèle, `fields_description` sans métadonnée d'alerte |
| **Réécriture v5** (`5.0.0a1`, alpha) | `routes_v5.py`, `alerts_v5.py`, `plugin/base_v5.py`, `plugin/thresholds_v5.py`, `plugins/<p>/model_v5.py` | **le contrat que glances-rs suit** |

**glances-rs mirrore la réécriture v5**, pas le legacy (confirmé : `model_v5.py` porte `watched`/`default_thresholds`/`normalize_by` depuis `9167e21e` — c'est la source des tables `fields.rs` ; `routes_v5.py` a `/{plugin}/info` depuis toujours, ce qui résout l'énigme « /info absent du repo » de l'audit précédent : il était dans le *mauvais* module). Tout l'audit ci-dessous porte sur la stack v5.

⚠️ **La réécriture v5 est en alpha active** : `base_v5.py` a pris **+549 lignes** sur la fenêtre. Conséquence directe pour la règle « ne pas doubler Glances v5 » : réaligner seulement les parties **stables/additives**, tenir les mécanismes internes encore mouvants.

---

## 1. Écarts classés par impact

| # | Tag | Écart | Nouveau depuis synchro ? | Repos par défaut impacté ? | Reco |
|---|---|---|---|---|---|
| 1 | payload | Champ `hidden: bool` par item sur `network`/`diskio` | **Oui** | Non (toujours `false` par défaut) | Réaligner (petit) |
| 2 | route | `/all/info`, `/{plugin}/limits`, `/all/limits`, `/args` | **Oui** | — | `/all/info` + `/limits` : réaligner ; `/args` : bas |
| 3 | alert | Glances ship des `default_thresholds` built-in → `_levels` décoré *out-of-box* ; glances-rs = config-only | Non (divergence **délibérée** v0.3.0) | **Oui, par conception** | **Décision produit** (voir §3) |
| 4 | payload | `short_name` ajoutés (load min1/5/15, network interface/Rx·s/Tx·s) | Oui | Non (`/info` only) | Réaligner (trivial) |
| 5 | behaviour | Config opt-in `[network] hide_no_up`/`hide_no_ip`, `[fs] allow` | Oui | Non (défaut off) | Tenir (feature-complétude) |
| 6 | behaviour | `[fs] free_space` → `_metadata["free_space"]` | Oui | Non (défaut off) | Tenir / à clarifier |

### Rien de cassé sur l'existant
Plusieurs fix Glances de la fenêtre touchent des bugs que **glances-rs n'a jamais eus, par architecture** (vérifié) — aucune action :
- `8b89bf68` network « rate 0 dans une direction ne doit pas muter l'autre » → glances-rs calcule `level_for` indépendamment par `(item, field)`, pas de boucle rx/tx combinée.
- `9efec19a` diskio « alerter sur le bitrate, pas le compteur cumulé » → glances-rs émet déjà `read_bytes`/`write_bytes` **comme des débits/s** (pas de compteur cumulé latché), `normalize_by: None`. Même résultat, atteint indépendamment.
- `9029fd66` rate « initialiser un stat nouveau depuis le dernier sample » → glances-rs skippe déjà le diff (pas l'item) pour une interface/disque apparue, et est fortement typé (pas de `KeyError` possible).

---

## 2. Détail des écarts à réaligner

### Gap 1 — champ `hidden` (network, diskio) · [payload, nouveau]
`base_v5.py:56-71` ajoute `_BASE_METADATA_FIELDS["hidden"]` (`unit: bool`, `internal: True`) à **chaque item de collection** dont le plugin déclare `HIDE_ZERO_FIELDS`. network (`["bytes_recv","bytes_sent"]`) et diskio (`["read_bytes","write_bytes"]`) le déclarent → `hidden` est **toujours émis** (valeur `false` quand `hide_zero` off, le défaut). `internal: True` **n'est pas** strippé de l'API (`get_api_payload(keep_internal=True)`, `base_v5.py:1200`) — seul le chemin export le retire. Donc `hidden` est **sur le fil** pour tout item `/api/5/network` et `/api/5/diskio`.

glances-rs : aucun champ `hidden` ; son hiding est name/regex (`KeyFilter`) et **exclut** l'item au lieu de le flaguer.

**Reco :** émettre `hidden: false` sur les items network/diskio (cas toujours-présent) + l'ajouter à `fields.rs` (`internal: true`, `unit: bool`). Le mécanisme sticky `hide_zero`/`hide_threshold_bytes` (config valeur-based) est opt-in → **tenir** (interne encore mouvant en alpha). Petit, faible risque.

### Gap 2 — routes `/limits` + `/all/info` + `/args` · [route, nouveau]
`routes_v5.py` : `/all/info` (L161, `{plugin: fields_description}`), `/{plugin}/limits` (L184), `/all/limits` (L146, seuils **effectifs** = défaut+config fusionnés), `/args` (L174, args CLI sanitizés).

glances-rs expose : `pluginslist`, `all`, `alert`, `{plugin}/info`, `{plugin}`. Manquent les 4.

**Reco :**
- **`/all/info`** : trivial — map de `fields()` sur les 9 plugins, réutilise le handler `/info` de v0.4.0. Réaligner (cheap, cohérent).
- **`/{plugin}/limits` + `/all/limits`** : exposent les seuils *effectifs*. Mais glances-rs ne ship **aucun** défaut (Gap 3) → son « effectif » = seuils configurés seulement, identique au `default_thresholds` de son `/info`. Implémentable et cohérent, **mais la sémantique diffère de Glances** tant que Gap 3 n'est pas tranché. Réaligner **après** décision Gap 3.
- **`/args`** : glances-rs a peu d'args ; valeur faible. Bas.

### Gap 4 — `short_name` · [payload, /info-only, cosmétique]
`model_v5.py` a ajouté des `short_name` : `load.min1/min5/min15` → `"1 min"`/`"5 min"`/`"15 min"` ; `network.interface_name`/`bytes_recv`/`bytes_sent` → `"interface"`/`"Rx/s"`/`"Tx/s"`. glances-rs : `FieldInfo.short_name` structurellement supporté et sérialisé par `/info`, mais `None` partout.

**Reco :** recopier ces `short_name` dans `fields.rs` (sourcer les 9 depuis le serveur live pour être exact). Trivial, impact `/info` seul.

---

## 3. La décision produit : `default_thresholds` built-in · [alert]

**Ce n'est PAS une régression — c'est une divergence délibérée de v0.3.0** (spec alerting §5.1 : config-only, zéro défaut shippé). Elle préexiste à la fenêtre de 232 commits. Mais elle devient **plus visible** maintenant :

- Glances v5 ship de vrais `default_thresholds` par champ surveillé (`cpu.total`/`mem.percent` 50/70/90, `cpu.steal` 5/15/30, `ctx_switches` 25000/37500/50000/cœur, network bandwidth 0.7/0.8/0.9…). → un Glances v5 **stock décore `_levels` out-of-box**.
- glances-rs stock n'émet **jamais** de `_levels` non vide (décore seulement si l'opérateur configure un seuil).
- Les nouvelles routes `/limits` + le `default_thresholds` de `/info` exposent côté Glances les **défauts effectifs** — glances-rs ne peut pas les refléter fidèlement sans une table de défauts.

**Décision à prendre (réversible) :**
- **(A) Garder config-only** (statu quo, conservateur) : glances-rs reste « silencieux par défaut », `/limits` renverrait les seuils configurés seulement, `/info.default_thresholds` idem. Divergence documentée assumée.
- **(B) Shipper une table de défauts built-in** mirroir de Glances v5 : parité totale (alerting out-of-box + `/limits`/`/info` fidèles), mais réintroduit une table de défauts à maintenir **et change le comportement par défaut** (les utilisateurs non configurés verraient soudainement des `_levels`/alertes) → **breaking change de comportement par défaut**, à peser contre le mandat de conservatisme.

**Ma reco :** rester en **(A)** pour l'instant — (B) est un changement de comportement par défaut qui mérite sa propre décision explicite, et la table de défauts v5 bouge encore (alpha). Si parité out-of-box souhaitée un jour, la livrer comme opt-in (`[alerts] use_builtin_defaults = true`) pour ne pas casser les installs existantes.

---

## 4. Recommandation de réalignement

**Réaligner maintenant (stable, additif, faible risque) — candidat v0.4.2 :**
1. `short_name` dans `fields.rs` (Gap 4) — trivial.
2. Champ `hidden: false` sur items network/diskio + `fields.rs` (Gap 1, partie toujours-présente seulement).
3. Route `/all/info` (Gap 2) — réutilise `/info`.

**Tenir jusqu'à stabilisation de Glances v5 alpha :**
- Mécanisme `hide_zero`/`hide_threshold_bytes` sticky (interne `base_v5.py` encore mouvant).
- Config opt-in `hide_no_up`/`hide_no_ip`/`fs allow`/`fs free_space` (défaut off, zéro drift).
- `/args`.

**Décision requise avant d'aller plus loin :**
- Gap 3 (défauts built-in) — pilote `/limits` et la sémantique `/info.default_thresholds`. Tant qu'il n'est pas tranché, `/{plugin}/limits` et `/all/limits` restent en attente.

**Workflow :** pour chaque item réaligné, me confirmer le `curl` de référence depuis ton serveur v5 live (les `short_name` exacts, la forme exacte de `hidden`), puisque le repo alpha peut encore différer de ton serveur.
