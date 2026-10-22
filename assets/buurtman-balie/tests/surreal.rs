#![cfg(feature = "surreal")]
use buurtman_balie::{surreal::*, *};
use serde_json::{json, Value as Json};
use std::{
    collections::BTreeMap,
    io::Write,
    process::{Command as Process, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

struct Query;
impl SurrealQuery for Query {
    fn query(&self, sql: &str, params: Json) -> Result<Vec<Json>, Error> {
        if std::env::var_os("BB_RACE_WORKER").is_some() && sql.contains("BEGIN TRANSACTION") {
            let barrier =
                std::path::PathBuf::from(std::env::var_os("BB_SURREAL_RACE_DIR").unwrap());
            std::fs::write(barrier.join(std::process::id().to_string()), b"ready").unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while std::fs::read_dir(&barrier).unwrap().count() < 2 {
                assert!(
                    std::time::Instant::now() < deadline,
                    "CAS barrier timed out"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        let mut child =
            Process::new(std::env::var("BB_TEST_PYTHON").expect("proof Python runtime"))
                .args(["tooling/surreal-proof.py", "--query"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(
                serde_json::to_string(&json!({"sql": sql, "params": params}))
                    .unwrap()
                    .as_bytes(),
            )
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "test HTTP transport process failed"
        );
        let response: Json = serde_json::from_slice(&output.stdout).unwrap();
        match response.get("error").and_then(Json::as_str) {
            Some("conflict") => Err(Error::Conflict),
            Some(_) => Err(Error::Storage(
                response
                    .get("diagnostic")
                    .and_then(Json::as_str)
                    .unwrap_or("HTTP transport failure")
                    .into(),
            )),
            None => {
                // The server has committed; deliberately lose its response at the transport boundary.
                if std::env::var_os("BB_LOSE_RESPONSE").is_some()
                    && sql.contains("BEGIN TRANSACTION")
                {
                    return Err(Error::Storage("injected committed response loss".into()));
                }
                Ok(serde_json::from_value(response["values"].clone()).unwrap())
            }
        }
    }
}
fn store() -> SurrealStore<Query> {
    SurrealStore::new(Query, 1024 * 1024).unwrap()
}
struct Grants(Arc<AtomicBool>);
impl Authorization for Grants {
    fn allows(&self, ctx: &Context, _: Access) -> bool {
        self.0.load(Ordering::SeqCst) && ctx.actor == "staff"
    }
}
fn context() -> Context {
    Context {
        workspace: "business".into(),
        actor: "staff".into(),
    }
}
fn req(id: &str, command: Command) -> Request {
    Request {
        operation_id: id.into(),
        command,
    }
}
fn client() -> Record {
    Record {
        id: "client".into(),
        name: "Client".into(),
        kind: Kind::Customer,
        description: String::new(),
        tags: vec![],
        fields: BTreeMap::new(),
        case_status: None,
        revision: 0,
        prices: None,
        client_terms: Some(ClientTerms {
            classification: ClientClass::B2C,
            pricing: ClientPricing::Tier("employee".into()),
        }),
    }
}
fn engine() -> Crm<SurrealStore<Query>, Grants> {
    Crm::new(
        store(),
        Grants(Arc::new(AtomicBool::new(true))),
        Limits::default(),
    )
}
fn uncertain() -> Request {
    let mut changed = client();
    changed.revision = 1;
    changed.name = "Committed without response".into();
    req("uncertain", Command::Update(changed))
}

#[test]
#[ignore = "requires isolated live server; run tooling/surreal-proof.py"]
fn live_seed_boundary() {
    let mut crm = engine();
    crm.execute(
        &context(),
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
    crm.execute(&context(), req("client", Command::Create(client())))
        .unwrap();
    crm.execute(
        &context(),
        req(
            "field",
            Command::DefineField(FieldDefinition {
                key: "information".into(),
                label: "Information".into(),
                applies_to: vec![Kind::Product],
                value_type: FieldType::Integer,
            }),
        ),
    )
    .unwrap();
    let mut product = client();
    product.id = "product".into();
    product.kind = Kind::Product;
    product.client_terms = None;
    product
        .fields
        .insert("information".into(), Value::Integer(i64::MAX));
    product.prices = Some(PriceBook {
        currency: "EUR".into(),
        scale: 2,
        tax_basis: TaxBasis::Included,
        b2c: Some(Amount(1500)),
        b2b: Some(Amount(1000)),
        custom: BTreeMap::new(),
    });
    crm.execute(&context(), req("product", Command::Create(product)))
        .unwrap();
    crm.execute(
        &context(),
        req(
            "link",
            Command::Link(Relation {
                source: "client".into(),
                label: "interested-in".into(),
                target: Target::Record("product".into()),
            }),
        ),
    )
    .unwrap();
    // Two independent OS processes contend for the same empty workspace revision.
    let worker = |round: &str| {
        Process::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "live_cas_worker", "--nocapture"])
            .env("BB_RACE_WORKER", "1")
            .env("BB_RACE_ROUND", round)
            .env(
                "BB_SURREAL_RACE_DIR",
                std::path::PathBuf::from(std::env::var_os("BB_SURREAL_RACE_DIR").unwrap())
                    .join(round),
            )
            .spawn()
            .unwrap()
    };
    for round in ["create", "update"] {
        let mut a = worker(round);
        let mut b = worker(round);
        let mut codes = [
            a.wait().unwrap().code().unwrap(),
            b.wait().unwrap().code().unwrap(),
        ];
        codes.sort();
        assert_eq!(codes, [0, 10]);
    }
    let raced = store().load("race").unwrap();
    assert_eq!(raced.revision, 2);
    assert_eq!(raced.fields.len(), 2);
    assert_eq!(raced.receipts.len(), 2);
    // The writer exits without handing the outcome to a parent caller.
    let status = Process::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "live_unknown_worker", "--nocapture"])
        .env("BB_LOSE_RESPONSE", "1")
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
#[ignore = "child of live_seed_boundary"]
fn live_cas_worker() {
    let round = std::env::var("BB_RACE_ROUND").unwrap();
    let mut crm = engine();
    let ctx = Context {
        workspace: "race".into(),
        ..context()
    };
    let key = format!("{}-{}", round, std::process::id());
    match crm.execute(
        &ctx,
        req(
            &key,
            Command::DefineField(FieldDefinition {
                key: key.clone(),
                label: key.clone(),
                applies_to: vec![Kind::Product],
                value_type: FieldType::Text,
            }),
        ),
    ) {
        Ok(_) => std::process::exit(0),
        Err(Error::Conflict) => std::process::exit(10),
        result => panic!("unexpected CAS outcome {result:?}"),
    }
}

#[test]
#[ignore = "child of live_seed_boundary"]
fn live_unknown_worker() {
    assert_eq!(
        engine().execute(&context(), uncertain()),
        Err(Error::Storage("injected committed response loss".into()))
    );
    std::process::exit(0);
}

#[test]
#[ignore = "requires isolated live server; run tooling/surreal-proof.py"]
fn live_invalid_boundary() {
    let good = serde_json::to_string(&Snapshot::default()).unwrap();
    let old = Snapshot {
        schema_version: 1,
        ..Default::default()
    };
    for (row, error) in [
        (
            json!({"layout_version": 2, "revision": "0", "payload": good}),
            Error::Invalid("database schema"),
        ),
        (
            json!({"layout_version": 1, "revision": "0", "payload": "{"}),
            Error::Invalid("snapshot encoding"),
        ),
        (
            json!({"layout_version": 1, "revision": "0", "payload": serde_json::to_string(&old).unwrap()}),
            Error::Invalid("snapshot schema"),
        ),
        (
            json!({"layout_version": 1, "revision": "00", "payload": good}),
            Error::Invalid("snapshot revision"),
        ),
        (
            json!({"layout_version": 1, "revision": "0", "payload": "x".repeat(1024 * 1024 + 1)}),
            Error::Capacity("snapshot bytes"),
        ),
    ] {
        Query
            .query(
                "UPSERT bb_workspaces:invalid CONTENT $params.row;",
                json!({"row": row}),
            )
            .unwrap();
        assert_eq!(store().load("invalid"), Err(error.clone()));
        assert_eq!(
            store().compare_exchange(
                "invalid",
                0,
                Snapshot {
                    revision: 1,
                    ..Default::default()
                }
            ),
            Err(error)
        );
        let rows = Query
            .query(
                "SELECT layout_version,revision,payload FROM bb_workspaces:invalid;",
                json!({}),
            )
            .unwrap();
        assert_eq!(rows.last().unwrap(), &json!([row]));
    }
    Query
        .query("DELETE bb_workspaces:invalid;", json!({}))
        .unwrap();
    let mut wide = Snapshot {
        revision: u64::MAX - 1,
        ..Default::default()
    };
    Query
        .query(
            "UPSERT bb_workspaces:wide CONTENT $params.row;",
            json!({"row": {
                "layout_version": 1, "revision": wide.revision.to_string(),
                "payload": serde_json::to_string(&wide).unwrap()
            }}),
        )
        .unwrap();
    wide.revision = u64::MAX;
    store()
        .compare_exchange("wide", u64::MAX - 1, wide.clone())
        .unwrap();
    assert_eq!(store().load("wide").unwrap(), wide);
    assert_eq!(
        store().compare_exchange("wide", u64::MAX, wide),
        Err(Error::Conflict)
    );
}

#[test]
#[ignore = "requires restarted isolated server; run tooling/surreal-proof.py"]
fn live_restart_boundary() {
    let before = store().load("business").unwrap();
    let grant = Arc::new(AtomicBool::new(true));
    let mut crm = Crm::new(store(), Grants(grant.clone()), Limits::default());
    assert_eq!(
        crm.quote(&context(), "client", "product")
            .unwrap()
            .final_amount,
        Amount(900)
    );
    assert_eq!(crm.field_definitions(&context()).unwrap().len(), 1);
    assert_eq!(crm.relations(&context(), "client").unwrap().len(), 1);
    let filter = SearchFilter {
        fields: BTreeMap::from([("information".into(), Value::Integer(i64::MAX))]),
        ..Default::default()
    };
    assert_eq!(
        crm.search_filtered(&context(), &filter, 10, None)
            .unwrap()
            .total,
        1
    );
    assert_eq!(
        crm.execute(&context(), uncertain()).unwrap(),
        before.receipts["uncertain"].outcome
    );
    assert_eq!(
        crm.execute(&context(), req("uncertain", Command::Create(client()))),
        Err(Error::OperationReused)
    );
    let mut stale = client();
    stale.revision = 1;
    assert_eq!(
        crm.execute(&context(), req("stale", Command::Update(stale))),
        Err(Error::Conflict)
    );
    assert_eq!(
        crm.get(
            &Context {
                workspace: "other".into(),
                ..context()
            },
            "client"
        ),
        Err(Error::NotFound)
    );
    grant.store(false, Ordering::SeqCst);
    assert_eq!(crm.get(&context(), "client"), Err(Error::Denied));
    assert_eq!(crm.execute(&context(), uncertain()), Err(Error::Denied));
    assert_eq!(store().load("business").unwrap(), before);
    assert_eq!(
        store().compare_exchange(
            "business",
            5,
            Snapshot {
                revision: 6,
                ..Default::default()
            }
        ),
        Err(Error::Conflict)
    );
    assert_eq!(store().load("business").unwrap(), before);
}
