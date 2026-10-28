use hsk_studio_accord::CancellationToken;
use hsk_studio_reel::{
    audio::aac::{asc_from_esds, parse_audio_specific_config},
    build_index, Limits,
};
use std::io::Cursor;

fn descriptor(tag: u8, payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() < 128);
    let mut value = vec![tag, payload.len() as u8];
    value.extend_from_slice(payload);
    value
}

fn esds(flags: u8, oti: u8, asc: Option<&[u8]>) -> Vec<u8> {
    let mut decoder = vec![oti, 0x15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    if let Some(asc) = asc {
        decoder.extend(descriptor(5, asc));
    }
    let mut es = vec![0, 1, flags];
    if flags & 0x80 != 0 {
        es.extend_from_slice(&[0, 2]);
    }
    if flags & 0x40 != 0 {
        es.extend_from_slice(&[3, b'u', b'r', b'l']);
    }
    if flags & 0x20 != 0 {
        es.extend_from_slice(&[0, 3]);
    }
    es.extend(descriptor(4, &decoder));
    es.extend(descriptor(6, &[2]));
    let mut value = vec![0; 4];
    value.extend(descriptor(3, &es));
    value
}

#[test]
fn public_esds_preserves_aac_optional_fields_and_no_dsi() {
    for flags in [0, 0x80, 0x40, 0x20, 0xe0] {
        let (oti, asc) = asc_from_esds(&esds(flags, 0x40, Some(&[0x12, 0x10]))).unwrap();
        assert_eq!((oti, asc.as_slice()), (0x40, &[0x12, 0x10][..]));
        let config = parse_audio_specific_config(&asc).unwrap();
        assert_eq!(
            (config.object_type, config.sample_rate, config.channels),
            (2, 44_100, 2)
        );
        assert_eq!(
            asc_from_esds(&esds(flags, 0x6b, None)).unwrap(),
            (0x6b, vec![])
        );
    }
    let fixture = include_bytes!("fixtures/audio/audio-tracks.mp4");
    let index = build_index(
        &mut Cursor::new(fixture),
        fixture.len() as u64,
        &Limits::default(),
        &CancellationToken::default(),
    )
    .unwrap();
    let aac = index
        .tracks
        .iter()
        .find(|track| track.codec == *b"mp4a")
        .unwrap();
    let (oti, asc) = asc_from_esds(aac.codec_config.strip_prefix(b"esds").unwrap()).unwrap();
    let config = parse_audio_specific_config(&asc).unwrap();
    assert_eq!(
        (oti, config.object_type, config.sample_rate, config.channels),
        (0x40, 2, 44_100, 2)
    );
}

#[test]
fn public_esds_rejects_nested_boundary_and_unterminated_lengths() {
    let valid = esds(0, 0x40, Some(&[0x12, 0x10]));
    let mut short_es = valid.clone();
    short_es[5] = 3;
    assert!(
        asc_from_esds(&short_es).is_err(),
        "ES children must fit their declared parent"
    );
    let mut short_decoder = valid.clone();
    short_decoder[10] = 16;
    assert!(
        asc_from_esds(&short_decoder).is_err(),
        "DSI cannot escape DecoderConfigDescriptor"
    );
    let mut long_es = valid.clone();
    long_es[5] = 127;
    assert!(
        asc_from_esds(&long_es).is_err(),
        "ES payload must be present"
    );
    let mut long_decoder = valid.clone();
    long_decoder[10] = 127;
    assert!(
        asc_from_esds(&long_decoder).is_err(),
        "decoder payload must fit ES"
    );
    let mut long_dsi = valid.clone();
    long_dsi[25] = 127;
    assert!(
        asc_from_esds(&long_dsi).is_err(),
        "DSI payload must fit decoder"
    );
    let mut padded = valid.clone();
    padded.splice(5..6, [0x80, 0x80, 0x80, valid[5]]);
    assert_eq!(asc_from_esds(&padded).unwrap(), (0x40, vec![0x12, 0x10]));
    let mut unterminated = valid.clone();
    unterminated.splice(5..6, [0x80, 0x80, 0x80, 0x80]);
    assert!(
        asc_from_esds(&unterminated).is_err(),
        "four continuation bytes do not terminate a length"
    );
    for length in 0..valid.len() - 3 {
        assert!(
            asc_from_esds(&valid[..length]).is_err(),
            "truncated input at {length}"
        );
    }
}
