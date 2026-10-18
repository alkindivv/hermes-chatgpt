---
created: 2026-10-18
updated: 2026-10-18
tags: [upstream, parity, maintenance]
status: verified
agent: codex
---

# Upstream maintenance and review

This procedure maintains the source relationship described in [the reconstruction
specification](spec.md) and [contracts parity](contracts-parity.md). Inventory
checks detect byte/API changes. Domain mapping directs review. Neither proves
that a Rust implementation semantically matches an updated application.

## Accepted references

- ChatGPT launcher: v6.1.10, `8645a89e6a5fbe011e1bfbb0821a386ca7477ad5`.
- GPUI/CEF: `1e8870069d8765d7aeb6cc74f1026bd7bca4aed2`.
- Local CEF patch: `tools/patches/gpui-cef-pin.patch`, intentionally containing
  **both** the GPUI revision pin and the primary worker's native zoom API.
- GPUI dependency revision: `ef075910c99ce2c8fd07da4174e7c3bb71513f35`.
- Full accepted patch SHA256:
  `f1e77131249183a706c1def721c2484a96402eadce669907e6a0bca29ecbfe09`.
- Patched `crates/gpui-cef/src/platform/cef/mod.rs` SHA256:
  `c1df02fa4be31a9dfefb84ff47a445143f134baa6f2b80e702b8a923750bb49c`.

`upstream.lock.json` pins the launcher source SHA, 321 file hashes, 54 exposed
preload method/channel contracts, CEF source SHA, 24 accepted CEF file hashes,
original CEF Cargo manifest hash, and full local patch hash. This acceptance is
limited to the reviewed changes. It does not accept future vendor edits.

`Webview::set_zoom_factor` accepts finite factors in 0.5–2.0, gets the native
browser host, and converts the factor to a CEF zoom level using
`ln(factor) / ln(1.2)`. It does not evaluate page DOM. Patch replay proves source
reproducibility; it does not prove native zoom behavior or bundle execution.
The primary owns native runtime and UI acceptance.

## 1. Check before inspection

Run from the workspace root:

```sh
python3 tools/upstream.py cef-guard
python3 tools/upstream.py check
python3 tools/upstream.py test
```

A normal check must exit 0. Investigate failures before proceeding; do not replace
hashes to make a check green. Preserve concurrent edits. Do not reset vendor,
change installed apps, profiles/config, tunnels, active processes, or production
routes. This workflow does not contact a VPS or launch upstream setup/serve.

The CEF guard reconstructs the locked files from CEF HEAD in a temporary directory,
checks and applies the **full** patch there, and compares every replayed file's
SHA256 with the lock. It separately compares the active CEF checkout with those
hashes. Missing/additional files, changed source SHA, changed patch bytes, failed
replay, or changed patched content fail closed. It never applies a patch to the
active vendor tree. All temporary replay files are removed after inspection;
no Cargo build, dependency download, CEF binary extraction or build cache is used.

## 2. Inspect one exact candidate in isolation

Resolve a full 40-hex commit identity through the intended upstream release
review. Do not use a moving branch name as an accepted identity. Replace the
example value below with that exact SHA:

```sh
python3 tools/upstream.py update-inspect --new-sha FULL_40_HEX_SHA
```

The default uses a temporary separate clone, selects the detached SHA, prints
JSON, and removes the clone afterward. The pinned source checkout and its refs
remain unchanged. No remote fetch occurs by default. A new SHA normally exits
**1**, meaning unreviewed drift was found. This is expected and does not authorize
baseline acceptance. Exit 1 with `Upstream inspection failed` instead indicates
an operational/parser failure, which must be resolved before using the report.

For source review, retain a new candidate directory:

```sh
python3 tools/upstream.py update-inspect --new-sha FULL_40_HEX_SHA --checkout /tmp/codex-upstream-candidate
```

The destination must not exist and must be outside the pinned source. Remove
only that explicitly task-owned candidate when the review is finished. With
limited disk space, inspect one source candidate at a time; do not create a build
or CEF cache in it. A retained clone remains if later inspection fails, so inspect
that path before any cleanup.

If the exact commit is unavailable locally, append an explicit
`--fetch-from https://github.com/miuuyy/codex-chatgpt-web.git`. Fetch writes only
to the separate candidate clone. It does not update the pinned checkout's refs.
No candidate code, setup/install script, service or UI is executed by inspection.

The report contains `candidate`, `unreviewedChanges`, `touchedModules`, and
`cefPatchGuard`. Each touched module includes its path, old/new SHA256 and a
heuristic domain. Unknown paths are `unmapped-review-required`; manually classify
them. For a commit beyond the accepted reference, `tag: null` avoids asserting
an unverified release name. An empty touched-module list with a new source SHA
still requires identity/history review.

## 3. Review source changes by domain

In the retained candidate, compare the accepted SHA to the candidate without
changing either tree:

```sh
git -C /tmp/codex-upstream-candidate diff --stat 8645a89e6a5fbe011e1bfbb0821a386ca7477ad5 FULL_40_HEX_SHA
git -C /tmp/codex-upstream-candidate diff 8645a89e6a5fbe011e1bfbb0821a386ca7477ad5 FULL_40_HEX_SHA -- launcher/electron/preload.cjs
```

