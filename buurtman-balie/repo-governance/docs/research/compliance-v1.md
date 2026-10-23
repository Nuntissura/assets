---
file_id: BB-COMPLIANCE-RESEARCH-v1
file_kind: research
updated_at: 2026-10-11
---

<topic id="scope" wp="WP-BB-008-compliance-research-v1" status="researched">

# EU, US and Belgium compliance research

Evidence and candidate product requirements, checked on 11 October 2026. This is research context;
the specification and accepted WP contracts own implementation authority. No tax filing, invoice transmission,
regulatory certification or live business applicability decision was performed.

Language (`en`, `fr`, `nl`), business jurisdiction, customer B2C/B2B classification and fiscal treatment are separate
inputs. Language changes must not choose VAT rates, sales-tax treatment, legal entity or invoice routing.
Existing price previews apply a chosen price base/discount and tax-included/excluded flag; they do not calculate tax.

Actual seller establishments, registrations, products/services, destinations, channels, turnover and host invoice
systems remain unresolved inputs. Do not infer them from this repository, the operator's language or customer labels.

</topic>

<topic id="belgium-vat" wp="WP-BB-008-compliance-research-v1" status="researched">

## Belgium VAT

The Belgian finance authority lists 21%, 12% and 6% VAT rates, with zero for some exceptional supplies.
Applicability depends on the supply; this is not an instruction to use a particular rate for a customer.
[Official rates](https://finances.belgium.be/fr/entreprises/tva/assujettissement-tva/taux-et-calcul/taux-tva).

Its small-business exemption overview gives an annual turnover ceiling of EUR 25,000 excluding VAT.
The seller's selected regime and eligibility must be explicit; B2B client classification is not an exemption.
[Official VAT regimes](https://financien.belgium.be/nl/ondernemingen/btw/btw-plicht/btw-regime).

The detailed FPS Finance page adds exclusions, a prorated ceiling for activity starting during 2026,
and crossing rules: exceeding EUR 25,000 by at most 10% ends exemption next calendar year;
exceeding by more than 10% switches from the transaction causing that crossing. EUR 27,500 is therefore
not a general eligibility ceiling. Regime selection/effective date and qualifying turnover need canonical
seller facts, not a CRM total. The small-business exemption does not itself remove the structured B2B
invoice obligation described below.
[Detailed exemption conditions, 2026 examples](https://financien.belgium.be/nl/ondernemingen/btw/btw-plicht/vrijstellingsregeling).

Candidate: host-owned effective-dated tax treatment containing jurisdiction, supply category, seller regime,
customer VAT evidence, selected rate/exemption reason and source reference. Keep checkout/invoice commit canonical
in the host; reject missing treatment rather than silently selecting zero. Verification needs domestic goods,
services, exempt supplies, inclusive/exclusive prices, corrections and rounding against the selected invoice owner.

</topic>

<topic id="belgium-e-invoicing" wp="WP-BB-008-compliance-research-v1" status="researched">

## Belgian structured invoices

From 1 January 2026, in-scope domestic Belgian VAT-subject B2B transactions require structured electronic invoices.
An emailed PDF alone does not fulfill that requirement. The official scope page identifies limited exceptions,
including Article 44 treatment and non-established taxpayers without a Belgian fixed establishment.
Outgoing private-customer invoices are outside this B2B mandate, while supplier invoice reception can still apply.
[Official scope and exceptions](https://efacture.belgium.be/fr/article/pour-qui-la-facturation-electronique-deviendra-t-elle-obligatoire).

The official format guidance uses Peppol BIS through Peppol, with a mutually agreed EN16931-conforming
alternative format and transmission method.
[Official invoice format and transmission](https://efacture.belgium.be/fr/article/quest-ce-quune-facture-electronique).

Candidate: preserve the canonical invoice owner and link CRM clients/cases to its invoice identifiers, delivery
status, corrections and structured artifacts. Do not implement PDF export as a claim of Belgian compliance.
Before selecting an adapter, inspect the actual invoice/accounting system's Peppol/EN16931 boundary.
Proof needs send/receive acknowledgment, rejected delivery, credit notes, replay, access and artifact preservation.
The FAQ keys commencement to in-scope invoices established, issued or sent from 1 January 2026, even for earlier
transactions. Customer/supplier unavailability has specific alternative-channel conditions; model these as owner-approved
exception states, never an automatic PDF fallback. Preserve the structured original and readable rendering.
The FAQ's self-billing technical-readiness flexibility ran only through 30 June 2026 under stated conditions;
that date has passed at this review. It is not an ongoing general grace period.
[Official FAQ](https://efacture.belgium.be/fr/FAQ/questions-generales-b2b).

</topic>

<topic id="eu-cross-border" wp="WP-BB-008-compliance-research-v1" status="researched">

## EU cross-border VAT

Your Europe distinguishes cross-border goods/services and business/private customers. It describes a EUR 10,000
threshold for qualifying intra-EU distance sales and electronic services and destination taxation/OSS where applicable.
The threshold is not a universal rule for all services or all sellers. Intra-EU B2B treatment depends on transaction
facts and applicable VAT-number evidence; selecting B2B in the CRM does not establish those facts.
[Official cross-border guidance](https://europa.eu/youreurope/business/finance-and-tax/vat/cross-border-vat/index_en.htm).

The Commission's detailed rule combines cross-border B2C telecommunications/broadcasting/electronic (TBE) services
and intra-Community distance goods sales. The EUR 10,000 excluding-VAT ceiling must hold in both current and preceding
calendar years, with establishment in only one Member State and qualifying cross-border supplies; goods dispatch starts
in that state. It excludes other services and imported distance goods. Choosing destination treatment binds for two years;
exceeding the ceiling applies destination treatment. Do not classify every online photo-service sale as electronic service.
[Commission OSS conditions](https://vat-one-stop-shop.ec.europa.eu/one-stop-shop_en).

Candidate: keep location, VAT evidence, supply category, effective date and scheme separate from pricing tier.
Reuse host invoice/tax services; preserve the chosen decision and its source revision with the sale. Test changes
in destination, invalid/missing VAT evidence, threshold windows and refunds using authoritative host transactions.

</topic>

<topic id="us-sales-tax" wp="WP-BB-008-compliance-research-v1" status="researched">

## US sales tax

The IRS directs businesses to their state revenue department for sales-tax information.
Do not implement a single US-wide rate or assume EU VAT behavior applies.
[IRS business guidance](https://www.irs.gov/faqs/small-business-self-employed-other-business/starting-or-ending-a-business).

California's official marketplace guide illustrates separate marketplace-facilitator collection and remote-seller
rules. Economic nexus considers tangible-property deliveries exceeding USD 500,000 in the current or preceding
calendar year, including related-person and marketplace sales; physical presence is a separate route. This evidence covers
California and that scope; it is not a multi-state applicability matrix.
[California marketplace guidance](https://cdtfa.ca.gov/industry/MPFAct.htm).

Candidate: host tax-provider port with destination, product category, nexus/registration, channel and marketplace
responsibility as explicit inputs. Determine target states before selecting coverage. Test mixed channels,
exemption evidence, refunds and jurisdiction changes. No US tax rates or nexus conclusions are configured.

</topic>

<topic id="privacy-retention" wp="WP-BB-008-compliance-research-v1" status="researched">

## CRM privacy and retention

EDPB guidance covers purposes, security, retention and controller/processor boundaries, and individual access,
rectification, objection, erasure and portability rights.
[SME guidance](https://www.edpb.europa.eu/sme/find-practical-info/faq_en),
[Data-subject rights](https://www.edpb.europa.eu/topics/key-gdpr-concepts/data-subject-rights_en).

Current CRM snapshots and operation receipts retain business data; receipt replay can retain earlier customer
values after an update. There is no erase/export/retention policy implementation. A future design must reconcile
rights handling with replay safety and required fiscal retention, covering backups and derived indexes too.
Do not silently prune receipts or treat a hidden UI row as erasure. Candidate controls: workspace authorization,
purpose-specific projections, documented retention, owner-directed export and scoped deletion/anonymization.
Acceptance needs independent reads of originals, historical receipts, derived outputs and restored backups.

GDPR Article 17(3) allows necessary retention for legal obligations or legal claims; it does not exempt every CRM receipt.
Article 5 requires purpose limitation, minimization and storage limitation. Candidate: separate host fiscal archives from
CRM purpose/retention scope; record an explicit retention basis/expiry and processing restriction for retained data.
After an authorized erasure, restoration must apply the same deletion/restriction state before data becomes accessible.
Any receipt redesign must retain operation-ID misuse detection and avoid replay resurrecting erased record payloads.
[GDPR Articles 5, 17, 18 and 21](https://eur-lex.europa.eu/eli/reg/2016/679/oj/eng).

California's official CCPA guidance defines applicability and rights for covered businesses.
State coverage and business eligibility need separate investigation; do not equate GDPR support with US privacy coverage.
[California Attorney General](https://oag.ca.gov/privacy/ccpa).

The CPPA FAQ gives the inflation-adjusted revenue trigger as USD 26.625 million or more for the preceding calendar
year, effective 1 January 2025, alongside buying/selling/sharing information of at least 100,000 California
residents/households or deriving at least 50% of annual revenue from selling/sharing their personal information.
Other coverage routes and service-provider/contractor obligations exist. Its employee/B2B-data exemptions expired
31 December 2022; a B2B tag is not a privacy exclusion. The Attorney General FAQ still displays USD 25 million:
do not encode that older nominal threshold. Host role, California operations and applicable coverage remain unresolved.
[Current CPPA applicability FAQ](https://cppa.ca.gov/faq.html).

Employee/HR records need separate purpose and access scope; GDPR Articles 5, 9 and 32 address minimization,
special-category data and security. Current workspace reads expose all CRM fields to authorized staff: storing HR
material there would not establish restricted HR access. Proposed: field/projection authorization, distinct retention
classes and auditable access before HR adoption. Resource/module/RSS/tag analytics need provenance, collection purpose,
aggregation and retention boundaries; public-source availability does not establish unrestricted personal-data reuse.
Prove forbidden-role reads fail and derived analytics/restores respect the selected restrictions.
[GDPR scope and safeguards](https://eur-lex.europa.eu/eli/reg/2016/679/oj/eng).

</topic>

<topic id="communications-consumer-rules" wp="WP-BB-008-compliance-research-v1" status="researched">

## Communication and consumer workflow boundaries

The FTC's CAN-SPAM guide requires truthful commercial-email routing/subjects and a working opt-out process;
it distinguishes commercial from transactional content. Any future campaign adapter needs suppression and
channel-purpose state shared with the canonical communication owner, rather than a generic customer tag.
[FTC business guide](https://www.ftc.gov/business-guidance/resources/can-spam-act-compliance-guide-business).

The FTC guide explicitly includes B2B commercial email; a B2B record is not a CAN-SPAM exclusion.
CAN-SPAM opt-out processing remains available for at least 30 days; requests are honored within 10 business days.
EU/Belgian communication treatment needs a separate channel and recipient scope: GDPR direct-marketing objection ends
that processing; ePrivacy Article 13 governs electronic-marketing consent and its existing-customer exception under
stated conditions. B2B classification does not settle recipient rules. Candidate: owner-supplied lawful-purpose/channel
decision, suppression state and send-time check, with queued/retried sends proven suppressed after objection.
[ePrivacy Article 13](https://eur-lex.europa.eu/legal-content/EN/TXT/?uri=CELEX:02002L0058-20091219),
[Belgian direct-marketing authority](https://www.autoriteprotectiondonnees.be/professionnel/themes/marketing-direct/bases-juridiques-a-vos-traitements-de-marketing-direct).

Belgian FPS Economy guidance distinguishes generic legal-person email addresses from named employee
addresses, which are natural-person addresses even on a company domain. Its existing-customer exception
requires directly collected sale/service contact details, the sender's similar products/services and a simple,
free objection opportunity at collection; opt-out remains available on advertising sends. GDPR processing
basis and electronic-channel permission are separate decisions. Candidate: retain recipient/address category,
collection source and owner-selected exception basis rather than infer send permission from B2B status.
[Belgian email advertising conditions](https://economie.fgov.be/fr/themes/line/commerce-electronique/spam/questions-frequemment-posees).

EU consumer-guarantee guidance covers statutory guarantees and remedies alongside commercial warranties.
Support cases can link to the canonical sale/return/warranty facts; CRM must not invent order dates or dispositions.
Exact goods/services and sales channels must be defined before retail or photo-service acceptance is finalized.
[Your Europe consumer guarantees](https://europa.eu/youreurope/business/selling-in-eu/consumer-contracts-guarantees/consumer-guarantees/index_en.htm).

Candidate: host-supplied channel, delivery/performance facts, item category and withdrawal/guarantee treatment,
separate from commercial warranty and CRM case status. Do not promise identical returns for store and distance sales.
Country-specific and personalized/digital/service exceptions need concrete review before timers or eligibility are built.

</topic>

<topic id="unresolved-owner-inputs" wp="WP-BB-008-compliance-research-v1" status="unresolved">

## Inputs before any compliance implementation

All rows are unresolved host inputs, not claims about the operator's businesses. Source inspection on
11 October 2026 found `TaxBasis` is a price flag, `calculate` discounts a selected base, `Receipt` retains
the request, and `get` clones a whole record after workspace read authorization. `Command` has no
erase/export, fiscal-decision, invoice-delivery or campaign operation.

| Area | Canonical owner and required facts | Failure/control candidate and required proof |
| --- | --- | --- |
| Belgian/EU VAT | Seller/tax owner: establishments, registrations/regime, effective dates, supply category, place of supply, VAT evidence, qualifying turnover/current-prior windows | Wrong exemption/destination: effective-dated owner decision; independently reconcile actual host sales, corrections and threshold-crossing transaction |
| Belgian invoices | Invoice/accounting owner: domestic scope, sender/receiver exceptions, format/channel capability, invoice issue date | PDF mistaken for compliance or repeated send: canonical invoice/status/artifact links; real send/receive rejection, correction and retry proof |
| US sales tax | Host tax owner/provider: target states, physical/economic nexus, taxable category, destination, related-party sales, marketplace responsibility | California rule applied nationally: state-specific scope/evidence; reconcile actual channels, refunds and provider decisions |
| Privacy/HR/analytics | Controller and host data owners: territorial/business coverage, purposes, field grants, retention basis/expiry, processors, backup/index/analytics inventory | Old payload survives erase/restore: receipt-aware design and restricted projections; independent historical/derived/restored reads plus forbidden-role denial |
| Communications | Communication owner: jurisdiction, channel, commercial/transactional purpose, recipient/address category, collection basis, suppression | Queued retry ignores objection: send-time owner suppression check; actual queue/retry boundary proof |
| Consumer cases | Sale/return owner: country, goods/service/digital/personalized category, sale channel, delivery/performance, withdrawal exceptions, warranty facts | Case status invents return eligibility: authoritative order facts; exact channel/category/date and correction proof |

Selected approach: host-owned decisions and references reuse `ExternalRef`, workspace authorization and revisions.
Rejected: inferred exemption from B2B, tax selection from language, a single US rule, universal retention expiry,
or demo records as business evidence. These are research candidates requiring a concrete accepted contract.
Fiscal archives stay with invoice/accounting owners; CRM receipt retention needs its own selected purpose/basis.

</topic>

<topic id="handoff" wp="WP-BB-008-compliance-research-v1" status="reviewed">

## Next compliance work

The compliance team reviews these sources and current code, maintains dated jurisdiction/scope evidence, and sends
candidate requirements to the spec coordinator. This research authorizes no new tax engine, invoice owner,
automatic exemptions, external VAT-number lookup or transfer of customer data. Implementation becomes executable
through a concrete accepted specification/WP with owner contracts and production-boundary proof.

High-return additions at the future integration boundary: effective-dated fiscal decisions with source IDs,
separate language/jurisdiction settings, canonical invoice links, and receipt-aware privacy recovery.
Each reuses current workspace/revision/external-reference contracts and needs its own exact owner/runtime proof.

Independent source review completed 11 October 2026 against `src/contracts.rs`, `src/pricing.rs`, `src/service.rs`,
README, accepted WP-BB-008 and current specification. Verdict: evidence-backed research deliverable with unresolved
business/transaction applicability; all candidate controls above are proposed future implementation. No tax engine,
privacy erasure, invoice transport, campaign sending or regulatory certification is implemented by this review.

Prior review corrected OSS conditions, dated e-invoice/exception handling, California threshold scope and
receipt-retention boundaries. This review independently reopened all cited official pages on 11 October 2026;
the linked rates page returned substantive content. The VAT-regime overview intermittently returned a human
challenge during follow-up, so its ceiling was corroborated against the opened detailed exemption and e-invoice
scope pages. Added exemption crossing/proration, an expired self-billing date, a direct Peppol/EN16931 citation,
precise CCPA coverage verbs and Belgian recipient/channel conditions. No seller eligibility was established.

Existing `.local/ui-proof/result.json`, `.local/localization-proof/result.json` and
`.local/manual-proof/result.json` were inspected as prior proof artifacts. They report ephemeral Rust MemoryStore
UI/localization/manual checks, not real fiscal transactions, invoice delivery, privacy erasure or campaign
suppression. They cannot prove these candidate controls or certify compliance; current-input validity of those
UI proofs was not reassessed here. No build, browser, package or governance check was run by this research lane.
WP008 `state.yaml` was inspected with `sync_pending: true`; devlead owns status synchronization and the declared
BB-CHECK-GOV proof. Only this research file was edited; shared specification/WP/state and product behavior remain
outside this assignment.

Priorities for a concrete follow-on contract: transaction/seller facts and fiscal-owner adapter; structured invoice
links/status; privacy replay/retention design; channel-specific suppression. Keep host identities, stock, checkout,
orders, usage and images canonical. Do not transmit operator/customer data during this research.

</topic>
