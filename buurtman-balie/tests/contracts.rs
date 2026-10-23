use buurtman_balie::memory::MemoryStore;
use buurtman_balie::*;
use std::collections::BTreeMap;

#[test]
fn linked_creation_is_atomic_and_replayed_as_one_intent() {
    let mut crm = Crm::new(MemoryStore::new(3), Grants, Limits::default());
    let context = ctx("portal");
    let case = record("case", "Report", Kind::SupportCase);
    let link = Relation {
        source: "case".into(),
        label: "reporter".into(),
        target: Target::External(ExternalRef {
            owner: "identity".into(),
            resource_type: "account".into(),
            id: "user".into(),
        }),
    };
    let mut invalid = link.clone();
    invalid.target = Target::Record("missing".into());
    assert!(crm
        .execute(
            &context,
            req(
                "invalid",
                Command::CreateLinked {
                    record: case.clone(),
                    relations: vec![link.clone(), invalid]
                }
            )
        )
        .is_err());
    assert_eq!(crm.get(&context, "case"), Err(Error::NotFound));
    let request = req(
        "create",
        Command::CreateLinked {
            record: case,
            relations: vec![link.clone()],
        },
    );
    let first = crm.execute(&context, request.clone()).unwrap();
    assert_eq!(crm.execute(&context, request.clone()).unwrap(), first);
    assert_eq!(crm.relations(&context, "case").unwrap(), vec![link]);
    let mut changed = request;
    if let Command::CreateLinked { relations, .. } = &mut changed.command {
        relations.clear();
    }
    assert_eq!(crm.execute(&context, changed), Err(Error::OperationReused));
    let mut wrong = Relation {
        source: "other".into(),
        label: "x".into(),
        target: Target::External(ExternalRef {
            owner: "identity".into(),
            resource_type: "account".into(),
            id: "user".into(),
        }),
    };
    assert!(crm
        .execute(
            &context,
            req(
                "wrong",
                Command::CreateLinked {
                    record: record("second", "Second", Kind::SupportCase),
                    relations: vec![wrong.clone()]
                }
            )
        )
        .is_err());
    wrong.source = "second".into();
    assert!(crm
        .execute(
            &context,
            req(
                "oversized",
                Command::CreateLinked {
                    record: record("second", "Second", Kind::SupportCase),
                    relations: vec![wrong; 33]
                }
            )
        )
        .is_err());
    assert_eq!(crm.get(&context, "second"), Err(Error::NotFound));
}

struct Grants;
impl Authorization for Grants {
    fn allows(&self, ctx: &Context, access: Access) -> bool {
        (ctx.actor == "operator" && matches!(ctx.workspace.as_str(), "portal" | "photo" | "store"))
            || (ctx.actor == "viewer" && ctx.workspace == "portal" && access == Access::Read)
    }
}
fn ctx(workspace: &str) -> Context {
    Context {
        workspace: workspace.into(),
        actor: "operator".into(),
    }
}
fn record(id: &str, name: &str, kind: Kind) -> Record {
    Record {
        id: id.into(),
        name: name.into(),
        case_status: (kind == Kind::SupportCase).then_some(CaseStatus::New),
        client_terms: (kind == Kind::Customer).then_some(ClientTerms {
            classification: ClientClass::B2C,
            pricing: ClientPricing::Standard,
        }),
        prices: None,
        kind,
        description: String::new(),
        tags: vec!["priority".into()],
        fields: BTreeMap::new(),
        revision: 0,
    }
}
fn req(id: &str, command: Command) -> Request {
    Request {
        operation_id: id.into(),
        command,
    }
}
fn engine() -> Crm<MemoryStore, Grants> {
    Crm::new(MemoryStore::new(3), Grants, Limits::default())
}

