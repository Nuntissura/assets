//! Feature `document`, CPU only (no Vulkan device): the compose hook (SMB-VK-UI-PASS-1). One compose call hands every
//! chrome target the frame's ordered lists with its clear colour, scale, text and resources; the CPU painter's target
//! gives the frame's own canvas byte for byte, and a target's refusal is typed and leaves the shown frame alone.
#![cfg(feature = "document")]
mod common;

use common::{Scratch, fixture};
use hsk_studio_native_host::HostError;
use hsk_studio_native_host::gui::{Chrome, ChromeTarget, CpuChrome, GuiFrame, row_id};
use hsk_studio_vulkan::Refusal;
use kumihimo_ui::geom::{Rgba8, Scale};
use kumihimo_ui::paint::{Canvas, paint};
use kumihimo_ui::tokens::{ColorToken, Theme};

/// A target that records what compose hands it and paints it with the CPU painter itself.
#[derive(Default)]
struct Recorder {
    extent: (u32, u32),
    clear: Option<Rgba8>,
    scale: Option<Scale>,
    lens: Vec<usize>,
    canvas: Option<Canvas>,
    draws: u32,
}
impl ChromeTarget for Recorder {
    fn draw(&mut self, c: Chrome<'_>) -> Result<(), HostError> {
        let Chrome { extent, clear, scale, lists, text, res } = c;
        let mut canvas = Canvas::new(extent.0, extent.1, clear).unwrap();
        for l in lists {
            paint(&mut canvas, l, scale, text, res);
        }
        (self.extent, self.clear, self.scale, self.lens, self.canvas) = (extent, Some(clear), Some(scale), lists.iter().map(|l| l.len()).collect(), Some(canvas));
        self.draws += 1;
        Ok(())
    }
}

/// Compose hands the frame's lists in paint order (Viewport, document, dock, status), the frame clear and the scale;
/// painted with the CPU painter they give the frame's canvas byte for byte, as does [`CpuChrome`], at four scales and
/// after a command.
#[test]
fn compose_hands_every_target_the_cpu_frame_in_paint_order() {
    let s = Scratch::new("compose");
    let doc = fixture(&s.0);
    for milli in [1000u32, 1250, 1500, 2000] {
        let mut f = GuiFrame::open(&doc, 800, 480, Some(milli)).unwrap();
        f.frame().unwrap();
        for round in 0..2 {
            if round == 1 {
                let green = f.rows().iter().find(|r| r.name == "Green").map(|r| row_id(&r.layer_id)).unwrap();
                f.invoke(&green).unwrap();
                assert!(f.frame().unwrap());
            }
            let shown = f.rgba().unwrap().to_vec();
            let mut rec = Recorder::default();
            f.compose(&mut rec).expect("compose");
            assert_eq!(rec.draws, 1);
            assert_eq!(rec.extent, (800, 480));
            assert_eq!(rec.clear, Some(ColorToken::SurfaceFrame.resolve(Theme::Dark)));
            assert_eq!(rec.scale, Some(f.scale()));
            let [vp, dock, bar] = f.panels();
            assert_eq!(rec.lens.len(), 4, "Viewport, document, dock, status: {:?}", rec.lens);
            assert_eq!((rec.lens[0], rec.lens[2], rec.lens[3]), (vp.display_list().len(), dock.display_list().len(), bar.display_list().len()));
            assert!(rec.lens[1] > 0, "the document list");
            assert!(rec.canvas.as_ref().unwrap().rgba() == &shown[..], "{milli} round {round}: the recorder paints the frame's bytes");
            let mut cpu = CpuChrome::default();
            f.compose(&mut cpu).expect("compose cpu");
            assert!(cpu.canvas.as_ref().unwrap().rgba() == &shown[..], "{milli} round {round}: CpuChrome paints the frame's bytes");
            assert!(f.rgba().unwrap() == &shown[..], "compose leaves the shown frame");
        }
    }
}

/// A target that loses the device: compose returns that refusal typed (exit 3), nothing retries, and the frame the
/// CPU path shows is unchanged.
#[test]
fn compose_target_refusal_is_typed_and_leaves_the_frame() {
    struct Lost(u32);
    impl ChromeTarget for Lost {
        fn draw(&mut self, _: Chrome<'_>) -> Result<(), HostError> {
            self.0 += 1;
            Err(HostError::Vulkan(Refusal::DeviceLost))
        }
    }
    let s = Scratch::new("compose-lost");
    let mut f = GuiFrame::open(&fixture(&s.0), 800, 480, Some(1000)).unwrap();
    f.frame().unwrap();
    let shown = f.rgba().unwrap().to_vec();
    let mut lost = Lost(0);
    let err = f.compose(&mut lost).unwrap_err();
    assert_eq!(err, HostError::Vulkan(Refusal::DeviceLost));
    assert_eq!(err.exit_code(), 3);
    assert_eq!(lost.0, 1, "one draw, no retry");
    assert!(f.rgba().unwrap() == &shown[..]);
    assert!(!f.frame().unwrap(), "the refused compose leaves the frame idle");
}
