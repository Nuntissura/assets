//! Lane SMB-PS-INFO-HISTOGRAM-1: pigment tonal diagnostics on synthetic planes (exact, hand-counted).
//! NOT_PROVEN: equality with Photoshop's Histogram / Info panels (bin cache levels, luminosity
//! weights, sample edge handling; no D5 case for PS-30).

use hsk_studio_pigment::diagnostics::{
    self as d, ColourModel, HistogramChannel as C, MAX_DIAG_EDGE, MAX_DIAG_PIXELS, Rgba8Plane, SampleSize, coverage_bounds, histogram, readout, sample,
};
use hsk_studio_pigment::selection::CoveragePlane;
use hsk_studio_pigment::{Error, Rect};

fn plane(x: i64, y: i64, w: u32, h: u32, data: &[u8]) -> Rgba8Plane<'_> {
    Rgba8Plane { x, y, width: w, height: h, data }
}

#[test]
fn histogram_bins_and_clip_counts_are_exact() {
    // 4 x 2 plane: black, white, (10, 20, 30), (255, 0, 128), (0, 0, 0, alpha 0) and three greys 100.
    let px: Vec<[u8; 4]> = vec![[0, 0, 0, 255], [255, 255, 255, 255], [10, 20, 30, 255], [255, 0, 128, 255], [7, 7, 7, 0], [100, 100, 100, 255], [100, 100, 100, 128], [100, 100, 100, 255]];
    let data: Vec<u8> = px.concat();
    let p = plane(0, 0, 4, 2, &data);
    let h = histogram(&p, None, 256).unwrap();
    assert_eq!((h.bin_count, h.pixels, h.transparent), (256, 8, 1));
    let red = h.channel(C::Red).unwrap();
    assert_eq!(red.count, 7);
    assert_eq!((red.bins[0], red.bins[255], red.bins[10], red.bins[100]), (1, 2, 1, 3));
    assert_eq!((red.clip_low, red.clip_high), (1, 2));
    assert_eq!(red.bins.iter().sum::<u64>(), 7);
    let mean = (0.0 + 255.0 + 10.0 + 255.0 + 300.0) / 7.0;
    assert!((red.mean - mean).abs() < 1e-12, "{}", red.mean);
    assert_eq!(red.median, 100);
    let green = h.channel(C::Green).unwrap();
    assert_eq!((green.clip_low, green.clip_high), (2, 1));
    let blue = h.channel(C::Blue).unwrap();
    assert_eq!((blue.bins[128], blue.bins[30]), (1, 1));
    // Alpha bins every pixel, transparent ones included.
    let alpha = h.channel(C::Alpha).unwrap();
    assert_eq!((alpha.count, alpha.bins[255], alpha.bins[128], alpha.bins[0]), (8, 6, 1, 1));
    // Luminosity: round((299 R + 587 G + 114 B) / 1000).
    let lum = h.channel(C::Luminosity).unwrap();
    assert_eq!(d::luminosity(10, 20, 30), 18); // 2990 + 11740 + 3420 = 18150 -> 18
    assert_eq!(d::luminosity(255, 0, 128), 91); // 76245 + 0 + 14592 = 90837 -> 91
    assert_eq!((lum.bins[0], lum.bins[255], lum.bins[18], lum.bins[91], lum.bins[100]), (1, 1, 1, 1, 3));
    // Four bins: value v in bin v * 4 / 256.
    let h4 = histogram(&p, None, 4).unwrap();
    let red4 = &h4.channel(C::Red).unwrap().bins;
    assert_eq!(red4, &vec![2, 3, 0, 2]);
    // A coverage region counts only covered pixels (document coordinates; plane at (10, 5)).
    let moved = plane(10, 5, 4, 2, &data);
    let cov = CoveragePlane { bounds: Rect { x: 11, y: 5, width: 2, height: 1 }, data: vec![u16::MAX, 1] };
    let hs = histogram(&moved, Some(&cov), 256).unwrap();
    assert_eq!((hs.pixels, hs.transparent), (2, 0));
    let r = hs.channel(C::Red).unwrap();
    assert_eq!((r.bins[255], r.bins[10], r.count), (1, 1, 2));
}