#[test]
fn workspace_and_current_authority_fence_every_operation() {
    let mut crm = engine();
    crm.execute(
        &ctx("portal"),
        req(
            "create",
            Command::Create(record("c", "Portal customer", Kind::Customer)),
        ),
    )
    .unwrap();
    assert_eq!(crm.get(&ctx("photo"), "c"), Err(Error::NotFound));
    let denied = Context {
        workspace: "portal".into(),
        actor: "intruder".into(),
    };
    assert_eq!(crm.get(&denied, "c"), Err(Error::Denied));
    assert_eq!(crm.search(&denied, "", None, 10, None), Err(Error::Denied));
    assert_eq!(crm.relations(&denied, "c"), Err(Error::Denied));
    assert_eq!(crm.field_definitions(&denied), Err(Error::Denied));
    let viewer = Context {
        actor: "viewer".into(),
        ..ctx("portal")
    };
    assert!(crm.get(&viewer, "c").is_ok());
    assert_eq!(
        crm.execute(
            &viewer,
            req("x", Command::Create(record("x", "X", Kind::Customer)))
        ),
        Err(Error::Denied)
    );
}

#[test]
fn replay_is_atomic_and_different_intent_is_rejected() {
    let mut crm = engine();
    let context = ctx("portal");
    let request = req(
        "create",
        Command::Create(record("c", "Customer", Kind::Customer)),
    );
    let first = crm.execute(&context, request.clone()).unwrap();
    assert_eq!(crm.execute(&context, request).unwrap(), first);
    assert_eq!(
        crm.execute(
            &context,
            req(
                "create",
                Command::Create(record("other", "Other", Kind::Customer))
            )
        ),
        Err(Error::OperationReused)
    );
    let mut edited = crm.get(&context, "c").unwrap();
    edited.name = "Changed".into();
    assert_eq!(
        crm.execute(&context, req("edit", Command::Update(edited.clone())))
            .unwrap()
            .record_revision,
        Some(2)
    );
    assert_eq!(
        crm.execute(&context, req("stale", Command::Update(edited))),
        Err(Error::Conflict)
    );
    assert_eq!(crm.get(&context, "c").unwrap().revision, 2);
}

#[test]
fn typed_fields_and_links_preserve_canonical_identity() {
    let mut crm = engine();
    let context = ctx("store");
    crm.execute(
        &context,
        req(
            "field",
            Command::DefineField(FieldDefinition {
                key: "vip".into(),
                label: "VIP".into(),
                applies_to: vec![Kind::Customer],
                value_type: FieldType::Boolean,
            }),
        ),
    )
    .unwrap();
    let mut customer = record("c", "Client", Kind::Customer);
    customer
        .fields
        .insert("vip".into(), Value::Text("yes".into()));
    assert_eq!(
        crm.execute(&context, req("bad", Command::Create(customer.clone()))),
        Err(Error::Invalid("field type"))
    );
    customer.fields.insert("vip".into(), Value::Boolean(true));
    crm.execute(&context, req("customer", Command::Create(customer)))
        .unwrap();
    crm.execute(
        &context,
        req(
            "case",
            Command::Create(record("s", "Missing order", Kind::SupportCase)),
        ),
    )
    .unwrap();
    let relation = Relation {
        source: "s".into(),
        label: "customer".into(),
        target: Target::Record("c".into()),
    };
    crm.execute(&context, req("link", Command::Link(relation.clone())))
        .unwrap();
    assert_eq!(crm.relations(&context, "c").unwrap(), vec![relation]);
    let external = Relation {
        source: "s".into(),
        label: "order".into(),
        target: Target::External(ExternalRef {
            owner: "commerce".into(),
            resource_type: "order".into(),
            id: "42".into(),
        }),
    };
    crm.execute(&context, req("external", Command::Link(external)))
        .unwrap();
    assert_eq!(crm.relations(&context, "s").unwrap().len(), 2);
    assert_eq!(
        crm.execute(
            &ctx("photo"),
            req(
                "leak",
                Command::Link(Relation {
                    source: "s".into(),
                    label: "customer".into(),
                    target: Target::Record("c".into())
                })
            )
        ),
        Err(Error::NotFound)
    );
}

