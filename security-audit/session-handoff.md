# Obscura — Synthèse de session / handoff pour la suite

> Document de reprise : où on en est, ce qui a été fait, ce qui reste, et comment
> s'y remettre. Date : 2026-06-10.

---

## 1. Contexte projet

- **Obscura** : moteur de navigation headless en Rust (8 crates, ~18,4k LoC), embarque V8,
  parle **CDP** (Chrome DevTools Protocol) + **MCP**. Conçu pour scraping / agents IA.
- **Postulat de menace** : Obscura **récupère et exécute du contenu hostile par conception**.
  Acteurs : **A1** site cible malveillant (JS de page), **A2** client CDP/MCP ou page web locale
  → 127.0.0.1, **A3** entrées locales, **A4** supply chain.
- **Emplacement repo** : `C:\Users\user\AppData\Local\Temp\obscura` (dir Temp — vérifier qu'il
  existe toujours).
- **Git** : remotes `origin` et `fork` pointent tous deux vers le fork **public**
  `charlesmamane26-sketch/obscura` (parent amont = `h4ckf0r0day/obscura`, à NE PAS toucher
  sans coordination). Branche de travail : **`ci/audit-gate-and-hygiene`**.

## 2. Ce qui a été fait (chronologie)

1. **Audit multi-agents** (Workflow, 226 agents) : 62 pistes → **47 findings confirmés**
   (6 Critique, 29 Élevée, 9 Moyenne, 3 Faible), 17 rejetés par vérification adversariale.
2. **Remédiation** : les 6 Critique + la totalité des Élevée + la plupart des Moyenne,
   corrigées **et testées**.
3. **12 commits thématiques** sur la branche (voir §4), poussés sur le fork.
4. **PR #1** (interne au fork, `isCrossRepository:false`) — **5 jobs CI verts**, dont un
   nouveau job `stealth` (libclang) qui build+teste enfin le chemin `wreq`.
5. **Advisory** de divulgation rédigé + **patch bundle** (`git format-patch` + bundle).
6. **Décision utilisateur** : rapport + advisory + patches **commités et poussés en public**
   sous `security-audit/` (divulgation publique assumée — voir §6).

## 3. État courant (au handoff)

- **HEAD** : `d853ae4` (== `fork/ci/audit-gate-and-hygiene`). Working tree **propre**.
- **PR** : https://github.com/charlesmamane26-sketch/obscura/pull/1 — CI verte.
- **Livrables Desktop** : `obscura-audit-roadmap.md` (origine), `obscura-audit-findings.md`
  (rapport), `obscura-security-advisory-DRAFT.md` (advisory), `obscura-patches/` +
  `obscura-audit-fixes.zip` (bundle), ce fichier.
- **Dans le repo** : `security-audit/{audit-findings.md, security-advisory.md, patches/}`.

## 4. Commits de la branche (ordre dépendances)

