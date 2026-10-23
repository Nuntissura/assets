#![cfg(feature = "sqlite")]
use buurtman_balie::{sqlite::SqliteStore, *};
use rusqlite::Connection;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    process::Command as Process,
    sync::atomic::{AtomicU64, Ordering},
};

struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "bb-durable-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        Self(dir.join("crm.sqlite"))
    }
    fn store(&self) -> SqliteStore {
        SqliteStore::open(&self.0, 3, 1024 * 1024).unwrap()
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        std::fs::remove_dir_all(self.0.parent().unwrap()).unwrap();
    }
}
struct Grants;
impl Authorization for Grants {
    fn allows(&self, ctx: &Context, access: Access) -> bool {
        (ctx.actor == "staff" && matches!(ctx.workspace.as_str(), "business" | "other"))
            || (ctx.actor == "viewer" && ctx.workspace == "business" && access == Access::Read)
    }
}
fn context() -> Context {
    Context {
        actor: "staff".into(),
        workspace: "business".into(),
    }
}
fn request(id: &str, command: Command) -> Request {
    Request {
        operation_id: id.into(),
        command,
    }
}
fn customer() -> Record {
    Record {
        id: "client".into(),
        kind: Kind::Customer,
        name: "Client".into(),
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
fn engine(db: &Database) -> Crm<SqliteStore, Grants> {
    Crm::new(db.store(), Grants, Limits::default())
}
fn seed(crm: &mut Crm<SqliteStore, Grants>) {
    crm.execute(
        &context(),
        request(
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
    crm.execute(&context(), request("client", Command::Create(customer())))
        .unwrap();
    crm.execute(
        &context(),
        request(
            "field",
            Command::DefineField(FieldDefinition {
                key: "stock-info".into(),
                label: "Information".into(),
                applies_to: vec![Kind::Product],
                value_type: FieldType::Integer,
            }),
        ),
    )
    .unwrap();
    let mut product = customer();
    product.id = "product".into();
    product.kind = Kind::Product;
    product.client_terms = None;
    product
        .fields
        .insert("stock-info".into(), Value::Integer(i64::MAX));
    product.prices = Some(PriceBook {
        currency: "EUR".into(),
        scale: 2,
        tax_basis: TaxBasis::Included,
        b2c: Some(Amount(1500)),
        b2b: Some(Amount(1000)),
        custom: BTreeMap::new(),
    });
    crm.execute(&context(), request("product", Command::Create(product)))
        .unwrap();
    crm.execute(
        &context(),
        request(
            "link",
            Command::Link(Relation {
                source: "client".into(),
                label: "interested-in".into(),
                target: Target::Record("product".into()),
            }),
        ),
    )
    .unwrap();
}

#[test]
fn disk_reopen_preserves_pricing_fields_links_receipts_and_grants() {
    let db = Database::new();
    {
        seed(&mut engine(&db));
    }
    // Independent canonical query, without the Store implementation.
    let conn = Connection::open(&db.0).unwrap();
    let bytes: Vec<u8> = conn
        .query_row(
            "SELECT snapshot FROM bb_workspaces WHERE workspace='business'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let snapshot: Snapshot = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(snapshot.revision, 5);
    assert_eq!(snapshot.receipts.len(), 5);
    assert_eq!(snapshot.records.len(), 2);
    assert_eq!(snapshot.fields.len(), 1);
    assert_eq!(snapshot.pricing_tiers.len(), 1);
    assert_eq!(snapshot.relations.len(), 1);
    drop(conn);
    let mut crm = engine(&db);
    assert_eq!(
        crm.quote(&context(), "client", "product")
            .unwrap()
            .final_amount,
        Amount(900)
    );
    assert_eq!(crm.field_definitions(&context()).unwrap().len(), 1);
    assert_eq!(crm.relations(&context(), "client").unwrap().len(), 1);
    let filter = SearchFilter {
        fields: BTreeMap::from([("stock-info".into(), Value::Integer(i64::MAX))]),
        ..Default::default()
    };
    assert_eq!(
        crm.search_filtered(&context(), &filter, 10, None)
            .unwrap()
            .total,
        1
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
    assert_eq!(
        crm.get(
            &Context {
                actor: "outsider".into(),
                ..context()
            },
            "client"
        ),
        Err(Error::Denied)
    );
    assert!(crm
        .get(
            &Context {
                actor: "viewer".into(),
                ..context()
            },
            "client"
        )
        .is_ok());
    assert_eq!(
        crm.execute(
            &Context {
                actor: "viewer".into(),
                ..context()
            },
            request("denied", Command::Create(customer()))
        ),
        Err(Error::Denied)
    );
    let mut stale = crm.get(&context(), "client").unwrap();
    stale.name = "Updated".into();
    crm.execute(
        &context(),
        request("update", Command::Update(stale.clone())),
    )
    .unwrap();
    assert_eq!(
        crm.execute(&context(), request("stale", Command::Update(stale))),
        Err(Error::Conflict)
    );
    assert_eq!(db.store().load("business").unwrap().revision, 6);
}

fn child(db: &Database, mode: &str) -> std::process::Child {
    Process::new(std::env::current_exe().unwrap())
        .args(["--exact", "process_worker", "--nocapture"])
        .env("BB_TEST_DB", &db.0)
        .env("BB_TEST_MODE", mode)
        .spawn()
        .unwrap()
}

#[test]
fn process_worker() {
    let Some(path) = std::env::var_os("BB_TEST_DB") else {
        return;
    };
    let mode = std::env::var("BB_TEST_MODE").unwrap();
    let mut store = SqliteStore::open(path.clone(), 3, 1024 * 1024).unwrap();
    if mode == "abort" {
        drop(store);
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "BEGIN IMMEDIATE; UPDATE bb_workspaces SET snapshot=x'00' WHERE workspace='business';",
        )
        .unwrap();
        std::process::exit(0); // No rollback/Drop; process dies with an uncommitted transaction.
    }
    if mode == "race" {
        let next = Snapshot {
            revision: 1,
            ..Default::default()
        };
        match store.compare_exchange("race", 0, next) {
            Ok(()) => std::process::exit(0),
            Err(Error::Conflict) => std::process::exit(10),
            other => panic!("unexpected race result: {other:?}"),
        }
    }
    let mut crm = Crm::new(store, Grants, Limits::default());
    let mut record = crm.get(&context(), "client").unwrap();
    record.name = "Committed without response".into();
    crm.execute(&context(), request("uncertain", Command::Update(record)))
        .unwrap();
    std::process::exit(0); // Commit completed; response never delivered.
}

#[test]
fn process_exit_rollback_and_uncertain_commit_reconcile_after_restart() {
    let db = Database::new();
    {
        seed(&mut engine(&db));
    }
    let original = db.store().load("business").unwrap();
    assert!(child(&db, "abort").wait().unwrap().success());
    assert_eq!(db.store().load("business").unwrap(), original);
    let mut changed = original.records["client"].clone();
    changed.name = "Committed without response".into();
    let req = request("uncertain", Command::Update(changed));
    assert!(child(&db, "commit").wait().unwrap().success());
    let conn = Connection::open(&db.0).unwrap();
    let bytes: Vec<u8> = conn
        .query_row(
            "SELECT snapshot FROM bb_workspaces WHERE workspace='business'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let saved: Snapshot = serde_json::from_slice(&bytes).unwrap();
    drop(conn);
    assert_eq!(saved.records["client"].name, "Committed without response");
    assert_eq!(saved.receipts["uncertain"].request, req);
    assert_eq!(
        engine(&db).execute(&context(), req).unwrap(),
        saved.receipts["uncertain"].outcome
    );
    assert_eq!(db.store().load("business").unwrap(), saved);
    assert_eq!(
        engine(&db).execute(
            &context(),
            request("uncertain", Command::Create(customer()))
        ),
        Err(Error::OperationReused)
    );
}

#[test]
fn two_processes_cannot_both_commit_the_same_revision() {
    let db = Database::new();
    drop(db.store());
    let mut a = child(&db, "race");
    let mut b = child(&db, "race");
    let mut codes = [
        a.wait().unwrap().code().unwrap(),
        b.wait().unwrap().code().unwrap(),
    ];
    codes.sort();
    assert_eq!(codes, [0, 10]);
    assert_eq!(db.store().load("race").unwrap().revision, 1);
}

#[test]
fn storage_budgets_corruption_and_versions_fail_without_overwrite() {
    let db = Database::new();
    let mut store = SqliteStore::open(&db.0, 1, 1024).unwrap();
    let next = Snapshot {
        revision: 1,
        ..Default::default()
    };
    store.compare_exchange("a", 0, next.clone()).unwrap();
    assert_eq!(
        store.compare_exchange("b", 0, next.clone()),
        Err(Error::Capacity("workspaces"))
    );
    let mut too_big = next.clone();
    too_big.revision = 2;
    too_big.records.insert(
        "c".into(),
        Record {
            description: "x".repeat(2000),
            ..customer()
        },
    );
    assert_eq!(
        store.compare_exchange("a", 1, too_big),
        Err(Error::Capacity("snapshot bytes"))
    );
    let mut invalid = next.clone();
    invalid.schema_version = 1;
    assert_eq!(
        store.compare_exchange("a", 0, invalid),
        Err(Error::Invalid("snapshot schema"))
    );
    assert_eq!(store.load("a").unwrap(), next);
    drop(store);
    let conn = Connection::open(&db.0).unwrap();
    conn.execute("UPDATE bb_workspaces SET revision='9'", [])
        .unwrap();
    assert_eq!(
        db.store().load("a"),
        Err(Error::Invalid("snapshot revision"))
    );
    conn.execute("UPDATE bb_workspaces SET snapshot=?1", [vec![0u8; 2000]])
        .unwrap();
    assert_eq!(
        SqliteStore::open(&db.0, 1, 1024).unwrap().load("a"),
        Err(Error::Capacity("snapshot bytes"))
    );
    conn.execute("UPDATE bb_workspaces SET snapshot=x'00'", [])
        .unwrap();
    assert_eq!(
        db.store().load("a"),
        Err(Error::Invalid("snapshot encoding"))
    );
    conn.execute_batch("PRAGMA user_version=99;").unwrap();
    assert!(matches!(
        SqliteStore::open(&db.0, 1, 1024),
        Err(Error::Invalid("database schema"))
    ));
}