| Report domain | Required source review | Rust/application review and evidence |
| --- | --- | --- |
| `launcher-state-ui` | State defaults/repair, languages/copy, preload IPC, onboarding, layout/navigation and feedback | `crates/contracts` state, generated assets, native UI handlers; state/serde tests and matching native screenshots where affected |
| `browser-account-turns` | Browser lifecycle, account model observations, login, tabs, task/epoch ownership, cancellation, DOM and attachment acceptance | Browser/backend adapter; isolated fixtures plus native browser proof; authenticated flows need an explicitly authorized isolated account |
| `interaction-context` | Automatic/Zero Risk distinctions, confirmation deadlines, retained/fresh/saved chats, compaction and Bigger Context | State/model policy plus runtime/browser behavior; negative mode tests and turn/context evidence |
| `responses-mcp-model-policy` | Model slugs/families/efforts, capabilities and budgets, native catalog, Responses/SSE, broker and MCP ownership | Contracts and bridge regression tests; no manufactured model/account availability or successful tool dispatch |
| `runtime-integration-updates` | Journals/restoration, process ownership, credentials, setup, supervisor, tunnel and update behavior | Runtime tests with temporary files and test-owned children; production actions require separately authorized operational work |
| `limits-estimates` | Account scope, opt-in, receipts/dedup, rolling boundaries, uncertainty, limits copy/policy | Contracts limits tests and controller/browser review; observed usage remains an estimate, not server quota/reset proof |
| `upstream-maintenance-review` | Dependency/packaging/license/docs/install changes and revised acceptance claims | Review affected artifact lineage, packaging and maintenance instructions; preserve attribution |
| `unmapped-review-required` | Read and classify the actual changed file and its callers/tests | Assign an owner and evidence requirement before acceptance |

For every changed preload method/channel, record its Rust handler or explicitly
unported behavior. The inventory is an API change detector, not an implementation
map. Read changed callers and tests as well as the changed declaration. A domain
label can be wrong or cover several behaviors; reviewers must correct that
classification rather than treating it as semantic approval.

Record each affected behavior's source paths, previous/new contract, Rust paths,
owner, tests run, and remaining gaps in a review/PR. Add regression cases before
porting changed behavior. A passing old fixture suite cannot prove a new contract
was ported. Do not execute candidate TypeScript simply to obtain expected outputs.

## 4. Port and validate before accepting a new baseline

1. Make scoped Rust/browser-asset changes and keep unrelated edits intact.
2. Run affected contract/protocol/runtime tests using the shared two-job build
   budget. Coordinate with the primary; do not start competing Cargo loops.
3. Regenerate assets only from a reviewed, pinned source using the explicit
   Node22+ build-time generator documented in [contracts parity](contracts-parity.md).
   The generator rejects unreviewed candidates. For a new version, source approval,
   pin/lock/generator changes and fixture updates must be one explicit reviewed
   change; do not bypass its guard to run candidate modules.
4. Record native/browser, visual, authenticated and production evidence separately
   from source/fixture evidence. Mark unported behaviors by name. A source sync or
   successful compile does not establish full-app parity.
5. Only after the source-domain review and required implementation evidence,
   deliberately review changes to the vendor reference identity, `PIN`/release
   metadata in `tools/upstream.py`, `upstream.lock.json`, generated fixtures/assets,
   parity documentation and any affected packaging references. The tool has no
   accept/auto-port command. Never paste the candidate inventory over the lock
   without the corresponding implementation and review record.
6. Re-run drift and CEF guards against the intentionally accepted references.
   Archive actual command results and unresolved acceptance boundaries. This
   maintenance procedure itself does not authorize changing an installed app,
   production route or running integration.

## CEF changes require a separate patch review

The current patch edits only `Cargo.toml` and
`crates/gpui-cef/src/platform/cef/mod.rs`. To review another intentional local
change, first inspect `git -C vendor/gpui-cef diff HEAD --` and identify exactly
which hunks the primary accepted. Capture only reviewed paths with `git diff HEAD
--binary -- <reviewed paths>` into the patch. Verify the CEF source commit remains
correct. Update the patch hash and only the explicitly accepted file hashes in
the lock; do not refresh all hashes from a dirty checkout. Keep original-manifest
identity pinned. This is a manual reviewed action, never part of `check`.

Then run `test`, `cef-guard`, `check` and `update-inspect` again. If a future patch
adds/deletes files absent from CEF HEAD, extend the reconstruction/replay logic and
its tests explicitly; the current guard intentionally expects all locked files
to exist in the original commit. No patch application to active vendor is needed
for guard verification. Replaying a patch into a real checkout is a separate
implementation step owned by that checkout's primary worker.

## Validation for the accepted pin + zoom patch

On 2026-10-18, `python3 tools/upstream.py test` passed all three tests. Coverage
includes source/API drift with unchanged locks, a committed candidate inspected
in a separate detached clone with path/domain/hash reporting, full CEF patch
replay, zoom-source tampering, replayed-source hash mismatch, patch-byte drift
and malformed patch rejection. `cef-guard` and `check` passed. Default
`update-inspect` and isolated same-SHA inspection returned no unreviewed changes;
a future SHA is not accepted by those checks. These checks use temporary source
files only and provide no claim of native zoom or whole-app runtime parity.
