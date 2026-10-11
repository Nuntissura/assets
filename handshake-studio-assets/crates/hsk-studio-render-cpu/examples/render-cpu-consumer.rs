//! Render-cpu consumer: `--descriptor` prints the module descriptor; `--selftest` renders the
//! hand-computed oracle (backdrop 0.2/0.4/0.6, red source at opacity 0.5) and fails on mismatch.
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::Rect;
use hsk_studio_render_cpu::{
    BlendMode, BlendSpace, DESCRIPTOR, NoSources, Op, PixelSink, RenderOptions, RenderPlan,
    Renderer, SinkError,
};
use std::env;

struct OnePixel {
    colour: Vec<u8>,
    alpha: Vec<u8>,
}
impl PixelSink for OnePixel {
    fn accept(&mut self, _rect: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        self.colour.extend_from_slice(colour);
        self.alpha.extend_from_slice(alpha);
        Ok(())
    }
}

fn f32s(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

fn selftest() -> Result<(), String> {
    let pixel = Rect {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
    };
    let fill = |rgba: [f32; 4], blend, opacity| Op::Fill {
        rect: pixel,
        rgba,
        blend,
        opacity,
    };
    for (mode, expect) in [
        (BlendMode::Normal, [0.6_f32, 0.2, 0.3]),
        (BlendMode::Multiply, [0.2, 0.2, 0.3]),
    ] {
        let plan = RenderPlan::new(
            1,
            pixel,
            [0; 32],
            BlendSpace::LinearLight,
            vec![
                fill([0.2, 0.4, 0.6, 1.0], BlendMode::Normal, 1.0),
                fill([1.0, 0.0, 0.0, 1.0], mode, 0.5),
            ],
        );
        let renderer = Renderer::new(RenderOptions::default()).map_err(|e| e.to_string())?;
        let mut sink = OnePixel {
            colour: vec![],
            alpha: vec![],
        };
        let receipt = renderer
            .render(&plan, 1, &NoSources, &CancellationToken::default(), &mut sink)
            .map_err(|e| e.to_string())?;
        let (c, a) = (f32s(&sink.colour), f32s(&sink.alpha));
        let ok = c.iter().zip(expect).all(|(got, want)| (got - want).abs() < 1e-6)
            && (a[0] - 1.0).abs() < 1e-6;
        println!(
            "{} colour={c:?} alpha={a:?} chunks={} ops={}",
            mode.name(),
            receipt.chunks,
            receipt.ops_executed
        );
        if !ok {
            return Err(format!("{} oracle mismatch", mode.name()));
        }
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["--descriptor"] | ["--help"] => println!("{DESCRIPTOR}"),
        ["--selftest"] => {
            if let Err(error) = selftest() {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        _ => {
            eprintln!("usage: render-cpu-consumer --descriptor | --selftest");
            std::process::exit(2);
        }
    }
}
