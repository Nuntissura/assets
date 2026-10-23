//! Thin CLI over hsk-studio-press. Never prints document text.
//!   press-consumer --descriptor
//!   press-consumer --inspect FILE.pdf
//!   press-consumer --set-title NEW FILE.pdf OUT.pdf
//!   press-consumer --redact PAGE X0 Y0 X1 Y1 FILE.pdf OUT.pdf [--partial | --remove-images]
use hsk_studio_accord::CancellationToken;
use hsk_studio_press::{Limits, PayloadLocation, PdfDoc, PdfScalar, RedactionRegion, UnsupportedPolicy, redact_and_save};
use std::process::ExitCode;

fn run(args: &[String]) -> Result<(), String> {
    let cancel = CancellationToken::default();
    let limits = Limits::default();
    let open = |path: &str| -> Result<PdfDoc, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("read: {e}"))?;
        PdfDoc::open(&bytes, &limits, &cancel).map_err(|e| e.to_string())
    };
    match args.first().map(String::as_str) {
        Some("--descriptor") => println!("{}", hsk_studio_press::DESCRIPTOR),
        Some("--inspect") => {
            let doc = open(args.get(1).ok_or("FILE")?)?;
            let i = doc.inspect();
            println!(
                "version={} pages={} objects={} encrypted={} allow_modify={:?} signatures={} signed={} xref_sections={} sha256={}",
                i.version, i.page_count, i.object_count, i.encrypted, i.permissions_allow_modify, i.signature_count, i.signed, i.xref_sections, i.source_sha256
            );
            if !i.encrypted {
                let m = doc.proposal(&cancel).map_err(|e| e.to_string())?;
                println!("fields={} signature_fields={}", m.form_fields.len(), m.signatures.len());
                for p in m.pages.iter().take(5) {
                    println!("page {} rotate={} annots={} content_streams={}", p.index, p.rotate, p.annotations.len(), p.content_streams);
                }
            }
        }
        Some("--set-title") => {
            let (title, file, out) = (args.get(1).ok_or("NEW")?, args.get(2).ok_or("FILE")?, args.get(3).ok_or("OUT")?);
            let mut doc = open(file)?;
            let info = doc.inspect();
            let sha = info.source_sha256.clone();
            let target = doc.info_ref().ok_or("document has no /Info")?;
            doc.edit_entry(target, "Title", PdfScalar::Text(title.clone()), &sha, &cancel).map_err(|e| e.to_string())?;
            let bytes = doc.save_full(&cancel).map_err(|e| e.to_string())?;
            std::fs::write(out, &bytes).map_err(|e| format!("write: {e}"))?;
            println!("saved bytes={}", bytes.len());
        }
        Some("--redact") => {
            let n = |i: usize| -> Result<f64, String> { args.get(i).ok_or("arg")?.parse::<f64>().map_err(|e| e.to_string()) };
            let region = RedactionRegion { page_index: n(1)? as u32, rect: [n(2)?, n(3)?, n(4)?, n(5)?] };
            let (file, out) = (args.get(6).ok_or("FILE")?, args.get(7).ok_or("OUT")?);
            let policy = if args.iter().any(|a| a == "--partial") {
                UnsupportedPolicy::ReportAndContinue
            } else if args.iter().any(|a| a == "--remove-images") {
                UnsupportedPolicy::RemoveImages
            } else {
                UnsupportedPolicy::Refuse
            };
            let mut doc = open(file)?;
            let r = redact_and_save(&mut doc, &[region], policy, &cancel).map_err(|e| e.to_string())?;
            std::fs::write(out, &r.bytes).map_err(|e| format!("write: {e}"))?;
            let rep = &r.reports[0];
            let (mut raw, mut dec, mut strs, mut cont) = (0, 0, 0, 0);
            let mut objs = std::collections::BTreeSet::new();
            for h in &r.proof.hits {
                match h.location {
                    PayloadLocation::RawBytes { .. } => raw += 1,
                    PayloadLocation::DecodedStream { object, .. } | PayloadLocation::DecodedText { object } | PayloadLocation::InlineImage { object, .. } => {
                        dec += 1;
                        objs.insert(object);
                    }
                    PayloadLocation::StringObject { object } => {
                        strs += 1;
                        objs.insert(object);
                    }
                    PayloadLocation::ContentString { object } => {
                        cont += 1;
                        objs.insert(object);
                    }
                }
            }
            println!(
                "leaks raw={raw} decoded={dec} string_objects={strs} content_operands={cont} objects={:?} kept_text_collisions={}",
                objs.iter().take(8).collect::<Vec<_>>(),
                r.proof.kept_text_collisions.len()
            );
            println!("not_proven={:?}", r.not_proven_reasons().iter().map(|n| n.code()).collect::<Vec<_>>());
            println!(
                "strings_removed={} annotations_removed={} unredacted={:?} complete={} layout_shift={} proven={} hits={} unreachable={} xref_sections={} undecoded_streams={} short_needles={} residual_in_region={}",
                rep.text_strings_removed,
                rep.annotations_removed.len(),
                rep.unredacted,
                rep.complete,
                rep.layout_shift_possible,
                r.is_proven(),
                r.proof.hits.len(),
                r.proof.unreachable_objects.len(),
                r.proof.xref_sections,
                r.proof.streams_not_decodable.len(),
                r.short_needles,
                r.residual_in_region
            );
        }
        _ => return Err("usage: --descriptor | --inspect FILE | --set-title NEW FILE OUT | --redact PAGE X0 Y0 X1 Y1 FILE OUT [--partial | --remove-images]".into()),
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}