| # | Commit | Thème |
|---|---|---|
| 1 | `5d03669` fix(browser) | centralise `url_is_file_scheme` (fondation gate file://) |
| 2 | `cd5d82f` fix(net,js) | SSRF resolver+denylist, body cap, header filter |
| 3 | `3e7f36f` fix(net) | validation `Domain=` cookies + perms jar |
| 4 | `a3d3d51` fix(dom) | caps DoS (récursion/arène) |
| 5 | `d93eb26` fix(js) | retrait `Deno.core.ops` du realm de page |
| 6 | `124edce` fix(cdp) | gate file:// interception + WS Origin/Host + logs |
| 7 | `f88d615` fix(mcp) | gate file:// + Content-Length + Origin/Host |
| 8 | `cde9b5a` fix(cli) | flag `--allow-file-access` MCP + redact proxy |
| 9 | `4eab925` ci | `--locked` + checksums release |
| 10 | `8f00781` ci | job build/test stealth (wreq/BoringSSL) |
| 11 | `32fd131` fix(net) | resolver SSRF à la résolution pour stealth wreq |
| 12 | `d853ae4` docs(security) | rapport + advisory + patch bundle dans le repo |

## 5. Findings — statut

**Tous les Critique + Élevée traités**, points clés :
- **SSRF** (parité complète sur les 4 clients : nav, `op_fetch_url`, module loader, stealth) :
  `is_forbidden_ip` canonique + `SsrfDnsResolver`/`StealthSsrfResolver` (résout, rejette IP
  interdite, épingle la connexion → anti-rebinding) ; re-validation des redirections ; cap corps.
- **`Deno.core.ops`** retiré du realm de page (IIFE + `__ops` privé + sous-ensemble sûr
  `{op_dom, op_binding_called}`).
- **file://** : gates MCP (`tool_navigate`/`tool_tab_new`) + CDP (`process_with_interception`).
- **Transports** : allowlist Origin + pin Host (CDP WS + MCP HTTP), cap Content-Length, ACAO:* retiré.
- **Cookies** `Domain=` (host-relationship + PSL best-effort), **header injection**, **DoS caps**,
  **CDP-04** slice UTF-8, **OPS-04** redact proxy, **SUPPLY-01/02**.

### Résidus restants (TODO — voir §7 pour les pointeurs code)
- **COOK-04** : enforcement `SameSite` à l'egress (besoin de threader le site initiateur).
- **OPS-02 (cdp-domains)** : jail de chemin `file://` (besoin d'un répertoire de base ;
  seulement si `--allow-file-access`).
- **AX-02** : marche d'ancêtres O(N·depth) dans `getFullAXTree` (DoS CPU).
- **wreq** : cap de corps **en streaming** (actuellement pré-check `Content-Length` seulement).
- **OPS-03 (concurrency)** : `Runtime.evaluate` boucle sync — watchdog `cdp_watchdog`
  déjà armé ; vérification fine non faite.
- **Info** : gate Semgrep (`continue-on-error`, Rust expérimental), checksum V8 prébuilt (upstream).

## 6. Note divulgation (important)

- La remédiation a été menée en **divulgation responsable** : PR **interne au fork**, l'amont
  non touché. PUIS l'utilisateur a choisi de **committer rapport+advisory en public** (PoC
  d'exploits non corrigés en amont). → De fait, **divulgation publique**.
- **Action recommandée restante** : prévenir vite le mainteneur `h4ckf0r0day` (advisory prêt)
  pour éviter qu'il l'apprenne par un tiers. L'en-tête « Do not publish » de l'advisory est
  désormais incohérent (à nettoyer si l'utilisateur veut).

## 7. Pointeurs code pour les résidus (par où reprendre)

- **SameSite (COOK-04)** : `crates/obscura-net/src/cookies.rs` (`get_cookie_header`) + threader
  le site initiateur depuis `crates/obscura-net/src/client.rs` (`fetch_with_method`) et l'appelant
  navigation. Le champ `same_site` est déjà stocké, jamais appliqué.
- **file:// jail** : `crates/obscura-net/src/client.rs` (`fetch_file_url`) — restreindre à un
  base-dir + canonicalize + refuser symlink/UNC. Câbler une config.
- **AX-02** : `crates/obscura-cdp/src/domains/accessibility.rs` (~`getFullAXTree`/parentId walk).
- **wreq streaming cap** : `crates/obscura-net/src/wreq_client.rs` (`fetch`, lecture du corps) —
  `wreq::Response::chunk()` existe (API miroir reqwest, vérifiée dans le cache cargo).
- **SSRF resolver de référence** : `crates/obscura-net/src/client.rs` (`SsrfDnsResolver`,
  `is_forbidden_ip`, `read_body_capped`) — modèle à réutiliser.

## 8. Gotchas d'environnement (ne pas se faire avoir)

- **Pas de libclang** sur cette machine → `cargo build/test --features stealth` échoue en local
  (boring-sys2/BoringSSL bindgen). **Valider le stealth via la CI** (job `stealth`, libclang installé).
- **Pas de réseau** → utiliser `cargo --offline` (sources crates en cache ; `cargo clean -p`
  peut déclencher un refresh d'index réseau qui échoue).
- **V8** : build-script outputs cachés sous `target/debug/build/v8-*` ; survivent à
  `cargo clean -p <crate>`. **Ne PAS** faire un `cargo clean` complet (rebuild V8 ~5-10 min).
- **html5ever** : parsing super-linéaire sur nesting profond → ne PAS tester les caps DoS avec
  `parse_html("<div>".repeat(100_000))` (hang). Construire l'arbre via l'API DOM (cf. test
  `build_deep_tree` dans `tree_sink.rs`).
- **Git** : warnings LF→CRLF inoffensifs (normalisation).
- **Bash tool** : timeout max 600000 ms (10 min) ; CI à froid peut dépasser → re-watch.

## 9. Commandes utiles

```sh
# Build/test offline (non-stealth)
cargo check --workspace --all-targets --offline
cargo test -p obscura-net --lib --offline
cargo test -p obscura-dom --offline

# Surveiller la CI de la PR
gh pr checks 1 --repo charlesmamane26-sketch/obscura --watch --interval 25

# Appliquer le patch bundle ailleurs
git am -3 security-audit/patches/*.patch     # base = commit 24c95d6
```