#[test]
fn fuzzy_tag_search_pages_are_complete_and_stale_cursors_fail() {
    let mut crm = engine();
    let context = ctx("portal");
    for id in ["a", "b", "c"] {
        crm.execute(
            &context,
            req(
                id,
                Command::Create(record(id, "Camera support", Kind::SupportCase)),
            ),
        )
        .unwrap();
    }
    let first = crm
        .search(&context, "camra", Some("PRIORITY"), 1, None)
        .unwrap();
    assert_eq!(first.total, 3);
    assert_eq!(first.hits[0].record.id, "a");
    let second = crm
        .search(&context, "camra", Some("priority"), 2, first.next.as_ref())
        .unwrap();
    assert_eq!(
        second
            .hits
            .iter()
            .map(|h| h.record.id.as_str())
            .collect::<Vec<_>>(),
        vec!["b", "c"]
    );
    assert!(second.next.is_none());
    assert_eq!(
        crm.search(&context, "other", Some("priority"), 1, first.next.as_ref()),
        Err(Error::Conflict)
    );
    crm.execute(
        &context,
        req(
            "new",
            Command::Create(record("d", "Camera", Kind::Customer)),
        ),
    )
    .unwrap();
    assert_eq!(
        crm.search(&context, "camra", Some("priority"), 1, first.next.as_ref()),
        Err(Error::Conflict)
    );
    assert!(crm.search(&context, "", None, 101, None).is_err());
}

#[test]
fn capacity_rejection_does_not_commit_partial_state() {
    let mut crm = Crm::new(
        MemoryStore::new(1),
        Grants,
        Limits {
            records: 1,
            operations: 2,
            ..Limits::default()
        },
    );
    crm.execute(
        &ctx("portal"),
        req("one", Command::Create(record("a", "A", Kind::Customer))),
    )
    .unwrap();
    assert_eq!(
        crm.execute(
            &ctx("portal"),
            req("two", Command::Create(record("b", "B", Kind::Customer)))
        ),
        Err(Error::Capacity("records"))
    );
    assert_eq!(crm.get(&ctx("portal"), "b"), Err(Error::NotFound));
    assert_eq!(
        crm.execute(
            &ctx("photo"),
            req("one", Command::Create(record("a", "A", Kind::Customer)))
        ),
        Err(Error::Capacity("workspaces"))
    );
    assert_eq!(crm.get(&ctx("photo"), "a"), Err(Error::NotFound));
}

struct LostResponse {
    inner: MemoryStore,
    lose: bool,
}
impl Store for LostResponse {
    fn load(&self, w: &str) -> Result<Snapshot, Error> {
        self.inner.load(w)
    }
    fn compare_exchange(&mut self, w: &str, e: u64, s: Snapshot) -> Result<(), Error> {
        self.inner.compare_exchange(w, e, s)?;
        if self.lose {
            self.lose = false;
            return Err(Error::Storage("response lost after commit".into()));
        }
        Ok(())
    }
}
#[test]
fn uncertain_commit_reconciles_with_atomic_operation_receipt() {
    let mut crm = Crm::new(
        LostResponse {
            inner: MemoryStore::new(1),
            lose: true,
        },
        Grants,
        Limits::default(),
    );
    let request = req(
        "retry",
        Command::Create(record("c", "Client", Kind::Customer)),
    );
    assert!(matches!(
        crm.execute(&ctx("portal"), request.clone()),
        Err(Error::Storage(_))
    ));
    assert_eq!(
        crm.execute(&ctx("portal"), request)
            .unwrap()
            .workspace_revision,
        1
    );
    assert_eq!(crm.get(&ctx("portal"), "c").unwrap().revision, 1);
}

#[test]
fn adapter_cas_and_snapshot_roundtrip_are_version_fenced() {
    let mut store = MemoryStore::new(1);
    let snapshot = Snapshot {
        revision: 1,
        ..Snapshot::default()
    };
    store
        .compare_exchange("portal", 0, snapshot.clone())
        .unwrap();
    assert_eq!(
        store.compare_exchange("portal", 0, snapshot.clone()),
        Err(Error::Conflict)
    );
    assert_eq!(
        serde_json::from_str::<Snapshot>(&serde_json::to_string(&snapshot).unwrap()).unwrap(),
        snapshot
    );
}

