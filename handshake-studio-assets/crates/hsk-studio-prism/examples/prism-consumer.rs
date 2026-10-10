//! External-input CLI; all paths/identities/revisions are explicit caller inputs.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::{DeliveryClass, DeliveryError, MAX_FRAME_BYTES, SinkPort};
use hsk_studio_prism::*;
use std::{
    collections::BTreeMap,
    env,
    fs::File,
    io::{self, Read, Write},
};

struct Collector {
    bytes: Vec<u8>,
    reject: bool,
}
impl SinkPort for Collector {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        if self.reject {
            return Err(DeliveryError::Rejected);
        }
        if class != DeliveryClass::Terminal
            || bytes.len() > MAX_FRAME_BYTES
            || !self.bytes.is_empty()
        {
            return Err(DeliveryError::Saturated);
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
}
fn read_bounded(path: &str, limit: usize) -> Result<Vec<u8>, &'static str> {
    let mut out = Vec::new();
    File::open(path)
        .map_err(|_| "input_unavailable")?
        .take((limit + 1) as u64)
        .read_to_end(&mut out)
        .map_err(|_| "input_unavailable")?;
    if out.len() > limit {
        return Err("input_limit");
    }
    Ok(out)
}
fn digest(text: &str) -> Result<[u8; 32], &'static str> {
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid_hash");
    }
    let mut out = [0; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).map_err(|_| "invalid_hash")?;
    }
    Ok(out)
}
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(&mut s, "{b:02x}").expect("string write");
    }
    s
}
fn get<'a>(args: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str, &'static str> {
    args.get(key).map(String::as_str).ok_or("missing_argument")
}
fn run() -> Result<(), &'static str> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args == ["--help"] || args == ["--descriptor"] {
        println!("{DESCRIPTOR}");
        return Ok(());
    }
    if args.len() > 48 || args.iter().any(|a| a.len() > 4096) {
        return Err("argument_limit");
    }
    let mut values = BTreeMap::new();
    let mut cancel_flag = false;
    let mut reject = false;
    let mut i = 0;
    while i < args.len() {
        let key = &args[i];
        if ["--cancel", "--reject-sink"].contains(&key.as_str()) {
            let flag = if key == "--cancel" {
                &mut cancel_flag
            } else {
                &mut reject
            };
            if *flag {
                return Err("duplicate_argument");
            }
            *flag = true;
            i += 1;
            continue;
        }
        if ![
            "--source",
            "--destination",
            "--source-hash",
            "--destination-hash",
            "--pixels",
            "--depth",
            "--intent",
            "--revision",
            "--expected-revision",
            "--source-id",
            "--destination-id",
            "--resource-id",
            "--correlation",
            "--account",
            "--principal",
            "--owner-account",
            "--owner-principal",
            "--access-space",
            "--session",
        ]
        .contains(&key.as_str())
        {
            return Err("unknown_argument");
        }
        let value = args.get(i + 1).ok_or("missing_argument")?;
        if values.insert(key.clone(), value.clone()).is_some() {
            return Err("duplicate_argument");
        }
        i += 2;
    }
    let source = read_bounded(get(&values, "--source")?, MAX_PROFILE_BYTES)?;
    let destination = read_bounded(get(&values, "--destination")?, MAX_PROFILE_BYTES)?;
    let text = read_bounded(get(&values, "--pixels")?, 262144)?;
    let text = std::str::from_utf8(&text).map_err(|_| "pixel_syntax")?;
    let mut channels = Vec::new();
    for value in text.split_whitespace() {
        if channels.len() >= MAX_PIXELS * 3 {
            return Err("pixel_limit");
        }
        channels.push(value.parse::<f32>().map_err(|_| "pixel_syntax")?);
    }
    if !channels.len().is_multiple_of(3) {
        return Err("pixel_syntax");
    }
    let pixels: Vec<[f32; 3]> = channels
        .chunks_exact(3)
        .map(|p| [p[0], p[1], p[2]])
        .collect();
    let sid = DomainId::parse(get(&values, "--source-id")?).map_err(|_| "invalid_id")?;
    let did = DomainId::parse(get(&values, "--destination-id")?).map_err(|_| "invalid_id")?;
    let rid = DomainId::parse(get(&values, "--resource-id")?).map_err(|_| "invalid_id")?;
    let actor = ActorContext::new(
        get(&values, "--account")?,
        get(&values, "--principal")?,
        get(&values, "--owner-account")?,
        get(&values, "--owner-principal")?,
        get(&values, "--access-space")?,
        get(&values, "--session")?,
    )
    .map_err(|_| "invalid_context")?;
    let intent = match get(&values, "--intent")? {
        "relative_colorimetric" => Intent::RelativeColorimetric,
        "perceptual" => Intent::Perceptual,
        "saturation" => Intent::Saturation,
        "absolute_colorimetric" => Intent::AbsoluteColorimetric,
        _ => return Err("unsupported_intent"),
    };
    let request = TransformRequest {
        source: ProfileInput {
            profile_id: &sid,
            bytes: &source,
            expected_sha256: digest(get(&values, "--source-hash")?)?,
        },
        destination: ProfileInput {
            profile_id: &did,
            bytes: &destination,
            expected_sha256: digest(get(&values, "--destination-hash")?)?,
        },
        pixels: &pixels,
        intent,
        bit_depth: get(&values, "--depth")?
            .parse()
            .map_err(|_| "invalid_depth")?,
        black_point_compensation: false,
        revision: get(&values, "--revision")?
            .parse()
            .map_err(|_| "invalid_revision")?,
        expected_revision: get(&values, "--expected-revision")?
            .parse()
            .map_err(|_| "invalid_revision")?,
        correlation_id: get(&values, "--correlation")?
            .parse()
            .map_err(|_| "invalid_correlation")?,
        resource_id: &rid,
        actor: &actor,
        private_project_text: None,
    };
    let mut engine = Prism::new(actor.clone(), 1).map_err(Error::code)?;
    let token = CancellationToken::default();
    if cancel_flag {
        token.cancel();
    }
    let mut collector = Collector {
        bytes: Vec::with_capacity(MAX_FRAME_BYTES),
        reject,
    };
    let result = engine.transform(&request, &token, &mut collector);
    let mut output = String::new();
    use std::fmt::Write;
    match result {
        Ok(result) => {
            let r = result.receipt;
            write!(&mut output,"{{\"status\":\"transformed\",\"source_id\":\"{}\",\"destination_id\":\"{}\",\"source_hash\":\"{}\",\"destination_hash\":\"{}\",\"icc_version\":\"{}\",\"engine\":\"{}\",\"engine_version\":\"{}\",\"intent\":\"{}\",\"bit_depth\":{},\"revision\":{},\"correlation\":{},\"cache_hit\":{},\"options\":{{\"analytical\":true,\"cicp\":false,\"fixed_point\":false,\"extended_range_rgb_xyz\":true,\"bpc\":false}},\"delivery\":\"accepted\",\"diagnostic_frame_hex\":\"{}\",\"pixels\":[",
                r.source_profile_id.as_str(),r.destination_profile_id.as_str(),hash_hex(&r.source_sha256),hash_hex(&r.destination_sha256),
                r.icc_version,r.engine_name,r.engine_version,r.intent.code(),r.bit_depth,r.revision,r.correlation_id,r.cache_hit,hex(&collector.bytes)).expect("string write");
            for (i, p) in result.pixels.iter().enumerate() {
                if i > 0 {
                    output.push(',');
                }
                write!(&mut output, "[{},{},{}]", p[0], p[1], p[2]).expect("string write");
            }
            output.push_str("]}\n");
        }
        Err(failure) => {
            let delivery = match failure.delivery {
                Ok(_) => "accepted",
                Err(e) => e.code(),
            };
            writeln!(&mut output,"{{\"status\":\"rejected\",\"error\":\"{}\",\"operation_error\":\"{}\",\"delivery\":\"{}\",\"dropped_attempts\":{},\"reconciliation_required\":{},\"diagnostic_frame_hex\":\"{}\"}}",
                failure.error.code(),failure.operation_error.map_or("none",Error::code),delivery,failure.diagnostic_state.dropped_attempts,
                failure.diagnostic_state.reconciliation_required,hex(&collector.bytes)).expect("string write");
            io::stdout()
                .lock()
                .write_all(output.as_bytes())
                .map_err(|_| "consumer_output_indeterminate")?;
            return Err(failure.error.code());
        }
    }
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(output.as_bytes())
        .map_err(|_| "consumer_output_indeterminate")?;
    stdout
        .flush()
        .map_err(|_| "consumer_output_indeterminate")?;
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("rejected:{error}; use --descriptor for supported inputs/recovery");
        std::process::exit(2);
    }
}
