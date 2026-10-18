# Rust contracts and source-bound parity

Reference: `miuuyy/codex-chatgpt-web` v6.1.10,
`8645a89e6a5fbe011e1bfbb0821a386ca7477ad5`. Rust implementation lives
in `crates/contracts`; its upstream MIT copyright and full permission notice
are preserved in `crates/contracts/LICENSE.upstream`. This is policy parity,
not full application or authenticated browser parity.

## Public integration API

The crate is `cw-contracts` (`cw_contracts` in Rust). Root exports:
`LauncherState`, `BrowserInteractionMode::{Automatic,Manual}` and
`Surface::{Browser,Setup,Mcp,Activity,Limits,Settings}`. Public modules are
`state`, `models`, `limits`. Serialized contract fields use camelCase;
model catalog JSON retains upstream snake_case wire keys.

| Module | Public API | Contract |
| --- | --- | --- |
| `state` | `LauncherState::default()` | Exact 22-key `state.cjs` initial state, including null language/reminder/smoke version, Automatic, sidebar 252, predictions on, no invented setup status |
| `state` | `LauncherState::normalize(serde_json::Value) -> cw_contracts::Result<Self>` | Version fallback, field repair, onboarding/core-setup mode rules, deprecated bridgeEnabled removal, unknown field preservation |
| `state` | `set_preference(&mut self, key: &str, value: &Value) -> Result<()>` | Only keepRunningOnClose and showBrowserDuringTurns; true only for JSON boolean true; unknown preference errors |
| `state` | `set_sidebar(bool, f64)`, `set_mcp_step(u8)`, `set_language(&str)` | Explicit Result validation; sidebar rejects outside 240–420 before rounding; five supported languages |
| `state` | `StateStore::{open,read,update,set_preference}` | Explicit file path, atomic private persistence, clone read, version forced to 1, memory advances after successful save |
| `state` | `next_session_refresh_reminder_at(f64) -> Result<String>` | 48-hour delay, finite timestamp validation, UTC millisecond ISO string |
| `models` | `ChatGptWebAccountCapabilities` | Required solAvailable/proAvailable, optional extraHighAvailable, experimentalBiggerContext, browserInteractionMode, zeroRiskProEnabled; callers must supply observed capabilities |
| `models` | `BackendModel`, `ModelFamily`, `CodexEffort`, `AdapterEffort`, `ChatGptWebModelRoute` | Distinct Luna/Sol/ZeroRisk/ZeroRiskPro, 5.6/6 families, legacy Ultra versus adapter Max; ChatGptWeb-prefixed aliases for model/effort/family types |
| `models` | `all_model_routes()`, `available_chat_gpt_web_model_routes(&caps, include_legacy) -> Result<Vec<_>>` | All 16 pinned routes and exact picker ordering; account gating; hidden legacy identities |
| `models` | `require_chat_gpt_web_model_route(id, &caps, reasoning: Option<&str>) -> Result<_>` | Account/mode checks, immutable legacy effort bindings, current grouped effort selection |
| `models` | `chat_gpt_web_route_efforts(&route, &caps)` | Independently gated Extra High; no manufactured Pro eligibility |
| `models` | `resolve_chat_gpt_web_context_limits(backend, effort, &caps, family) -> Result<ChatGptWebContextLimits>` | Exact standard/Bigger/Pro/Luna/manual budgets, GPT-6 Instant exception, explicit unsupported effort errors |
| `models` | `resolve_chat_gpt_web_transport_limits(backend, effort, &caps) -> Result<ChatGptWebTransportLimits>` | Independent composer-character and message-token bounds, absent values omitted |
| `models` | `resolve_chat_gpt_web_message_token_budget(backend, effort, &caps, image_tokens)`, `resolve_chat_gpt_web_staging_token_budget(backend, effort, &caps)` | Fixed platform/image reserves and repeated Plus Instant headroom; Result; rejects manual backend for automatic budgets |
| `models` | `chat_gpt_web_image_token_reserve(detail)`, `supports_chat_gpt_web_bigger_context(backend, effort, family)` | Original image reserve versus ordinary detail; exact staged-context eligibility |
| `models` | `build_chat_gpt_web_model(&template, &route, &caps, compatibility_v1) -> Result<Value>` | Native metadata cloning, routed context, effort metadata, stripped compaction/availability notices, removed service tiers, compatibility priority bounds |
| `models` | `augment_native_model_catalog(&catalog, &caps, full_mode, compatibility_v1, context_override: Option<u64>) -> Result<Value>` | Removes old web rows, selects native template, preserves native rows, explicit V1 and native maximum override; never substitutes a fake template |
| `limits` | `LimitsStore::open(path) -> Result<Self, LimitsStoreError>` | Strict version/schema, bounded regular file, no credentials/prompt payloads |
| `limits` | `configure(&mut self, account_key, Plan, now_ms) -> Result<LimitsSnapshot>` | Explicit opt-in, 64-hex normalized account key, Pro100/Pro200/Unsupported, preserved retained account history |
| `limits` | `record(&mut self, account_key, id, ReceiptModel, at_ms, now_ms) -> Result<bool>` | Active-account only, duplicate receipt protection, skew rejection, seven-day retention, no cap enforcement, bounded capacity |
| `limits` | `snapshot(&mut self, now_ms) -> Result<LimitsSnapshot>`, `matches_account(&self, key)` | Rolling exclusive lower boundary/inclusive now, unknown-Pro uncertainty, no invented remaining/reset time |

