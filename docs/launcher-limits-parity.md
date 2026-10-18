---
created: 2026-10-18
updated: 2026-10-18
tags: [launcher, limits, parity]
status: implementation-awaiting-integration
agent: codex
---

# Native Limits surface

Presentation port of pinned v6.1.10 `LimitsSurface.tsx`, `limits.css`,
`useLimits.ts` attention predicate, five-language `limits-copy.json` and
`limits-policy.json`. See [contracts parity](contracts-parity.md) for the receipt
store and the distinction between local observations and account availability.
MIT attribution remains in the module and `assets/UPSTREAM-LICENSE`.

## Integration API

New module: `crates/app/src/limits_view.rs`.

```rust
pub(crate) fn render_limits_surface(
    this: &crate::ui::Launcher,
    cx: &mut gpui::Context<crate::ui::Launcher>,
) -> gpui::AnyElement
```

Primary integration steps, intentionally outside this change:

1. Declare `mod limits_view` in the application root; delegate the previous
   `Launcher::render_limits` to this function.
2. Expose existing `Launcher::{button,heading,section,notice,content,icon_button}`
   as `pub(crate)`. `ui::icon`, `manual`, `busy` and the consumed fields are already
   public. The module uses the shared native controls and content scroller.
3. Dispatch `Action::Tracking` to plan setup. Dispatch `Action::RefreshLimits`
   to snapshot refresh separately: the backend originally routed **both** actions
   to account inspection. Presentation does not start either operation itself.
4. Permit this exact upstream URL through the external-link allowlist:
   `https://help.openai.com/en/articles/20001354-gpt-56-and-gpt-6-pro-in-chatgpt`.
   `Action::External` carries that static URL. Retain the primary's source-open
   failure handling; rendering must never claim a link opened successfully.
5. Preserve transient tracker state when updating the snapshot as described below.
   In particular, retain the last good snapshot on refresh errors and associate
   asynchronous results with their initiating request/version. The presentation
   module does not implement polling, cancellation or account probing.

## Actual JSON shape and tracker boundary

`Launcher.limits` currently holds the receipt-store snapshot directly, not
React's `{ snapshot, reading, settingUp, readError, setupError }` wrapper:

```json
{
  "enabled": true,
  "plan": "pro_200",
  "trackingSince": 0,
  "checkedAt": 0,
  "totalMessages": 301,
  "unknownProMessages": 1,
  "incomplete": true,
  "windows": [
    {"id":"gpt-6-pro-7d","model":"gpt-6-pro","durationMs":604800000,"limit":200,"used":250,"uncertainUsed":1}
  ]
}
```

Snapshot `label` fields can remain present; the view localizes model/duration
labels from policy fields, matching upstream. Optional `disabledReason` and
`error` are consumed. The existing unconfigured fallback
`{"enabled":false,"disabledReason":"unconfigured"}` displays setup, not active
tracking, and supplies no invented history/counts.

The renderer additionally accepts these **transient UI-only** fields in that
same object, defaulting to false/absent when not provided:

| Key | Presentation effect |
| --- | --- |
| `reading: true` | Disable refresh; show loading only when no snapshot exists |
| `settingUp: true` | Disable setup/refresh and show localized checking label |
| `readError: string` | Show localized initial/stale-read error, preserving an empty error message as an error indicator |
| `setupError: string` | Show nonempty setup failure before snapshot `error` |

These fields must never be written into the strict limits receipt store. The
existing native `operation == "Checking ChatGPT plan…"` also selects setting-up
presentation until primary tracker integration supplies explicit state.
An object with only tracker keys and no snapshot fields is treated as **no
snapshot**. It can display loading/initial-read errors without enabling setup.
A null snapshot is also supported. The backend currently swallows initial store
open/read errors into its unconfigured fallback; the primary must preserve the
actual failure text for this surface to display it. No failure message or
successful plan observation is manufactured by the renderer.

## Ported presentation behavior

- Localized title/subtitle, refresh and setup/recheck/checking labels; loading,
  read/stale-read/setup/snapshot errors; active/paused/not-configured/unsupported/
  Zero Risk status; checked date; busy browser explanation.
