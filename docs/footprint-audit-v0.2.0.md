# Audit footprint — glances-rs v0.2.0

> Étude des pistes d'optimisation CPU/RAM, correspondant à la **Phase 9** du
> `DEVELOPMENT_PLAN.md` (« Footprint optimization study »). Document de travail
> interne. Chaque piste est un *arbitrage explicite* : gain mesuré ou estimé,
> risque, effort, et impact sur les contrats `ARCHITECTURE.md` (§3 lazy, §5.4
> rate-safety, §8.1 anti-leak).

## Contexte & méthode

- Mesures réalisées sur la machine de dev (16 cœurs), binaire `make build`
  (profil footprint : `lto`, `codegen-units=1`, `strip`, `panic="abort"`),
  scope identique aux **9 plugins** v0.2.0, via `scripts/footprint.sh` sur
  `/api/5/all`.
- Baseline v0.2.0 (état actuel, runtime multi-thread) :

  | charge | RSS | CPU |
  |---|---|---|
  | repos | 5.1 MiB | ≈ 0 % |
  | 2 req/s | 5.4 MiB | 0.40 % |
  | 10 req/s | 5.8 MiB | 0.50 % |
  | 100 req/s | 12.1 MiB | 2.10 % |

## Synthèse priorisée

| # | Piste | Gain mesuré / estimé | Risque | Effort | Verdict |
|---|---|---|---|---|---|
| 1 | Runtime tokio `multi_thread` → `current_thread` | **−18 % RSS repos, −47 % RSS @100 req/s** (mesuré) | Faible | Trivial (1 ligne) | ✅ **Adopté** (Phase 9) |
| 2 | Pré-sérialiser le payload une fois par cycle (au lieu de cloner + re-sérialiser par requête) | CPU @100 req/s ÷ (refresh × req/s) ; ↓ alloc hot-path | Moyen | Moyen | Adopter après mesure (phase 2) |
| 3 | Sampler partagé / lectures `/proc` dédupliquées (§5.2) | Faible à nul tant que `/all` est concurrent | Moyen | Moyen | **Mesurer d'abord, sans doute rejeter** |
| 4 | Poids de `sysinfo` vs usage réel | Binaire + arbre de deps | Moyen | Élevé | Étudier (feature-gating) |
| 5 | Profil build : `opt-level`, allocateur | Taille ↔ RSS | Faible | Faible | Mesurer les deux axes |

---

## Piste 1 — Runtime tokio sur-dimensionné *(headline, mesuré)*

**Constat.** `main.rs` utilise `#[tokio::main]`, donc le runtime **multi-thread
par défaut**, qui crée *un worker par cœur*. Mesure : **17 threads OS** au repos
(1 main + 16 workers) pour un serveur dont la charge la plus lourde mesurée est
2,1 % de CPU. C'est l'anti-pattern « runtime sur-provisionné » : 16 threads
oisifs, leurs piles, et un scheduler work-stealing inutile pour une charge
quasi nulle et I/O-bound.

**Expérience (build jetable `flavor = "current_thread"`, même profil, 9 plugins) :**

| charge | multi-thread (actuel) | current_thread | Δ |
|---|---|---|---|
| threads | 17 | **1** | −16 |
| RSS repos | 5.1 MiB | **4.2 MiB** | **−18 %** |
| RSS @2 req/s | 5.4 MiB | 4.2 MiB | −22 % |
| RSS @10 req/s | 5.8 MiB | 4.4 MiB | −24 % |
| RSS @100 req/s | 12.1 MiB | **6.4 MiB** | **−47 %** |
| CPU @100 req/s | 2.10 % | 1.70 % | ≈ |

Le gain explose sous charge : la croissance RSS de la baseline vient en grande
partie des piles de workers touchées quand le work-stealing réveille les 16
threads. En mono-thread, la RSS reste plate (4.2 → 6.4 MiB).

**Pourquoi c'est sûr ici.** Le travail est trivial et I/O-bound (lectures
`/proc`, `sysinfo`, sérialisation de quelques KB). La concurrence du réveil
`/all` (§5.2) est de la concurrence *asynchrone*, pas du parallélisme : elle
fonctionne à l'identique sur un runtime mono-thread. On ne perd que le
parallélisme multi-cœur, dont cette charge n'a aucun besoin.

