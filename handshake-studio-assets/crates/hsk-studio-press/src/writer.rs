//! Canonical object serialization and the full-save writer. A full save writes ONLY objects
//! reachable from the trailer (`/Root`, `/Info`), as one fresh file: no incremental tail, no
//! orphan objects, no superseded revisions. Unmodified objects are copied verbatim from the
//! source bytes; modified/new objects are serialized canonically.
//!
//! Hostile-input bounds: the cross-reference table of a save has one 20-byte line per object number
//! up to the highest live one, so that number is limited to `Limits::max_object_number` and the
//! table size is checked against `Limits::max_bytes` before a line is written (test
//! `hostile_parser_dims_u32_max`).
#![deny(clippy::arithmetic_side_effects)]
use crate::error::{Limits, PressError, Result};
use crate::reader::{Entry, check_cancel};
use crate::syntax::{Dict, Object};
use hsk_studio_accord::CancellationToken;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn write_object(obj: &Object, out: &mut Vec<u8>) {
    match obj {
        Object::Null => out.extend_from_slice(b"null"),
        Object::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Object::Int(v) => out.extend_from_slice(v.to_string().as_bytes()),
        Object::Real(v) => out.extend_from_slice(format_real(*v).as_bytes()),
        Object::Name(n) => write_name(n, out),
        Object::Str(s) => write_string(s, out),
        Object::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                write_object(item, out);
            }
            out.push(b']');
        }
        Object::Dict(d) => write_dict(d, out),
        Object::Stream(s) => {
            let mut dict = s.dict.clone();
            dict.set("Length", Object::Int(i64::try_from(s.data.len()).unwrap_or(i64::MAX)));
            write_dict(&dict, out);
            out.extend_from_slice(b"\nstream\n");
            out.extend_from_slice(&s.data);
            out.extend_from_slice(b"\nendstream");
        }
        Object::Ref(r) => out.extend_from_slice(format!("{} {} R", r.num, r.generation).as_bytes()),
    }
}

pub(crate) fn write_dict(d: &Dict, out: &mut Vec<u8>) {
    out.extend_from_slice(b"<<");
    for (k, v) in &d.entries {
        out.push(b' ');
        write_name(k, out);
        out.push(b' ');
        write_object(v, out);
    }
    out.extend_from_slice(b" >>");
}

pub(crate) fn format_real(v: f64) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" || s == "-0" { "0".into() } else { s.into() }
}

pub(crate) fn write_name(n: &[u8], out: &mut Vec<u8>) {
    out.push(b'/');
    for &b in n {
        if (0x21..=0x7e).contains(&b) && b != b'#' && crate::syntax::is_regular(b) {
            out.push(b);
        } else {
            out.extend_from_slice(format!("#{b:02X}").as_bytes());
        }
    }
}

fn write_string(s: &[u8], out: &mut Vec<u8>) {
    out.push(b'(');
    for &b in s {
        match b {
            b'(' | b')' | b'\\' => {
                out.push(b'\\');
                out.push(b);
            }
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            0x20..=0x7e => out.push(b),
            _ => out.extend_from_slice(format!("\\{b:03o}").as_bytes()),
        }
    }
    out.push(b')');
}

/// Every object number reachable from `roots` by following indirect references. Iterative with a
/// visited set, so reference cycles and very deep chains neither recurse nor loop.
pub(crate) fn reachable(objects: &BTreeMap<u32, Entry>, roots: &[u32]) -> BTreeSet<u32> {
    let mut seen = BTreeSet::new();
    let mut work: Vec<u32> = roots.to_vec();
    while let Some(num) = work.pop() {
        if !seen.insert(num) {
            continue;
        }
        let Some(entry) = objects.get(&num) else {
            seen.remove(&num);
            continue;
        };
        let mut stack: Vec<&Object> = vec![&entry.obj];
        while let Some(o) = stack.pop() {
            match o {
                Object::Ref(r) => work.push(r.num),
                Object::Array(a) => stack.extend(a.iter()),
                Object::Dict(d) => stack.extend(d.entries.iter().map(|(_, v)| v)),
                Object::Stream(s) => stack.extend(s.dict.entries.iter().map(|(_, v)| v)),
                _ => {}
            }
        }
    }
    seen
}