Time arguments are nonnegative integer milliseconds bounded to JavaScript's safe
integer maximum. Invalid JSON model/plan variants fail serde deserialization.
All failures expose explicit Result; limits errors additionally expose upstream
`LIMITS_*` code strings. Storage paths are caller-selected: this crate does not
read ~/.codex or select production paths, start processes, or probe accounts.

## Source mapping

- `launcher/electron/state.cjs`, `languages.json`, `launcher/src/types.ts`,
  main-process `launcher:set-preference` / sidebar / MCP-step handlers → `state`.
- `src/chatgpt-web-models.ts`, `src/model-catalog.ts` → `models`.
- `launcher/electron/limits-store.cjs`, `limits-policy.json`,
  `launcher/src/limits-types.ts` → `limits`.
- Corresponding launcher state/limits tests and TS model/catalog tests informed
  Rust behavioral regressions. Pinned defaults and route descriptions are JSON
  data included at compile time; no JavaScript runtime, Bun/Node wrapper or
  runtime source evaluation is used.

## Drift review

`python3 tools/upstream.py check` compares pinned source SHA, every source file's
SHA256 (including newly added untracked nonignored files), and all 54 exposed
preload methods, IPC operation kinds and channels. Missing files, source/API
additions, modified source bytes or a new commit fail closed.

`python3 tools/upstream.py update-inspect --source /path/to/candidate` prints the
candidate inventory and differences and also exits nonzero on drift. Both commands preserve the reference checkout and lock. Without candidate
options they are read-only. Candidate checkout/fetch, when requested, is confined
to a separate clone described below. Neither command accepts an update, starts
setup/serve, updates the app, nor touches production. Review and mapping must
precede a separate deliberate lock edit. The script's reference SHA must
also be reviewed for a new release. `upstream.lock.json` is initially bound to
the pinned reference, not generated from an arbitrary future candidate.

`python3 tools/upstream.py test` changes source bytes and adds a preload API in
a temporary Git fixture, checks both commands fail, and proves the lock was not
rewritten. `cargo test -p cw-contracts -j 2` exercises only this crate.

## Explicitly unported / proof boundaries

- Electron IPC subscription delivery, native window/navigation actions, browser
  state snapshots, smoke tests, login/logout, passkey flow, DOM model detection,
  usage-account probing and limits-controller async account/receipt orchestration.
- Runtime-changing preference handlers (`setBiggerContext`,
  `setComposerPredictions`, `setSkillAttachments`, `setFreshConversationPerTurn`,
  `setUseSavedChats`, `setAutoApproveToolCalls`, `setZeroRiskPro`,
  `setBrowserInteractionMode`) require runtime/browser ownership, credentials
  checks, setup effects and feedback. Ordinary preference operations here do
  not pretend those effects completed.
- ECMAScript's implementation-dependent permissive `Date.parse` grammar:
  normalization accepts RFC3339, RFC2822 and YYYY-MM-DD persisted reminders;
  arbitrary locale-specific legacy date strings are repaired to null. Millisecond
  reminder generation is implemented for chrono's representable date range.
- Raw `createStateStore.update` can persist deliberately invalid field types in
  JavaScript; the Rust typed update rejects those values rather than persisting
  an invalid `LauncherState`. Valid updates preserve extension fields.
- The TS route union restricts manual/automatic backend combinations at compile
  time; the Rust route record is flat for application composition. Use route
  lookup functions for validated source-owned routes rather than constructing
  arbitrary manual/backend combinations.
- Windows-specific atomic rename retry delays are not ported. Private atomic
  replacement is exercised on macOS; Windows behavior remains unverified.
- No authenticated account availability, actual sends, production route,
  deployment, browser/visual parity or whole-app parity is claimed.

## Verification recorded 2026-10-18

- `cargo test -p cw-contracts -j 2`: **11 behavioral tests passed**, no failures;
  one crate-scoped Cargo invocation, including private state persistence,
  corruption/write-failure preservation, capacity/retention, account/route/context
  matrix and native catalog compatibility/override behavior.
- `rustfmt --check --edition 2024` over this crate's Rust sources/tests: passed.
- `python3 tools/upstream.py check`: pinned SHA matched, **321 files and 54 preload
  methods** matched.
- `python3 tools/upstream.py test`: temporary-source drift regression passed;
  both check and update-inspect returned failure with an unchanged lock.
- `git -C vendor/upstream status --short`: empty. No setup/serve, browser, tunnel,
  account, production configuration or VPS operation was performed.