**Recommandation.**
```rust
#[tokio::main(flavor = "current_thread")]
```
Option intermédiaire si l'on veut garder une marge de parallélisme :
`#[tokio::main(flavor = "multi_thread", worker_threads = 2)]` — mais la mesure
ne le justifie pas. On peut aussi retirer la feature `rt-multi-thread` de
`tokio` dans `Cargo.toml` (allège l'arbre) une fois `current_thread` adopté.

**Validation.** Suite de tests verte (les tests pilotent le routeur via
`oneshot`, indépendants du flavor) + re-run `scripts/footprint.sh`. Conserver la
ligne dans le tableau footprint du README après adoption.

**✅ Adopté (Phase 9).** `main.rs` passe à `#[tokio::main(flavor =
"current_thread")]` et la feature `rt-multi-thread` est retirée de `tokio`
(`rt` suffit), ce qui réduit aussi le binaire (2.2 → 2.1 MiB). 101 tests verts,
lint clean. Re-mesure (release, 9 plugins) — **meilleure que l'expérience
jetable**, le retrait de feature ayant encore allégé :

| charge | avant (multi-thread) | après (current_thread) | Δ |
|---|---|---|---|
| threads | 17 | 1 | −16 |
| RSS repos | 5.1 MiB | **3.8 MiB** | −25 % |
| RSS @2 req/s | 5.4 MiB | 3.8 MiB | −30 % |
| RSS @10 req/s | 5.8 MiB | 4.0 MiB | −31 % |
| RSS @100 req/s | 12.1 MiB | **5.5 MiB** | **−55 %** |
| CPU @100 req/s | 2.10 % | 1.60 % | −0.5 pt |

Face à Glances 4.5.5 (même scope) : **~28× moins de mémoire au repos, ~21×
sous polling lourd**. Décision enregistrée dans ARCHITECTURE.md §9 ; README
rafraîchi.

---

## Piste 2 — Recompute dans le hot-path : clone + re-sérialisation par requête

**Constat.** Le store est un `HashMap<PluginId, serde_json::Value>`
(`state.rs:31`). Chaque requête :
1. `snapshot(id)` fait `store.read().await.get(&id).cloned()` (`state.rs:77`)
   → **clone profond** de tout l'arbre `Value` ;
2. `Json(value)` re-**sérialise** l'arbre en octets sur le thread de la requête
   (`api/mod.rs:78`, `:69`).

Or le `refresh` par défaut est **2.0 s** (`config.rs:120`) : entre deux cycles
de collecte, la donnée est *identique*. À 100 req/s, on clone et re-sérialise
~200 fois un payload qui ne change qu'une fois toutes les 2 s. C'est
exactement l'anti-pattern « ne pas recomputer à chaque cycle ce qui peut être
mis en cache » — appliqué au mauvais étage (par requête au lieu de par cycle).

**Piste.** Publier une **fois par cycle** la forme déjà sérialisée. La boucle de
collecte (`collector.rs:133`) sérialise le `Value` en `Arc<[u8]>` (ou
`bytes::Bytes`) au moment du `publish`, et le store devient
`HashMap<PluginId, Arc<[u8]>>`. Les handlers renvoient les octets cachés
directement (`([(CONTENT_TYPE, "application/json")], bytes)`), sans clone
profond ni re-sérialisation. Le clone devient un simple bump de compteur de
réf.

- Pour `/api/5/{plugin}` : trivial, on sert les octets tels quels.
- Pour `/api/5/all` : assembler l'objet JSON par **concaténation** des
  fragments cachés (`{"cpu":<frag>,"diskio":<frag>,...}`) plutôt que de
  re-sérialiser une `Map<String,Value>`. Garder l'ordre trié (clé → fragment
  via `BTreeMap`).

**Arbitrage / risque (moyen).**
- Touche `state.rs` (type du store + `snapshot`/`publish`), `collector.rs`
  (sérialiser au publish), `api/mod.rs` (handlers + assemblage `/all`). →
  candidat **stratégie deux phases** : introduire le store sérialisé en
  parallèle, basculer les handlers, puis retirer l'ancien chemin `Value`.
- Coût mémoire : on garde quelques KB d'octets par plugin dans le store (au
  lieu d'un `Value`) — neutre, voire plus léger qu'un `Value` (pas de surcoût
  d'arbre `BTreeMap`/`Vec`).
- À mesurer : le gain CPU à 100 req/s et l'effet RSS. **N'adopter que si la
  mesure le confirme** (précédent Phase 7 : décision enregistrée, pas
  silencieuse).

> Note : combinée à la piste 1, la croissance RSS sous charge devrait être
> quasi éliminée — la 1 retire les piles de workers, la 2 retire l'allocation
> par requête.

---

## Piste 3 — Sampler partagé / lectures `/proc` dédupliquées (§5.2, §3.7)

**Constat.** Plusieurs plugins lisent des sources voisines : `cpu` et `system`
touchent `/proc/stat` / identité noyau ; `mem` et `memswap` lisent
`/proc/meminfo`·`/proc/vmstat`. `network` et `fs` gardent chacun leur handle
`sysinfo` (`Networks`/`Disks`) dans leur `State` — déjà réutilisé entre cycles
(bien), mais un par plugin.

**Arbitrage.** Le `DEVELOPMENT_PLAN.md` §3.7 prévoit un sampler partagé
*seulement si le profiling le justifie*. Or sous `/all` concurrent, chaque
plugin tourne dans sa propre tâche/loop : mutualiser une lecture imposerait une
synchronisation (lock ou barrière) qui **va à l'encontre** de l'architecture
lazy-par-plugin (§3) — chaque collecteur est indépendant et s'arrête seul. Le
coût d'une lecture `/proc/stat` ou `/proc/meminfo` est de l'ordre de la dizaine
de µs ; à refresh = 2 s, le doublon est négligeable.

**Verdict : mesurer d'abord, très probablement rejeter.** Ne pas toucher
l'architecture de réveil pour un gain non démontré. Si un jour `cpu`+`system`
ou `mem`+`memswap` étaient fusionnés, ce serait au niveau *plugin* (un plugin
qui expose deux vues), pas via un sampler global partagé.

---

## Piste 4 — Poids de `sysinfo` vs usage réel

**Constat.** Sur Linux (cible primaire), **5 plugins sur 9 lisent `/proc`/`/sys`
en direct** (`mem`, `cpu`, `system`, `memswap`, `diskio` via `linux.rs`).
`sysinfo` n'est réellement utilisé sur Linux que par `network` (`Networks`),
`fs` (`Disks`), `system` (`host_name`/`kernel_version`) et `load`
(`logical_core_count`) — et comme fallback dégradé hors Linux.

`sysinfo` 0.38 tire un arbre non négligeable et, sur Linux, `Networks` relit
`/proc/net/dev` que l'on pourrait parser directement (comme `network.rs` lit
déjà `/sys/class/net` pour `is_up`/`speed`).