#[test]
fn sample_sizes_average_their_neighbourhoods() {
    // 101 x 101 plane, value = column index (R), row index (G), 7 (B), opaque. Centre (50, 50).
    let n = 101u32;
    let mut data = Vec::new();
    for y in 0..n {
        for x in 0..n {
            data.extend([x as u8, y as u8, 7, 255]);
        }
    }
    let p = plane(0, 0, n, n, &data);
    for size in SampleSize::ALL {
        let s = sample(&p, 50, 50, size).unwrap().unwrap();
        let e = size.edge();
        assert_eq!(s.pixels, e * e, "{size:?}");
        // A symmetric square around 50 averages to 50 on both axes.
        assert!((s.rgb[0] - 50.0).abs() < 1e-9 && (s.rgb[1] - 50.0).abs() < 1e-9 && (s.rgb[2] - 7.0).abs() < 1e-9, "{size:?} {s:?}");
        assert_eq!(s.rgba8, [50, 50, 7, 255]);
    }
    // Off-centre 3x3 at (10, 20): columns 9..=11, rows 19..=21.
    let s = sample(&p, 10, 20, SampleSize::S3).unwrap().unwrap();
    assert_eq!((s.rgb[0], s.rgb[1]), (10.0, 20.0));
    // Codes are the spec's closed enumeration.
    let codes: Vec<&str> = SampleSize::ALL.iter().map(|s| s.code()).collect();
    assert_eq!(codes, ["point", "3x3", "5x5", "11x11", "31x31", "51x51", "101x101"]);
    for s in SampleSize::ALL {
        assert_eq!(SampleSize::from_code(s.code()), Some(s));
    }
    // Colour is alpha-weighted: a transparent neighbour does not pull the colour.
    let two = [200u8, 0, 0, 255, 0, 0, 255, 0];
    let s = sample(&plane(0, 0, 2, 1, &two), 0, 0, SampleSize::S3).unwrap().unwrap();
    assert_eq!((s.rgb, s.alpha, s.pixels), ([200.0, 0.0, 0.0], 127.5, 2));
    assert_eq!(s.rgba8, [200, 0, 0, 128]);
}

#[test]
fn edge_samples_average_in_bounds_pixels() {
    // 4 x 4, R = 10 * x + y.
    let mut data = Vec::new();
    for y in 0..4u8 {
        for x in 0..4u8 {
            data.extend([10 * x + y, 0, 0, 255]);
        }
    }
    let p = plane(100, 200, 4, 4, &data);
    // Corner (100, 200): 5x5 keeps columns 0..=2 and rows 0..=2 of the plane: 9 pixels.
    let s = sample(&p, 100, 200, SampleSize::S5).unwrap().unwrap();
    assert_eq!(s.pixels, 9);
    let want = (0..3).flat_map(|x| (0..3).map(move |y| f64::from(10 * x + y))).sum::<f64>() / 9.0;
    assert!((s.rgb[0] - want).abs() < 1e-9, "{} vs {want}", s.rgb[0]);
    // 101x101 at any pixel averages the whole 4 x 4 plane.
    let s = sample(&p, 102, 201, SampleSize::S101).unwrap().unwrap();
    assert_eq!(s.pixels, 16);
    // A centre outside the plane reads nothing.
    assert_eq!(sample(&p, 99, 200, SampleSize::S101).unwrap(), None);
    assert_eq!(sample(&p, 104, 200, SampleSize::Point).unwrap(), None);
    assert_eq!(p.pixel(103, 203), Some([33, 0, 0, 255]));
    assert_eq!(p.pixel(i64::MIN, 0), None);
    // Selection bounds: tight bounds of covered pixels in document pixels.
    let cov = CoveragePlane { bounds: Rect { x: -3, y: 4, width: 4, height: 3 }, data: vec![0, 0, 0, 0, 0, 9, 0, 0, 0, 0, 0, 1] };
    assert_eq!(coverage_bounds(&cov).unwrap(), Some((Rect { x: -2, y: 5, width: 3, height: 2 }, 2)));
    assert_eq!(coverage_bounds(&CoveragePlane { bounds: Rect { x: 0, y: 0, width: 2, height: 1 }, data: vec![0, 0] }).unwrap(), None);
    // Readouts.
    assert_eq!(readout([255.0, 0.0, 0.0], ColourModel::Rgb).unwrap(), vec![255.0, 0.0, 0.0]);
    assert_eq!(readout([255.0, 0.0, 0.0], ColourModel::Hsb).unwrap(), vec![0.0, 100.0, 100.0]);
    assert_eq!(readout([0.0, 0.0, 0.0], ColourModel::Cmyk).unwrap(), vec![0.0, 0.0, 0.0, 100.0]);
    assert_eq!(readout([255.0, 255.0, 0.0], ColourModel::Cmyk).unwrap(), vec![0.0, 0.0, 100.0, 0.0]);
    let lab = readout([255.0, 255.0, 255.0], ColourModel::Lab).unwrap();
    assert!((lab[0] - 100.0).abs() < 1e-3 && lab[1].abs() < 1e-2 && lab[2].abs() < 1e-2, "{lab:?}");
    for m in ColourModel::ALL {
        assert_eq!(ColourModel::from_code(m.code()), Some(m));
        assert_eq!(readout([1.0, 2.0, 3.0], m).unwrap().len(), m.components().len());
    }
}

