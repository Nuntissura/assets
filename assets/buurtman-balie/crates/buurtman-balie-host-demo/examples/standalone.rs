use buurtman_balie::{memory::MemoryStore, *};
use std::collections::BTreeMap;
struct DemoAuthorization;
impl Authorization for DemoAuthorization {
    fn allows(&self, ctx: &Context, _: Access) -> bool {
        ctx.workspace == "demo" && ctx.actor == "demo-operator"
    }
}
fn main() -> Result<(), Error> {
    let mut crm = Crm::new(MemoryStore::new(1), DemoAuthorization, Limits::default());
    let context = Context {
        workspace: "demo".into(),
        actor: "demo-operator".into(),
    };
    for (id, name, kind) in [
        ("customer-1", "Camera customer", Kind::Customer),
        ("case-1", "Camera delivery feedback", Kind::SupportCase),
    ] {
        let case_status = (kind == Kind::SupportCase).then_some(CaseStatus::New);
        crm.execute(
            &context,
            Request {
                operation_id: format!("create-{id}"),
                command: Command::Create(Record {
                    id: id.into(),
                    client_terms: (kind == Kind::Customer).then_some(ClientTerms {
                        classification: ClientClass::B2C,
                        pricing: ClientPricing::Standard,
                    }),
                    prices: None,
                    kind,
                    name: name.into(),
                    description: String::new(),
                    tags: vec!["delivery".into()],
                    fields: BTreeMap::new(),
                    case_status,
                    revision: 0,
                }),
            },
        )?;
    }
    crm.execute(
        &context,
        Request {
            operation_id: "link-customer".into(),
            command: Command::Link(Relation {
                source: "case-1".into(),
                label: "customer".into(),
                target: Target::Record("customer-1".into()),
            }),
        },
    )?;
    let page = crm.search(&context, "camra", None, 10, None)?;
    println!(
        "Buurtman Balie API {API_VERSION}: {} related records found",
        page.total
    );
    for hit in page.hits {
        println!(
            "{}: {} ({} related records)",
            hit.record.id,
            hit.record.name,
            crm.relations(&context, &hit.record.id)?.len()
        );
    }
    Ok(())
}
