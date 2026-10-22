//! `web-consumer --descriptor | --tree FILE | --set-attr NODEKEY NAME VALUE FILE | --media QUERY`
//! Reads FILE, never writes it: the patched source goes to stdout. Exit 0 ok, 1 typed refusal, 2 usage.
use hsk_studio_accord::CancellationToken;
use hsk_studio_web::{
    DESCRIPTOR, Limits, NodeKey, WebDoc, WebError, parse, responsive::parse_media_query,
    set_attribute,
};
use std::{env, fs, process::ExitCode};

fn load(path: &str) -> Result<WebDoc, String> {
    let src = fs::read_to_string(path).map_err(|_| "unreadable input".to_owned())?;
    parse(&src, 0, &Limits::default(), &CancellationToken::default()).map_err(refusal)
}

fn refusal(e: WebError) -> String {
    format!("refused:{} span={:?}", e.code(), e.span())
}

fn run(args: &[String]) -> Result<Option<String>, String> {
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["--descriptor"] => Ok(Some(format!("{DESCRIPTOR}\n"))),
        ["--tree", file] => {
            let doc = load(file)?;
            let mut out = String::new();
            for row in doc.view() {
                let key = doc
                    .key_of(row.id)
                    .map(|k| k.to_string())
                    .unwrap_or_default();
                out.push_str(&format!(
                    "{} {:?} {} {}..{}\n",
                    "  ".repeat(row.depth),
                    row.kind,
                    key,
                    row.span.start,
                    row.span.end
                ));
            }
            Ok(Some(out))
        }
        ["--set-attr", key, name, value, file] => {
            let doc = load(file)?;
            let patch = set_attribute(&doc, &NodeKey::new(*key), name, value).map_err(refusal)?;
            let applied = hsk_studio_web::apply(&doc, &patch).map_err(refusal)?;
            Ok(Some(applied.source))
        }
        ["--media", query] => {
            let list = parse_media_query(query).map_err(refusal)?;
            Ok(Some(format!("{list:?}\n")))
        }
        _ => Ok(None),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match run(&args) {
        Ok(Some(text)) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Ok(None) => {
            eprintln!(
                "usage: web-consumer --descriptor | --tree FILE | --set-attr NODEKEY NAME VALUE FILE | --media QUERY"
            );
            ExitCode::from(2)
        }
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(1)
        }
    }
}
