//! External immutable byte inputs and explicit caller-owned source context; no GUI/host grants.
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort, TileAddress};
use hsk_studio_pigment::{
    caller::{MemoryAdmission, SerializedOwner},
    wire::*,
    *,
};
use std::{
    fs::File,
    io::{Read, Write},
};
struct Sink {
    frames: Vec<Vec<u8>>,
    mode: String,
}
impl SinkPort for Sink {
    fn try_send(&mut self, _: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        match self.mode.as_str() {
            "accepted" => {
                if self.frames.len() >= 65 {
                    return Err(DeliveryError::Saturated);
                }
                self.frames.push(bytes.to_vec());
                Ok(())
            }
            "rejected" => Err(DeliveryError::Rejected),
            "saturated" => Err(DeliveryError::Saturated),
            "unavailable" => Err(DeliveryError::Unavailable),
            "indeterminate" => Err(DeliveryError::Indeterminate),
            _ => Err(DeliveryError::Rejected),
        }
    }
}
fn read(path: &str, limit: usize) -> Result<Vec<u8>, Error> {
    if path.is_empty() || path.len() > 4096 {
        return Err(Error::InvalidInput);
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| Error::InvalidInput)?
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::InvalidInput)?;
    if bytes.len() > limit {
        return Err(Error::BudgetExceeded);
    }
    Ok(bytes)
}
fn load(tile: &Tile) -> Result<LoadedTile, Error> {
    Ok(LoadedTile {
        colour: read(&tile.colour_path, MAX_BYTES as usize)?,
        alpha: read(&tile.alpha_path, MAX_BYTES as usize)?,
        profile: read(&tile.profile_path, hsk_studio_prism::MAX_PROFILE_BYTES)?,
        id: DomainId::parse(&tile.tile_ref.colour_profile_id).map_err(|_| Error::InvalidInput)?,
    })
}
fn run() -> Result<(), Error> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--help"] || args == ["--descriptor"] {
        println!("{DESCRIPTOR}");
        return Ok(());
    }
    if !(args.len() == 2 || args.len() == 3 && args[2] == "--cancel") || args[0] != "--input" {
        return Err(Error::InvalidInput);
    }
    let input = parse(&read(&args[1], 262144)?)?;
    let actor = input.actor.validated()?;
    let document = DomainId::parse(&input.document_id).map_err(|_| Error::InvalidInput)?;
    let layer = DomainId::parse(&input.layer_id).map_err(|_| Error::InvalidInput)?;
    let mut loaded = Vec::new();
    let mut total = 0usize;
    for item in &input.tiles {
        let before = load(&item.before)?;
        let replacement = load(&item.replacement)?;
        for t in [&before, &replacement] {
            total = total
                .checked_add(t.colour.len())
                .and_then(|n| n.checked_add(t.alpha.len()))
                .and_then(|n| n.checked_add(t.profile.len()))
                .ok_or(Error::Overflow)?
        }
        if total > MAX_BYTES as usize {
            return Err(Error::BudgetExceeded);
        }
        loaded.push((before, replacement))
    }
    let tiles: Vec<_> = input
        .tiles
        .iter()
        .zip(&loaded)
        .map(|(item, (before, replacement))| {
            Ok(TileItem {
                before: before.borrowed(&item.before)?,
                replacement: replacement.borrowed(&item.replacement)?,
                expected_property_revision: item.expected_property_revision,
            })
        })
        .collect::<Result<_, Error>>()?;
    let mask_bytes = read(&input.mask.path, MAX_BYTES as usize)?;
    if total.checked_add(mask_bytes.len()).ok_or(Error::Overflow)? > MAX_BYTES as usize {
        return Err(Error::BudgetExceeded);
    }
    let revisions = input
        .current
        .revisions
        .iter()
        .map(|v| {
            Ok((
                TileAddress::new(
                    DomainId::parse(&v.layer_id).map_err(|_| Error::InvalidInput)?,
                    &v.object_key,
                    v.column,
                    v.row,
                )
                .map_err(|_| Error::InvalidInput)?,
                v.revision,
            ))
        })
        .collect::<Result<Vec<_>, Error>>()?;
    if ![
        "accepted",
        "rejected",
        "saturated",
        "unavailable",
        "indeterminate",
    ]
    .contains(&input.current.sink.as_str())
    {
        return Err(Error::InvalidInput);
    }
    // A one-shot process cannot retain an unresolved result across process exit.
    // Actual pending ownership is provided by the reusable caller port, not a fake token here.
    if input.current.sink == "indeterminate" {
        return Err(Error::UnsupportedFinalization);
    }
    let mut owner = SerializedOwner::new(
        DomainId::parse(&input.current.document_id).map_err(|_| Error::InvalidInput)?,
        DomainId::parse(&input.current.layer_id).map_err(|_| Error::InvalidInput)?,
        input.current.actor.validated()?,
        input.current.cancel_epoch,
        revisions,
        MemoryAdmission::new(input.current.byte_limit)?,
        Sink {
            frames: Vec::new(),
            mode: input.current.sink,
        },
    )?;
    let request = Request {
        transport_version: input.transport_version,
        operation: &input.operation,
        document_id: &document,
        layer_id: &layer,
        command_id: &input.command_id,
        correlation_id: input.correlation_id,
        actor: &actor,
        base_revision: input.base_revision,
        cancel_epoch: input.cancel_epoch,
        grid: input.grid,
        rectangle: input.rectangle,
        mask: Mask {
            bounds: input.mask.bounds,
            sample_format: &input.mask.sample_format,
            bytes: &mask_bytes,
            stride_bytes: input.mask.stride_bytes,
            sha256: digest(&input.mask.sha256)?,
        },
        tiles: &tiles,
        byte_limit: input.byte_limit,
    };
    let token = CancellationToken::default();
    if args.len() == 3 {
        token.cancel()
    }
    let outcome = masked_replace(&request, &mut owner, &token);
    let output = output(&outcome, &owner.sink.frames);
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &output).map_err(|_| Error::DeliveryFailed)?;
    stdout.write_all(b"\n").map_err(|_| Error::DeliveryFailed)?;
    stdout.flush().map_err(|_| Error::DeliveryFailed)?;
    if matches!(outcome, Outcome::ReconciliationRequired { .. }) {
        return Err(Error::ReconciliationRequired);
    }
    if let Outcome::Rejected(f) = outcome {
        return Err(f.error);
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("rejected:{}; use --descriptor", e.code());
        std::process::exit(2)
    }
}
