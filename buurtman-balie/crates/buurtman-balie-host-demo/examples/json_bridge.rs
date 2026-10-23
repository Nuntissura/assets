//! Local harness only. Host authentication must replace DemoAuthorization in production.
use buurtman_balie::{memory::MemoryStore, *};
use serde_json::{json, Value as Json};
use std::collections::BTreeMap;
use std::io::{self, BufRead, Read};

struct DemoAuthorization;
impl Authorization for DemoAuthorization {
    fn allows(&self, ctx: &Context, _: Access) -> bool {
        ctx.workspace == "demo" && ctx.actor == "demo-operator"
    }
}
fn main() {
    let mut crm = Crm::new(MemoryStore::new(1), DemoAuthorization, Limits::default());
    let context = Context {
        workspace: "demo".into(),
        actor: "demo-operator".into(),
    };
    let mut input = io::stdin().lock();
    loop {
        let mut line = String::new();
        let read = (&mut input).take(131_073).read_line(&mut line);
        if !matches!(read, Ok(n) if n > 0) {
            break;
        }
        if line.len() > 131_072 {
            println!("{}", json!({"error":"request too large"}));
            break;
        }
        let result = (|| -> Result<Json, String> {
            let call: Json = serde_json::from_str(&line).map_err(|e| e.to_string())?;
            match call["action"].as_str() {
                Some("tiers") => {
                    serde_json::to_value(crm.pricing_tiers(&context).map_err(|e| e.to_string())?)
                        .map_err(|e| e.to_string())
                }
                Some("quote") => serde_json::to_value(
                    crm.quote(
                        &context,
                        call["client_id"].as_str().unwrap_or(""),
                        call["product_id"].as_str().unwrap_or(""),
                    )
                    .map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string()),
                Some("fields") => serde_json::to_value(
                    crm.field_definitions(&context).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string()),
                Some("execute") => {
                    let request: Request = serde_json::from_value(call["request"].clone())
                        .map_err(|e| e.to_string())?;
                    serde_json::to_value(crm.execute(&context, request).map_err(|e| e.to_string())?)
                        .map_err(|e| e.to_string())
                }
                Some("search") => {
                    let fields: BTreeMap<String, Value> =
                        serde_json::from_value(call["fields"].clone())
                            .map_err(|e| e.to_string())?;
                    let kind: Option<Kind> =
                        serde_json::from_value(call["kind"].clone()).map_err(|e| e.to_string())?;
                    let filter = SearchFilter {
                        query: call["query"].as_str().unwrap_or("").into(),
                        fields,
                        kind,
                        ..Default::default()
                    };
                    let page = crm
                        .search_filtered(&context, &filter, 100, None)
                        .map_err(|e| e.to_string())?;
                    Ok(
                        json!({"records":page.hits.into_iter().map(|h|h.record).collect::<Vec<_>>(),"total":page.total,"revision":page.revision}),
                    )
                }
                _ => Err("unknown action".into()),
            }
        })();
        println!(
            "{}",
            match result {
                Ok(value) => json!({"value":value}),
                Err(error) => json!({"error":error}),
            }
        );
    }
}