#[test]
fn product_field_catalogue_is_reusable_and_drives_typed_filters() {
    let mut crm = engine();
    let context = ctx("store");
    crm.execute(
        &context,
        req(
            "define",
            Command::DefineField(FieldDefinition {
                key: "brand".into(),
                label: "Brand".into(),
                applies_to: vec![Kind::Product, Kind::Distributor],
                value_type: FieldType::Text,
            }),
        ),
    )
    .unwrap();
    for (id, brand, kind) in [
        ("camera-a", "Alpha", Kind::Product),
        ("camera-b", "Beta", Kind::Product),
        ("supplier", "Alpha", Kind::Distributor),
    ] {
        let mut input = record(id, id, kind);
        input
            .fields
            .insert("brand".into(), Value::Text(brand.into()));
        crm.execute(&context, req(id, Command::Create(input)))
            .unwrap();
    }
    assert_eq!(crm.field_definitions(&context).unwrap().len(), 1);
    let filters = BTreeMap::from([("brand".into(), Value::Text("Alpha".into()))]);
    let page = crm
        .search_filtered(
            &context,
            &SearchFilter {
                fields: filters,
                kind: Some(Kind::Product),
                ..Default::default()
            },
            10,
            None,
        )
        .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.hits[0].record.id, "camera-a");
    let wrong = BTreeMap::from([("brand".into(), Value::Boolean(true))]);
    assert_eq!(
        crm.search_filtered(
            &context,
            &SearchFilter {
                fields: wrong,
                ..Default::default()
            },
            10,
            None
        ),
        Err(Error::Invalid("field type"))
    );
    assert!(crm.field_definitions(&ctx("photo")).unwrap().is_empty());
    let mut changed = crm.field_definitions(&context).unwrap().remove(0);
    changed.value_type = FieldType::Boolean;
    assert_eq!(
        crm.execute(&context, req("retype", Command::DefineField(changed))),
        Err(Error::Conflict)
    );
}

#[test]
fn serialized_input_rejects_unknown_fields_and_unicode_limits_hold() {
    let input = record("p", "Ångström", Kind::Product);
    let mut value = serde_json::to_value(input.clone()).unwrap();
    value["invented_field"] = serde_json::json!(true);
    assert!(serde_json::from_value::<Record>(value).is_err());
    let mut crm = engine();
    crm.execute(&ctx("store"), req("valid", Command::Create(input)))
        .unwrap();
    assert_eq!(
        crm.search(&ctx("store"), "ångstrm", None, 10, None)
            .unwrap()
            .total,
        1
    );
    let mut oversized = record("large", "X", Kind::Product);
    oversized.name = "é".repeat(129);
    assert_eq!(
        crm.execute(&ctx("store"), req("oversized", Command::Create(oversized))),
        Err(Error::Invalid("text"))
    );
    assert_eq!(crm.get(&ctx("store"), "large"), Err(Error::NotFound));
}

#[test]
fn byte_budget_rejects_record_and_receipt_atomically() {
    let mut crm = Crm::new(
        MemoryStore::new(1),
        Grants,
        Limits {
            snapshot_bytes: 512,
            ..Limits::default()
        },
    );
    let mut input = record("a", "A", Kind::Customer);
    input.description = "X".repeat(400);
    assert_eq!(
        crm.execute(&ctx("portal"), req("too-large", Command::Create(input))),
        Err(Error::Capacity("snapshot bytes"))
    );
    assert_eq!(crm.get(&ctx("portal"), "a"), Err(Error::NotFound));
    let fresh = req(
        "too-large",
        Command::DefineField(FieldDefinition {
            key: "small".into(),
            label: "Small".into(),
            applies_to: vec![Kind::Customer],
            value_type: FieldType::Boolean,
        }),
    );
    assert!(crm.execute(&ctx("portal"), fresh).is_ok());
}