## Asset regeneration sidecar

`tools/extract-assets.py` generates the exact pinned `copyFor(language)` and
`limitsCopyFor(language)` exports for **en, zh-CN, zh-TW, ja, ko**, plus language
metadata, 24 source icons, `BrandMark` as `icons/brand.svg`, and the upstream MIT
license. It validates the upstream source SHA and full file inventory **before**
executing either pure translation module. It requires an explicit Node22+ binary
and invokes `--experimental-strip-types` only at build time. The Rust application
reads generated assets and has no Node/Bun runtime wrapper.

```sh
python3 tools/extract-assets.py --node "$HOME/.nvm/versions/node/v22.23.3/bin/node" --check
python3 tools/extract-assets.py --node "$HOME/.nvm/versions/node/v22.23.3/bin/node"
python3 tools/extract-assets.py --node "$HOME/.nvm/versions/node/v22.23.3/bin/node" --test
```

`--output /tmp/generated-assets` directs generation to a separate directory.
`--check` is read-only and fails on any missing/changed generated file. Every
invocation prints the expected per-file SHA256 inventory; generation rewrites
only differing generated paths and preserves unrelated assets, including the
separate GPUI license. Unknown JSX shapes or language inventories fail for review.
No translation text, connector names, SVG geometry or licensing was rewritten:
the initial roundtrip matched all pre-existing generated asset bytes exactly.
Only `assets/limits-copy.json` was added, containing all five upstream limits
translations.

Verified hashes:

| Generated asset | SHA256 |
| --- | --- |
| `copy.json` | `5191bcdbc7d39415b072196fce90602fd93c64a710562577028a86a74d5cf5f1` |
| `languages.json` | `491a0037b77b753f572a7f2a86ce41661a6a74cd7c665a93935d433c29d0746d` |
| `icons/brand.svg` | `897a04e5656aae069edc09370428cb9eaca161b2bf1e146669b569e83d5bd221` |
| `limits-copy.json` | `aa22e48f881ab8c9dab66499f8ed9ab966657dad9a0f6b8b7d8910d52589cee1` |
| `UPSTREAM-LICENSE` | `d8b06e8d802722749b8f09e4ed1f2439fde0995a09b7e47c65dca3cc54d43d6e` |

## Inspecting a future commit without accepting it

```sh
python3 tools/upstream.py update-inspect --new-sha FULL_40_HEX_SHA
python3 tools/upstream.py update-inspect --new-sha FULL_40_HEX_SHA --checkout /tmp/codex-candidate
```

These commands clone repository objects to a **separate** checkout and select a
detached exact commit there. The source checkout, its HEAD/refs, app assets and
baseline lock remain unchanged. The default clone is temporary and removed
following inspection. `--checkout` retains a candidate for review and must name a
new, nonexistent directory outside the reference. If the SHA is not present
locally, an explicit `--fetch-from REPOSITORY` fetches it into that candidate
clone only; inspection never fetches into the pinned vendor repository.
No automatic remote contact occurs without `--fetch-from`.

The JSON report includes `touchedModules`: each added/removed/modified path,
before/after SHA256, and a mapped parity domain (launcher state/UI, browser account
and turns, interaction/context, Responses/MCP/model policy, runtime/integration,
limits estimates, upstream maintenance). Unknown paths are explicitly
`unmapped-review-required`. A new source SHA or changed file/API still exits 1;
inspection never accepts the candidate, updates the app, regenerates assets, or
advances the baseline. A candidate beyond the reference has no asserted release
tag (`tag: null`). This is a review aid, not automatic semantic parity verification.

## CEF patch guard

`upstream.lock.json` additionally pins CEF commit
`1e8870069d8765d7aeb6cc74f1026bd7bca4aed2`, every CEF tracked file's expected SHA256,
the original Cargo manifest SHA256, and the SHA256 of
`tools/patches/gpui-cef-pin.patch`. The accepted manifest is the primary worker's
intentional GPUI revision pin. The guard does not revert or rewrite it.

```sh
python3 tools/upstream.py cef-guard
python3 tools/upstream.py check
```

Both upstream check and update inspection include this guard. It replays the
locked patch against the original CEF manifest in a **temporary directory** and
requires the replayed hash to match the accepted manifest. Changed patch bytes,
failed replay, a different CEF SHA, extra/missing files or unreviewed file contents
fail closed. No patch is applied to the active `vendor/gpui-cef` checkout.

Follow-up verification on 2026-10-18: **3 upstream workflow tests and 2 asset tests
passed**. Tests cover deliberate source/API drift and unchanged locks, a committed
candidate inspected in a detached separate clone, parity-domain/hash reporting,
CEF patch replay and rejected manifest/patch tampering, full asset byte/hash
roundtrip, output tampering/repair, and rejection of modified i18n **before Node
execution**. `check` and `cef-guard` passed. Same-SHA isolated inspection passed
with no touched modules. No additional Cargo test loop or application/UI/backend
file edit was performed for this follow-up.