**Piste.** Étudier le **feature-gating** de `sysinfo` (n'activer que les
composants utilisés : `network`, `disk`, `system`) pour alléger l'arbre et le
binaire ; à terme, évaluer le remplacement de `Networks`/`Disks` par des
lecteurs `/proc`·`statvfs` natifs sur Linux, alignés sur le pattern `linux.rs`.

**Arbitrage / risque (élevé, effort élevé).** Refactor qui touche le contrat
multi-plateforme (`sysinfo` est aussi le chemin dégradé macOS/Windows). À faire
en **deux phases** et seulement si l'audit de dépendances (ci-dessous) montre un
gain binaire/RSS réel. Commencer par mesurer : `cargo tree`, taille binaire avec
features réduites, `cargo bloat` sur le binaire release.

---

## Piste 5 — Profil de build

**Constat.** Profil release : `opt-level = 3`, `lto = true`, `codegen-units = 1`,
`strip`, `panic = "abort"`. `opt-level = 3` optimise la vitesse ; pour un
serveur I/O-bound où la vitesse CPU n'est pas le facteur limitant, `opt-level =
"s"`/`"z"` peut **réduire la taille binaire** (et parfois la RSS code).

**Piste.** Mesurer les **deux axes** (taille *et* RSS runtime) avec
`opt-level = "s"` puis `"z"`. Évaluer aussi un allocateur plus léger
(p.ex. l'allocateur système vs un éventuel jemalloc/mimalloc — ici on est déjà
sur l'allocateur système, donc surtout *ne pas* en ajouter un lourd). Adopter ce
que les chiffres favorisent, conformément à la note du profil (« decisions are
recorded »).

---

## Pistes mineures (faible impact, à noter)

- **Construction `serde_json::Value` par cycle** (`json!{...}` dans chaque
  `collect`) : allocation d'un arbre `BTreeMap` à chaque cycle. Largement
  absorbée par la piste 2 (si l'on sérialise au publish, on peut à terme
  sérialiser directement une struct typée `Serialize` sans passer par `Value`
  — gain alloc, mais refactor de tous les plugins ; bas ROI tant que la piste 2
  n'est pas faite).
- **`sort_by(... as_str ...)`** dans `network`/`fs`/`diskio` : compare en
  ré-extrayant les clés depuis le `Value`. Si l'on passe à des structs typées
  (piste 2/mineure), trier sur le champ natif est plus propre et évite les
  `as_str()`. Impact négligeable (quelques items).
- **`regex-lite`** : déjà le bon choix (léger) face à `regex` complet. RAS.

---

## Ordre d'exécution recommandé

1. **Piste 1** (1 ligne, gain mesuré majeur) — adopter, re-mesurer, mettre à
   jour le tableau footprint du README. Retirer la feature `rt-multi-thread`.
2. **Piste 5** (profil build) — rapide, mesurer taille+RSS, décider.
3. **Piste 2** (pré-sérialisation) — prototyper en deux phases, mesurer le gain
   CPU/RSS sous charge, adopter si confirmé.
4. **Piste 4** (audit deps / sysinfo) — étude `cargo tree`/`cargo bloat`, puis
   éventuel feature-gating en deux phases.
5. **Piste 3** — mesurer, puis très probablement documenter le rejet (ne pas
   compromettre §3).

Chaque optimisation conserve les garde-fous §5.4 (rate-safety) et la règle
anti-leak §8.1 ; la suite de tests reste verte et `scripts/footprint.sh` est la
porte d'acceptation (critère de sortie Phase 9).
