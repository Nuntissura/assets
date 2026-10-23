//! Exact predecessor promotion, independently reconciled with canonical main.

use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

fn exact_id(id: &str) -> bool {
    let Some(tail) = id.strip_prefix("WP-FF-") else {
        return false;
    };
    let Some((number, rest)) = tail.split_once('-') else {
        return false;
    };
    let Some((slug, version)) = rest.rsplit_once("-v") else {
        return false;
    };
    number.len() == 3
        && number.bytes().all(|byte| byte.is_ascii_digit())
        && !slug.is_empty()
        && slug.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
        && !version.starts_with('0')
        && !version.is_empty()
        && version.bytes().all(|byte| byte.is_ascii_digit())
}

pub(super) fn run(root: &Path, id: &str) -> Result<(), String> {
    if !exact_id(id) {
        return Err("FF-PROMOTION-E-EXACT-ID".into());
    }
    let packet: Value = serde_json::from_slice(
        &fs::read(root.join(format!(".GOV/work_packets/{id}/packet.json")))
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if packet.pointer("/identity/wp_id").and_then(Value::as_str) != Some(id) {
        return Err("FF-PROMOTION-E-PACKET-ID".into());
    }
    let count = check(root, &packet)?;
    println!(
        "{}",
        serde_json::json!({"status":"PASS","packet":id,"validated_predecessors":count})
    );
    Ok(())
}

/// Checks every exact predecessor named by the packet, without trusting its claims.
pub(super) fn check(root: &Path, packet: &Value) -> Result<usize, String> {
    let dependencies = packet
        .pointer("/scope/dependencies")
        .and_then(Value::as_array)
        .ok_or("FF-PROMOTION-E-DEPENDENCIES: dependency array missing")?;
    let mut ids = BTreeSet::new();
    let mut bases = BTreeSet::new();
    for dependency in dependencies {
        let text = dependency
            .as_str()
            .ok_or("FF-PROMOTION-E-DEPENDENCIES: non-string dependency")?;
        for word in text
            .split_whitespace()
            .filter(|word| word.starts_with("WP-FF-"))
        {
            if text.contains(" or ") || text.contains("..") || text.contains('|') {
                return Err(
                    "FF-PROMOTION-E-ALTERNATIVE: dependency alternatives are not exact bindings"
                        .into(),
                );
            }
            let id = word.trim_end_matches([',', ';', '.', ')']);
            if !exact_id(id) {
                return Err(format!("FF-PROMOTION-E-EXACT-ID: {id}"));
            }
            let (base, _) = id.rsplit_once("-v").ok_or("FF-PROMOTION-E-EXACT-ID")?;
            if !ids.contains(id) && !bases.insert(base.to_owned()) {
                return Err(format!("FF-PROMOTION-E-ALTERNATIVE: {base}"));
            }
            ids.insert(id.to_owned());
        }
    }
    if ids.is_empty() || ids.len() > 64 {
        return Err("FF-PROMOTION-E-DEPENDENCIES: no bounded exact predecessors".into());
    }
    if packet
        .pointer("/identity/wp_id")
        .and_then(Value::as_str)
        .is_some_and(|id| id.starts_with("WP-FF-011-"))
        && (!ids.contains("WP-FF-020-equivalent-work-benchmark-design-v1")
            || !ids.contains("WP-FF-021-first-runnable-production-slice-v1"))
    {
        return Err("FF-PROMOTION-E-PHASE-BINDING: WP011 requires exact WP020 and WP021".into());
    }
    for id in &ids {
        let path = format!(".GOV/work_packets/{id}/packet.json");
        let local: Value = serde_json::from_slice(
            &fs::read(root.join(&path))
                .map_err(|error| format!("FF-PROMOTION-E-MISSING: {id}: {error}"))?,
        )
        .map_err(|error| format!("FF-PROMOTION-E-PACKET: {id}: {error}"))?;
        let canonical: Value = serde_json::from_str(&super::command_output(
            root,
            "git",
            &["show", &format!("main:{path}")],
        )?)
        .map_err(|error| format!("FF-PROMOTION-E-CANONICAL: {id}: {error}"))?;
        if local != canonical
            || canonical.pointer("/identity/wp_id").and_then(Value::as_str) != Some(id.as_str())
        {
            return Err(format!("FF-PROMOTION-E-FORGED: {id}"));
        }
        if canonical
            .pointer("/lifecycle/status")
            .and_then(Value::as_str)
            != Some("VALIDATED")
        {
            return Err(format!("FF-PROMOTION-E-STATUS: {id}"));
        }
        if !canonical
            .pointer("/lifecycle/superseded_by")
            .is_some_and(Value::is_null)
        {
            return Err(format!("FF-PROMOTION-E-SUPERSEDED: {id}"));
        }
        let closure = canonical
            .pointer("/source_control/closure_commit")
            .and_then(Value::as_str)
            .filter(|sha| sha.len() == 40 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or_else(|| format!("FF-PROMOTION-E-CLOSURE: {id}"))?;
        super::command_output(
            root,
            "git",
            &["merge-base", "--is-ancestor", closure, "main"],
        )
        .map_err(|error| format!("FF-PROMOTION-E-CONTAINMENT: {id}: {error}"))?;
    }
    Ok(ids.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn exact_predecessor_public_boundary_rejects_unproven_claims() {
        let root = super::super::disposable_artifact_root(
            &super::super::repo_root().expect("repo"),
        )
        .join(format!(
            "promotion-fixture-{}-{}",
            std::process::id(),
            super::super::SystemTime::now()
                .duration_since(super::super::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("fixture root");
        let git =
            |args: &[&str]| super::super::command_output(&root, "git", args).expect("fixture git");
        git(&["init", "-b", "main"]);
        git(&["config", "user.name", "promotion-fixture"]);
        git(&["config", "user.email", "promotion-fixture@invalid"]);
        fs::write(root.join(".gitignore"), ".fforager-artifacts/\n").expect("ignore captures");
        git(&["add", ".gitignore"]);
        git(&["commit", "-m", "base"]);
        let closure = git(&["rev-parse", "HEAD"]).trim().to_owned();
        let id = "WP-FF-999-promotion-fixture-v1";
        let path = root.join(format!(".GOV/work_packets/{id}/packet.json"));
        fs::create_dir_all(path.parent().expect("packet parent")).expect("packet folder");
        let good = json!({"identity":{"wp_id":id},"lifecycle":{"status":"VALIDATED","superseded_by":null},"source_control":{"closure_commit":closure}});
        let consumer = json!({"scope":{"dependencies":[id]}});
        let persist = |value: &Value| {
            fs::write(&path, serde_json::to_vec(value).expect("packet JSON"))
                .expect("packet write");
            git(&["add", ".GOV"]);
            git(&["commit", "--allow-empty", "-m", "packet"]);
        };
        persist(&good);
        assert_eq!(check(&root, &consumer), Ok(1));
        let consumer_id = "WP-FF-998-consumer-fixture-v1";
        let consumer_path = root.join(format!(".GOV/work_packets/{consumer_id}/packet.json"));
        fs::create_dir_all(consumer_path.parent().expect("consumer parent"))
            .expect("consumer folder");
        let mut public_consumer = consumer.clone();
        public_consumer["identity"] = json!({"wp_id":consumer_id});
        fs::write(
            consumer_path,
            serde_json::to_vec(&public_consumer).expect("consumer JSON"),
        )
        .expect("consumer write");
        run(&root, consumer_id)
            .expect("actual public handler accepts independently contained predecessor");
        assert!(
            run(&root, id).is_err(),
            "predecessor itself lacks dependencies"
        );
        for dependency in [
            "WP-FF-999-promotion-fixture-v1..v2",
            "WP-FF-999-promotion-fixture-v1 or WP-FF-999-promotion-fixture-v2",
            "WP-FF-998-missing-v1",
        ] {
            assert!(check(&root, &json!({"scope":{"dependencies":[dependency]}})).is_err());
        }
        let tree = git(&["rev-parse", "HEAD^{tree}"]).trim().to_owned();
        let uncontained = git(&["commit-tree", &tree, "-m", "uncontained closure"])
            .trim()
            .to_owned();
        for (pointer, value) in [
            ("/lifecycle/status", json!("STUB")),
            ("/source_control/closure_commit", Value::Null),
            ("/source_control/closure_commit", json!(uncontained)),
            (
                "/lifecycle/superseded_by",
                json!("WP-FF-999-promotion-fixture-v2"),
            ),
        ] {
            let mut malformed = good.clone();
            *malformed.pointer_mut(pointer).expect("fixture pointer") = value;
            persist(&malformed);
            assert!(check(&root, &consumer).is_err(), "accepted {pointer}");
        }
        persist(&good);
        let mut forged = good.clone();
        forged["source_control"]["closure_commit"] =
            json!("0000000000000000000000000000000000000000");
        fs::write(&path, serde_json::to_vec(&forged).expect("forged JSON")).expect("forged write");
        assert!(
            check(&root, &consumer)
                .expect_err("forgery")
                .contains("FF-PROMOTION-E-FORGED")
        );
        assert!(
            check(
                &root,
                &json!({"identity":{"wp_id":"WP-FF-011-next-v1"},"scope":{"dependencies":[id]}})
            )
            .expect_err("phase binding")
            .contains("FF-PROMOTION-E-PHASE-BINDING")
        );
    }
}