pub(crate) fn trailer_roots(trailer: &Dict) -> Vec<u32> {
    ["Root", "Info"]
        .iter()
        .filter_map(|k| trailer.get(k).and_then(Object::as_ref).map(|r| r.num))
        .collect()
}

pub(crate) fn write_document(
    version: &str,
    objects: &BTreeMap<u32, Entry>,
    source: &[u8],
    trailer: &Dict,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<Vec<u8>> {
    let root = trailer.get("Root").and_then(Object::as_ref).ok_or(PressError::Malformed("no_root"))?;
    if !objects.contains_key(&root.num) {
        return Err(PressError::Malformed("no_root"));
    }
    let live = reachable(objects, &trailer_roots(trailer));
    let highest = live.iter().next_back().copied().unwrap_or(0);
    if highest > limits.max_object_number {
        return Err(PressError::InputLimit("object_number"));
    }
    let size = highest.checked_add(1).ok_or(PressError::InputLimit("object_number"))?;
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(format!("%PDF-{}\n", if version.is_empty() { "1.7" } else { version }).as_bytes());
    out.extend_from_slice(b"%\xE2\xE3\xCF\xD3\n");
    let mut offsets: BTreeMap<u32, (usize, u16)> = BTreeMap::new();
    for (i, num) in live.iter().enumerate() {
        if i % 256 == 0 {
            check_cancel(cancel)?;
        }
        let Some(entry) = objects.get(num) else { continue };
        offsets.insert(*num, (out.len(), entry.generation));
        out.extend_from_slice(format!("{} {} obj\n", num, entry.generation).as_bytes());
        match entry.span {
            Some((s, e)) if e <= source.len() && s <= e => out.extend_from_slice(source.get(s..e).unwrap_or(&[])),
            _ => write_object(&entry.obj, &mut out),
        }
        out.extend_from_slice(b"\nendobj\n");
        if out.len() > limits.max_bytes {
            return Err(PressError::InputLimit("max_output_bytes"));
        }
    }
    // One 20-byte line per object number: refuse before writing a table the limit cannot hold.
    let table = usize::try_from(size).unwrap_or(usize::MAX).saturating_mul(20);
    if out.len().saturating_add(table) > limits.max_bytes {
        return Err(PressError::InputLimit("max_output_bytes"));
    }
    let xref_pos = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    // Free list: each free entry names the next free object number (0 = end). A run of live
    // objects is walked once, by the free entry that precedes it.
    let next_free = |after: u32| -> u32 {
        let mut n = after.saturating_add(1);
        while n < size {
            if !offsets.contains_key(&n) {
                return n;
            }
            n = n.saturating_add(1);
        }
        0
    };
    out.extend_from_slice(format!("{:010} 65535 f \n", next_free(0)).as_bytes());
    for n in 1..size {
        match offsets.get(&n) {
            Some((off, generation)) => out.extend_from_slice(format!("{off:010} {generation:05} n \n").as_bytes()),
            None => out.extend_from_slice(format!("{:010} 00001 f \n", next_free(n)).as_bytes()),
        }
    }
    let mut t = Dict::default();
    t.set("Size", Object::Int(i64::from(size)));
    t.set("Root", Object::Ref(root));
    if let Some(info) = trailer.get("Info").and_then(Object::as_ref).filter(|r| live.contains(&r.num)) {
        t.set("Info", Object::Ref(info));
    }
    if let Some(id @ Object::Array(_)) = trailer.get("ID") {
        t.set("ID", id.clone());
    }
    out.extend_from_slice(b"trailer\n");
    write_dict(&t, &mut out);
    out.extend_from_slice(format!("\nstartxref\n{xref_pos}\n%%EOF\n").as_bytes());
    if out.len() > limits.max_bytes {
        return Err(PressError::InputLimit("max_output_bytes"));
    }
    Ok(out)
}
