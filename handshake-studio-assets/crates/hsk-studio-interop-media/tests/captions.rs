//! Lane SMB-PREMIERE-COLOR-TITLES slice 4: SubRip and WebVTT caption import / export against hand
//! values, round trips, and hostile inputs (malformed timing, decoys, caption bombs).
use hsk_studio_folio::{StudioCaptionAlign, StudioCaptionPlacement, StudioCaptionSegment, StudioCaptionStyle};
use hsk_studio_interop_media::captions::*;

const MS: u64 = TICKS_PER_MS;

#[test]
fn srt_and_vtt_import_and_export_hand_values() {
    let lim = CaptionLimits::default();
    // CRLF, a BOM, a whole-cue italic span, a partial bold span (dropped tag, kept text), an
    // `{\an8}` top placement and a font colour.
    let srt = "\u{feff}1\r\n00:00:01,500 --> 00:00:03,000\r\n<i>Hello there</i>\r\n\r\n\
               2\r\n00:00:02,000 --> 01:02:03,004\r\n{\\an8}Two <B>lines</B>\r\nsecond\r\n\r\n\
               3\n00:00:04,000 --> 00:00:05,000\n<font color=\"#ff8000\">Warm</font>\n";
    let s = parse_srt(srt.as_bytes(), &lim).unwrap();
    assert_eq!(s.len(), 3);
    assert_eq!((s[0].start, s[0].end, s[0].text.as_str()), (1500 * MS, 3000 * MS, "Hello there"));
    assert!(s[0].style.italic && !s[0].style.bold);
    assert_eq!(s[1].end, ((3600 + 2 * 60 + 3) * 1000 + 4) * MS);
    assert_eq!(s[1].text, "Two lines\nsecond");
    assert_eq!(s[1].style.placement, StudioCaptionPlacement::Top);
    assert_eq!(s[2].style.colour, Some([1.0, 128.0 / 255.0, 0.0, 1.0]));

    // WebVTT: header text, NOTE / STYLE blocks (a decoy arrow inside the NOTE), cue ids, short
    // MM:SS.mmm times, settings, voice / class tags, entities decoded after tag removal.
    let vtt = "WEBVTT - demo\n\nNOTE 00:00:09.000 --> 00:00:10.000 not a cue\n\nSTYLE\n::cue { color: red }\n\n\
               intro\n00:01.000 --> 00:02.250 line:0 align:left\n<v Ilka><c.loud>Fish &amp; chips</c></v> &lt;3\n\n\
               00:00:03.000 --> 00:00:04.000\n<b>Bold</b>\n";
    let v = parse_vtt(vtt.as_bytes(), &lim).unwrap();
    assert_eq!(v.len(), 2);
    assert_eq!((v[0].start, v[0].end, v[0].text.as_str()), (1000 * MS, 2250 * MS, "Fish & chips <3"));
    assert_eq!((v[0].style.placement, v[0].style.align), (StudioCaptionPlacement::Top, StudioCaptionAlign::Left));
    assert!(v[1].style.bold);

    // Export: exact millisecond text, escaping, styles; and each round-trips to the same segments.
    let segs = vec![
        StudioCaptionSegment { start: 1500 * MS, end: 3000 * MS, text: "a < b & c".into(), style: StudioCaptionStyle { italic: true, ..Default::default() } },
        StudioCaptionSegment {
            start: 4000 * MS,
            end: 5250 * MS,
            text: "top\nright".into(),
            style: StudioCaptionStyle { placement: StudioCaptionPlacement::Top, align: StudioCaptionAlign::Right, ..Default::default() },
        },
    ];
    let w = write_vtt(&segs).unwrap();
    assert!(w.starts_with("WEBVTT\n\n00:00:01.500 --> 00:00:03.000\n<i>a &lt; b &amp; c</i>\n\n"), "{w}");
    assert_eq!(parse_vtt(w.as_bytes(), &lim).unwrap(), segs);
    let w = write_srt(&segs).unwrap();
    assert!(w.starts_with("1\n00:00:01,500 --> 00:00:03,000\n<i>a < b & c</i>\n\n2\n"), "{w}");
    let back = parse_srt(w.as_bytes(), &lim).unwrap();
    assert_eq!(back[0], segs[0]);
    assert_eq!((back[1].text.as_str(), back[1].style.placement), ("top\nright", StudioCaptionPlacement::Top));
    // A time that is not a whole millisecond is refused, never rounded.
    let odd = [StudioCaptionSegment { start: 1, end: 2 * MS, text: "x".into(), style: StudioCaptionStyle::default() }];
    assert_eq!(write_srt(&odd).unwrap_err().code, CAPTION_TIMING);
}