#[test]
fn hostile_planes_are_refused_bounded() {
    let data = [0u8; 16];
    assert_eq!(plane(0, 0, 0, 4, &data).validate(), Err(Error::InvalidInput));
    assert_eq!(plane(0, 0, 2, 2, &data[..15]).validate(), Err(Error::MalformedStride));
    assert_eq!(plane(0, 0, 2, 3, &data).validate(), Err(Error::MalformedStride));
    // dims_u32_max: refused before any allocation or read.
    assert_eq!(plane(0, 0, u32::MAX, u32::MAX, &data).validate(), Err(Error::BudgetExceeded));
    assert_eq!(plane(0, 0, MAX_DIAG_EDGE + 1, 1, &data).validate(), Err(Error::BudgetExceeded));
    // aggregate_over_budget: both edges legal, the pixel count over MAX_DIAG_PIXELS.
    let edge = MAX_DIAG_EDGE;
    assert!(u64::from(edge) * u64::from(edge) > MAX_DIAG_PIXELS);
    assert_eq!(plane(0, 0, edge, edge, &data).validate(), Err(Error::BudgetExceeded));
    assert_eq!(histogram(&plane(0, 0, edge, edge, &data), None, 256).unwrap_err(), Error::BudgetExceeded);
    assert_eq!(sample(&plane(0, 0, edge, edge, &data), 0, 0, SampleSize::S101).unwrap_err(), Error::BudgetExceeded);
    let p = plane(0, 0, 2, 2, &data);
    assert_eq!(histogram(&p, None, 0).unwrap_err(), Error::InvalidInput);
    assert_eq!(histogram(&p, None, 257).unwrap_err(), Error::InvalidInput);
    // A coverage plane whose data does not match its bounds.
    let bad = CoveragePlane { bounds: Rect { x: 0, y: 0, width: 2, height: 2 }, data: vec![1; 3] };
    assert_eq!(histogram(&p, Some(&bad), 256).unwrap_err(), Error::MalformedStride);
    assert_eq!(coverage_bounds(&bad).unwrap_err(), Error::MalformedStride);
    // Extreme document origins do not overflow: off-plane reads are None.
    let far = plane(i64::MAX - 1, i64::MAX - 1, 2, 2, &data);
    assert_eq!(sample(&far, i64::MAX, i64::MAX, SampleSize::S101).unwrap().map(|s| s.pixels), Some(4));
    // Readouts refuse non-finite and out-of-range components.
    assert_eq!(readout([f64::NAN, 0.0, 0.0], ColourModel::Lab).unwrap_err(), Error::InvalidInput);
    assert_eq!(readout([256.0, 0.0, 0.0], ColourModel::Rgb).unwrap_err(), Error::InvalidInput);
}
