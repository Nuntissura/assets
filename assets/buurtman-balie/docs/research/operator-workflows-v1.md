---
file_id: BB-OPERATOR-WORKFLOWS-v1
file_kind: workflow-validation
updated_at: 2026-10-11
---

<topic id="execution" wp="WP-BB-013-operator-workflow-validation-v1" status="observed">

# Operator workflow execution

Executed the mounted standalone app in headless Chrome at 1280×1000 and 390×844 through `tooling/standalone.py` and the existing actual Rust `json_bridge` binary. This is isolated temporary MemoryStore data, not camera-store, supermarket or Portal production data. No host inventory, usage, order, tax or identity source was acquired or changed.

Reproduction script: `.local/operator-workflows/run.py`. Exact observed results: `.local/operator-workflows/result.json`; final rendered DOM: `.local/operator-workflows/final.html`. Opened and visually inspected `desktop.png`, `mobile.png` and `manual.png` in the same directory before verdicts. Desktop shows readable separate product/search/field/pricing panels; mobile stacks them without horizontal overflow or overlapping text. The mobile page is 4840px tall, placing the employee quote far below the product editor. Help expands above the editor and retains the draft; its language selector changes manual language only, as configured.

The run recorded all scenario results and screenshots, then ended with exit 1 during browser-driver cleanup (`Browser.close: Connection closed while reading from the driver`). The owned Python→Playwright driver ancestry was inspected; only that driver was stopped after shutdown stalled. The script's final Unicode stdout and unbounded browser close were replaced with ASCII stdout and bounded close; GP-031 records the preparation requirement. No rerun of unchanged successful scenarios was used to conceal that cleanup failure. Observed scenario artifacts remain available; this command is not claimed as a fully green shared BB-CHECK-UI run. Root owns the shared acceptance proof.

</topic>

<topic id="scenario-matrix" wp="WP-BB-013-operator-workflow-validation-v1" status="observed">

All executed scenarios start in the isolated demo business workspace and use the demo operator grant. New product/client records are temporary canonical records for that process only. `EXECUTED` describes observed steps, not production compatibility. SaaS linked references are opaque demo identifiers; they prove command acceptance, not the referenced owner's facts.

| Scenario / employee task | Exact entity / canonical owner | Expected steps and result | Actual result / evidence |
|---|---|---|---|
| Camera assistant creates item | Camera Orion, SKU CAM-001 / CRM descriptive catalogue | Create Brand/SKU fields, name/description/tags/prices; reuse fields in filter list | EXECUTED: saved camera with mirrorless/display tags; catalogue lists Brand/Refrigerated/SKU; `catalogue`, `new-CAM-001` |
| Supermarket clerk creates item | Milk 1L, SKU MILK-001 / CRM descriptive catalogue | Create chilled item with Boolean field and B2C/B2B prices | EXECUTED: saved grocery/chilled Milk; `new-MILK-001` |
| Camera clerk labels/finds item | Camera Orion tags / CRM | Search misspelled Camra Orion, then mirrorless | EXECUTED: camera returned both times; `search-Camra Orion`, `search-mirrorless` |
| Supermarket clerk finds chilled item | Milk Refrigerated=true / CRM | Search chilled; filter Boolean true | EXECUTED: only Milk returned; `search-chilled`, `supermarket-filter` |
| Clerk edits existing item | Milk same record / CRM | Select hit, edit description, save, retain SKU | EXECUTED: MILK-001 retained; `same-item-edit` |
| Clerk changes item with unfinished work | Camera draft and Milk / UI draft owner | Warn or retain camera draft before selecting Milk | GAP: camera draft silently replaced; `dirty-switch` |
| Manager registers consumers/business clients | Camera employee B2C, SaaS company B2B / CRM client terms; host identity separate | Mandatory visible classification, create independently assigned employee tier | EXECUTED: blank required classification before assignment; both classifications displayed; `mandatory-B2C`, `mandatory-B2B`, screenshots |
| Camera employee gets extra discount | Camera employee → Camera Orion / CRM price preview, host sale commit separate | B2C employee assigned B2B base80 minus10%=EUR72 | EXECUTED exact preview; `employee-quote` |
| Manager changes quote's base | Same camera B2B100 / CRM, host checkout separate | Invalidate old preview or recalculate EUR90 | GAP: old EUR72 preview remains visibly current; `stale-quote`, desktop/mobile |
| Clerk corrects invalid price | Camera B2C input -1 / UI validation | Reject locally with corrective instruction | GAP: says save unconfirmed/retry to reconcile despite local validation; `invalid-price`, desktop |
| Staff opens manual and returns | Selected Camera draft / UI | Open help, Dutch language, topic link; retain selected input | EXECUTED: camera and draft unchanged; `manual-return`, manual/mobile |
| Mobile assistant uses same screen | Camera editor/pricing / UI | Readable controls, no overlap/overflow, retained selection | EXECUTED inspected screenshots; no horizontal overflow; `mobile-layout`, `console` has no page errors |
| SaaS support agent records company/case/contact | saas-org, saas-case, saas-contact / CRM | Create Organization, SupportCase New, Interaction | EXECUTED actual bridge commands, revision1; `saas-create-*` |
| SaaS agent associates context | Case→organization, interaction→case / CRM | Link owned records | EXECUTED commands accepted; `saas-link-organization`, `saas-link-support`; relation canonical reread not exposed by bridge |
| SaaS operator attaches usage pointer | saas-org→portal/usage/usage-ref-demo / Portal owns usage | Store opaque pointer without inventing metric | EXECUTED accepted pointer only; `saas-link-usage`; real usage NOT_INSPECTED |
| Support agent retries assignment and handles stale edit | saas-case / CRM | Assign, replay identical operation; reject stale Closed update; reread | EXECUTED identical revision2/workspace17 outcomes, Conflict on stale update, independent search sees Assigned revision2; `saas-replay`, `saas-stale`, `saas-reread` |
| SaaS staff opens linked organization/support/usage and returns | CRM relations plus Portal owner | UI navigation preserves source/selection | NOT_IMPLEMENTED: mounted page has only catalogue/client-pricing; bridge lacks relation read; `saas-ui` |
| Retail receiving/replenishment | Actual stock ledger / retail inventory host | Receive delivery, quantities, location, count/reconcile, reserve | NOT_IMPLEMENTED in actual page; no selected inventory adapter; `retail-host` |
| Retail barcode/shelf/expiry labels | Product identity/stock batch/label printer / retail host | Scan/print labels, batch/expiry, price provenance | Tags and descriptive SKU supported; physical labeling/scan/batch/printing NOT_IMPLEMENTED; `retail-host` |
| Retail sale/return/availability | Sale/order/payment/reservation / checkout and inventory hosts | Preserve active sale while inspecting product/customer info; commit/return atomically | NOT_IMPLEMENTED; no selected host adapters, no sale/order/return controls; `retail-host` |
| Company staff/manager day-to-day | Shifts, HR, payroll, staff grants / HR/identity hosts | Role-specific access, schedule/attendance, approvals, audit | NOT_IMPLEMENTED/unproven; employee pricing is not HR; `company-employee` |
| SaaS operations/analytics | Subscription/usage/billing/RSS/tag telemetry / Portal and provider owners | Account linkage, measure resources/module usage, freshness/window, entitlement changes | NOT_IMPLEMENTED/unproven; a CRM external pointer does not supply or authorize metrics |
| Cross-company operations | Imports, exports, communications, reminders, reporting, restore / selected owners unresolved | Day-to-day company discovery surface | NOT_IMPLEMENTED/unproven; preserve growth and analytics stubs; not an exhaustive business requirements claim |