- Tracking only when enabled **and** the plan is Pro100/Pro200 **and** neither
  manual mode nor `disabledReason: zero-risk` is active. Unsupported accounts may
  recheck their plan, matching upstream, but receive no observed account usage.
- Observed model windows, rolling day/week/custom-hour duration, counts and
  public reference caps; progress meter clamped to 0–100% solely for drawing.
  The actual observed count is never clamped to the reference cap.
- Model-specific windows with unknown Pro attribution display **At least**,
  model-total-unknown and uncertainty text and hide their progress meter. Shared
  windows already include unknown Pro messages; they are never added again.
  Attention starts at observed used >= 75% of a positive reference limit.
- Scope warning and attribution explanation; empty-window state; seven-day
  history, unknown-model count and tracking-since time. Retained history remains
  visible in manual or unsupported mode when trackingSince is present.
- All public reference cards: Pro20x (200/week GPT-6 Pro, 170/day GPT-5.6 Sol Pro,
  200/day combined); Pro5x (50/week shared); Business (15/month Standard,
  50/week Premium, explicitly reference-only); Plus/other plans (unknown).
- Upstream reference source/date, **checkedOn 2026-09-19**, formatted as a UTC
  medium date; local snapshot timestamps use the current local timezone.
  Five-language date layouts and number grouping match the pinned source's
  Intl outputs for the tested values, without adding a locale dependency.

No remaining allowance, reset deadline, account capability or lifetime total is
calculated. `limit` is a public reference cap, not a server reading. Absent history
numbers display localized Unknown rather than invented zero. Invalid window data
is omitted; the source store already validates normal snapshots.

## Native styling and proof boundary

The surface follows source order and dark tokens: 8% white borders, 3% control
background, 14/10/4px panel/card/tag radii, 20/16px padding, 28px section spacing,
10px grid gaps, 30px observed counts, 22px lower bounds, 4px meters, and source
success/warning/error/accent colors. Reference cards collapse from two columns to
one at 540px of estimated content width; usage cards fit approximately 180px
minimum widths; history becomes two columns with its date spanning both.

Shared launcher helpers retain their existing text/control sizing: section labels
are currently 11px and shared buttons 32px, versus upstream 14px section headings
and 34px setup controls. Refresh's compact native icon uses the shared helper's
static ID label; richer localized accessibility labels/tooltips and animated
reload indicators are not implemented here. The visible localized checking and
loading states remain available, with no motion generated. Container width is
estimated from window/sidebar dimensions rather than measured CSS container
layout. Native screenshots, accessibility/tooltip verification, motion and exact
pixel acceptance remain primary integration work; metadata type-checking is not
visual or whole-app parity evidence.

## Verification

Six pure Rust mapping tests exercise:

- observed usage above reference caps, no fabricated remaining/reset estimate,
  lower-bound meter suppression, and no double counting unknown shared usage;
- manual/zero-risk suppression with retained history and public references;
- unsupported/unknown/paused states without capability inference;
- initial loading/read failure versus stale read/setup failure;
- five-language policy cards, source URL/date, locale timestamp layouts and number
  grouping/rounding;
- exact 75% warning threshold and zero-limit behavior.

Tests run from the exact `mod mapping` source with existing serde_json/chrono
artifacts using a temporary `rustc --test` harness. This avoids a new Cargo target,
CEF compilation, dependency downloads or build cache. An additional temporary
`rustc --emit=metadata` harness checks the complete renderer against existing
pinned GPUI artifacts and the current UI helper signatures. That harness supplies
minimal Launcher/helper signatures; actual module declaration/visibility and app
integration still require the primary's normal check. Neither harness starts a
browser, contacts an account, or performs OS actions. Temporary executables and
metadata are removed after testing.

Formatting: `rustfmt --check --edition 2024 crates/app/src/limits_view.rs`.
No `ui.rs`, `backend.rs`, `main.rs`, `assets.rs` or shared file was edited.