#[test]
fn hostile_caption_files_are_typed_refusals() {
    let lim = CaptionLimits::default();
    let srt = |t: &str| parse_srt(t.as_bytes(), &lim).map_err(|e| e.code);
    let vtt = |t: &str| parse_vtt(t.as_bytes(), &lim).map_err(|e| e.code);
    // Malformed timing: minutes / seconds >= 60, two-digit milliseconds, missing arrow, end before
    // start, a dot separator in SRT, absurd hours, a non-numeric counter.
    assert_eq!(srt("1\n00:61:00,000 --> 00:62:00,000\nx\n"), Err(CAPTION_TIMESTAMP));
    assert_eq!(srt("1\n00:00:00,00 --> 00:00:01,000\nx\n"), Err(CAPTION_TIMESTAMP));
    assert_eq!(srt("1\n00:00:00,000 00:00:01,000\nx\n"), Err(CAPTION_SYNTAX));
    assert_eq!(srt("1\n00:00:02,000 --> 00:00:01,000\nx\n"), Err(CAPTION_TIMING));
    assert_eq!(srt("1\n00:00:00.000 --> 00:00:01.000\nx\n"), Err(CAPTION_TIMESTAMP));
    assert_eq!(srt("1\n99999999:00:00,000 --> 99999999:00:01,000\nx\n"), Err(CAPTION_TIMESTAMP));
    assert_eq!(srt("one\n00:00:00,000 --> 00:00:01,000\nx\n"), Err(CAPTION_SYNTAX));
    // WebVTT header is exact (case and prefix decoys refused); a NUL inside a cue is refused.
    assert_eq!(vtt("webvtt\n\n00:01.000 --> 00:02.000\nx\n"), Err(CAPTION_HEADER));
    assert_eq!(vtt("WEBVTTX\n\n00:01.000 --> 00:02.000\nx\n"), Err(CAPTION_HEADER));
    assert_eq!(vtt("WEBVTT\n\n00:01.000 --> 00:02.000\na\u{0}b\n"), Err(CAPTION_SYNTAX));
    assert_eq!(parse_srt(&[0xff, 0xfe, 0x00], &lim).unwrap_err().code, CAPTION_ENCODING);
    // Unterminated tag is text; a bare ampersand stays; an unknown entity stays literal.
    let v = parse_vtt(b"WEBVTT\n\n00:01.000 --> 00:02.000\na <b & c &bogus; &#x41;\n", &lim).unwrap();
    assert_eq!(v[0].text, "a <b & c &bogus; A");
    // Caption bomb: the cue count and cue size bounds hold before storage; the byte bound before
    // any parse.
    let small = CaptionLimits { max_cues: 2, max_cue_text_bytes: 8, max_bytes: 4096 };
    let bomb: String = (0..3).map(|i| format!("{}\n00:00:0{i},000 --> 00:00:0{i},500\nx\n\n", i + 1)).collect();
    assert_eq!(parse_srt(bomb.as_bytes(), &small).unwrap_err().code, CAPTION_TOO_MANY_CUES);
    assert_eq!(parse_srt(b"1\n00:00:00,000 --> 00:00:01,000\n0123456789\n", &small).unwrap_err().code, CAPTION_CUE_TOO_LONG);
    let tiny = CaptionLimits { max_bytes: 10, ..lim };
    assert_eq!(parse_vtt(b"WEBVTT\n\n00:01.000 --> 00:02.000\nx\n", &tiny).unwrap_err().code, CAPTION_TOO_LARGE);
    // An arrow inside cue text written to SRT cannot become a timing line.
    let seg = [StudioCaptionSegment { start: 0, end: MS, text: "a --> b".into(), style: StudioCaptionStyle::default() }];
    let back = parse_srt(write_srt(&seg).unwrap().as_bytes(), &lim).unwrap();
    assert_eq!((back.len(), back[0].text.as_str()), (1, "a -- > b"));
}