</topic>

<topic id="observed-defects" wp="WP-BB-013-operator-workflow-validation-v1" status="proposed">

| ID | Exact reproduction / expected versus observed | User impact | Practical remediation / validation | Owner |
|---|---|---|---|---|
| WF-001 | Select Camera; type unsaved warranty description; select Milk result. Expected preserve/warn; observed description immediately becomes Milk shelf description | Silent draft loss in routine lookup | Track dirty draft by record, warn before destructive selection/New product and preserve per-record drafts; validate cancel/continue/save paths, browser refresh policy and failures | Reusable UI/host navigation draft boundary |
| WF-002 | Calculate employee quote EUR72; save Camera B2B100. Expected stale indication or EUR90; observed EUR72 still displayed | Staff may communicate or reuse outdated amount | Retain quote's client/product revisions, invalidate relevant preview on successful record/tier refresh or selection change; recalculate explicitly; independently test product/client/tier mutations plus unchanged unrelated updates | CRM preview UI; host checkout must still fence quoted revisions |
| WF-003 | Enter B2C -1 then Save. Expected local corrective message; observed `Save not confirmed: Use a nonnegative decimal price.. Retry to reconcile.` | Misleads staff into retrying invalid intent; conflates local validation with uncertain commit | Separate before-dispatch input validation from post-dispatch uncertain storage recovery; show correction text without reconcile action; test validation, confirmed conflict and response-loss paths | Product UI/error presentation |
| WF-004 | Open SaaS support/organization/usage from mounted UI. Expected connected detail/return; no such controls or relation-read bridge action | Existing backend records and links inaccessible to staff workflows | Add authorized related-detail/navigation adapter in selected host with stable return token and source/query/selection context; exercise actual owners and authorization, no demo substitution | Portal adapter/growth; core relation read already separate from demo bridge |
| WF-005 | Attempt receive delivery, label shelf, checkout/return. No mounted stock/quantity/printing/sale actions and no selected host adapters | Camera-store/supermarket end-to-end job impossible | Inspect canonical retail owners, select inventory/order/label boundary and define minimum connected receiving→catalogue→sale→return contract; proof must reconcile ledger/order/stock at actual host boundary | Retail inventory, checkout/order and label owners |

Cheap follow-ons reuse existing fields, exact pricing, relations, manual and searchable records: dirty-draft preservation, revision-visible quote freshness, context-preserving related navigation. They close observed defects; they do not authorize a new CRM/ERP implementation. Desktop/mobile proof, input-retention/error-path coverage and canonical owner reconciliation are required for their respective implementations. No product changes made by this lane.

</topic>
