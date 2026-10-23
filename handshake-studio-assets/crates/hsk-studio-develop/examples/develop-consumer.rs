//! External-input CLI. `--descriptor`, or `--synthetic [--cancel]`, or
//! `--plane FILE --width W --height H [--pattern RGGB|BGGR|GRBG|GBRG] [--black B] [--white W]
//! [--exposure EV]` (FILE = u16le row-major, stride = 2*width). Prints one receipt line.
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_develop::*;
use hsk_studio_pigment::Grid;
use std::{env, fs, process};

fn parse<T: std::str::FromStr>(args: &[String], flag: &str, default: T) -> Result<T, String> {
    match args.iter().position(|a| a == flag) {
        None => Ok(default),
        Some(i) => args
            .get(i + 1)
            .ok_or_else(|| format!("missing_value:{flag}"))?
            .parse()
            .map_err(|_| format!("invalid_value:{flag}")),
    }
}

fn run() -> Result<String, String> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|a| a == "--descriptor" || a == "--help") {
        return Ok(DESCRIPTOR.to_owned());
    }
    let (width, height, bytes) = if args.iter().any(|a| a == "--synthetic") {
        let mut bytes = Vec::new();
        for _ in 0..64 {
            bytes.extend_from_slice(&2048u16.to_le_bytes());
        }
        (8u32, 8u32, bytes)
    } else {
        let path: String = parse(&args, "--plane", String::new())?;
        if path.is_empty() {
            return Err("missing_input:--synthetic|--plane|--descriptor".into());
        }
        (
            parse(&args, "--width", 0u32)?,
            parse(&args, "--height", 0u32)?,
            fs::read(&path).map_err(|_| "input_unavailable".to_string())?,
        )
    };
    let pattern = match parse(&args, "--pattern", "RGGB".to_owned())?.as_str() {
        "RGGB" => Bayer::Rggb,
        "BGGR" => Bayer::Bggr,
        "GRBG" => Bayer::Grbg,
        "GBRG" => Bayer::Gbrg,
        _ => return Err("unsupported_pattern".into()),
    };
    let black: u16 = parse(&args, "--black", 0)?;
    let white: u16 = parse(&args, "--white", 4095)?;
    let exposure: f64 = parse(&args, "--exposure", 0.0)?;
    let plane = CfaPlane::new(PlaneInit {
        width,
        height,
        stride_bytes: width * 2,
        sensor_origin: (0, 0),
        cfa: Cfa {
            pattern: CfaPattern::Bayer(pattern),
            phase_x: 0,
            phase_y: 0,
        },
        black: [black; 3],
        white: [white; 3],
        source_revision: 1,
        bytes,
    })
    .map_err(|e| e.code().to_owned())?;
    let profile = ProfileBinding {
        profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001")
            .map_err(|_| "invalid_profile".to_string())?,
        sha256: [0; 32],
    };
    let mut recipe = DevelopRecipe::native(
        [1.0; 3],
        [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        profile,
    );
    recipe.exposure_stops = exposure;
    let cancel = CancellationToken::default();
    if args.iter().any(|a| a == "--cancel") {
        cancel.cancel();
    }
    let mut tiles = 0u32;
    let receipt = develop(
        &DevelopRequest {
            plane: &plane,
            recipe: &recipe,
            recipe_revision: 1,
            expected_source_revision: 1,
            grid: Grid {
                origin_x: 0,
                origin_y: 0,
                tile_width: 64,
                tile_height: 64,
            },
            output_origin: (0, 0),
            execution_hint: ExecutionHint::Auto,
        },
        &cancel,
        &mut |_tile| {
            tiles += 1;
            Ok(())
        },
    )
    .map_err(|e| e.code().to_owned())?;
    Ok(format!(
        "developed tiles={} pixels={} route={} token={} limits={}",
        tiles, receipt.pixels, receipt.route, receipt.math_token, receipt.limits
    ))
}

fn main() {
    match run() {
        Ok(line) => println!("{line}"),
        Err(code) => {
            eprintln!("rejected:{code}; consult --descriptor and correct only the caller input");
            process::exit(2)
        }
    }
}