#[test]
fn integer_wire_values_preserve_full_precision_for_browser_hosts() {
    let value = Value::Integer(i64::MAX);
    let json = serde_json::to_string(&value).unwrap();
    assert_eq!(json, r#"{"Integer":"9223372036854775807"}"#);
    assert_eq!(serde_json::from_str::<Value>(&json).unwrap(), value);
    assert!(serde_json::from_str::<Value>(r#"{"Integer":9223372036854775807}"#).is_err());
    assert!(serde_json::from_str::<Value>(r#"{"Integer":"9223372036854775808"}"#).is_err());
}

#[test]
fn mandatory_classification_and_employee_pricing_are_independent() {
    let mut crm = engine();
    let context = ctx("store");
    let mut missing = record("missing", "Unassigned", Kind::Customer);
    missing.client_terms = None;
    assert!(matches!(
        crm.execute(&context, req("missing", Command::Create(missing))),
        Err(Error::Invalid(_))
    ));
    crm.execute(
        &context,
        req(
            "tier",
            Command::DefinePricingTier(PricingTier {
                id: "employee".into(),
                name: "Employee".into(),
                base: PriceBasis::B2B,
                discount_basis_points: 1000,
            }),
        ),
    )
    .unwrap();
    let mut employee = record("employee-client", "Employee client", Kind::Customer);
    employee.client_terms = Some(ClientTerms {
        classification: ClientClass::B2C,
        pricing: ClientPricing::Tier("employee".into()),
    });
    crm.execute(&context, req("employee", Command::Create(employee)))
        .unwrap();
    let mut product = record("priced", "Priced product", Kind::Product);
    product.prices = Some(PriceBook {
        currency: "EUR".into(),
        scale: 2,
        tax_basis: TaxBasis::Included,
        b2c: Some(Amount(12000)),
        b2b: Some(Amount(8000)),
        custom: BTreeMap::new(),
    });
    crm.execute(&context, req("product", Command::Create(product)))
        .unwrap();
    let quote = crm.quote(&context, "employee-client", "priced").unwrap();
    assert_eq!(quote.classification, ClientClass::B2C);
    assert_eq!(quote.base, PriceBasis::B2B);
    assert_eq!(quote.base_amount, Amount(8000));
    assert_eq!(quote.discount_amount, Amount(800));
    assert_eq!(quote.final_amount, Amount(7200));
    assert_eq!(
        crm.quote(&ctx("photo"), "employee-client", "priced"),
        Err(Error::NotFound)
    );
    let denied = Context {
        actor: "intruder".into(),
        ..context
    };
    assert_eq!(
        crm.quote(&denied, "employee-client", "priced"),
        Err(Error::Denied)
    );
}

#[test]
fn custom_prices_missing_bases_and_discount_rounding_are_explicit() {
    let mut crm = engine();
    let context = ctx("store");
    for (id, base, bps) in [
        ("custom", PriceBasis::Custom("staff".into()), 500),
        ("half", PriceBasis::B2B, 5000),
    ] {
        crm.execute(
            &context,
            req(
                id,
                Command::DefinePricingTier(PricingTier {
                    id: id.into(),
                    name: id.into(),
                    base,
                    discount_basis_points: bps,
                }),
            ),
        )
        .unwrap();
    }
    let mut client = record("client", "Client", Kind::Customer);
    client.client_terms = Some(ClientTerms {
        classification: ClientClass::B2B,
        pricing: ClientPricing::Tier("custom".into()),
    });
    crm.execute(&context, req("client", Command::Create(client)))
        .unwrap();
    let mut product = record("p", "Product", Kind::Product);
    product.prices = Some(PriceBook {
        currency: "EUR".into(),
        scale: 2,
        tax_basis: TaxBasis::Excluded,
        b2c: Some(Amount(1000)),
        b2b: None,
        custom: BTreeMap::from([("staff".into(), Amount(500))]),
    });
    crm.execute(&context, req("p", Command::Create(product)))
        .unwrap();
    assert_eq!(
        crm.quote(&context, "client", "p").unwrap().final_amount,
        Amount(475)
    );
    let mut client = crm.get(&context, "client").unwrap();
    client.client_terms.as_mut().unwrap().pricing = ClientPricing::Standard;
    crm.execute(&context, req("standard", Command::Update(client)))
        .unwrap();
    assert_eq!(
        crm.quote(&context, "client", "p"),
        Err(Error::Invalid("selected base price unavailable"))
    );
    let mut product = crm.get(&context, "p").unwrap();
    product.prices.as_mut().unwrap().b2b = Some(Amount(1));
    crm.execute(&context, req("tiny", Command::Update(product)))
        .unwrap();
    let mut client = crm.get(&context, "client").unwrap();
    client.client_terms.as_mut().unwrap().pricing = ClientPricing::Tier("half".into());
    crm.execute(&context, req("half-client", Command::Update(client)))
        .unwrap();
    assert_eq!(
        crm.quote(&context, "client", "p").unwrap().final_amount,
        Amount(0)
    );
    assert!(crm
        .execute(
            &context,
            req(
                "invalid",
                Command::DefinePricingTier(PricingTier {
                    id: "bad".into(),
                    name: "Bad".into(),
                    base: PriceBasis::B2B,
                    discount_basis_points: 10001
                })
            )
        )
        .is_err());
}
