//! Strict allocation-free matching of borrowed canonical Path bytes (CON-032).
//! Geometry support classification belongs to the provider after this complete check.
use crate::path_contract::{Anchor, Error, Limits, Path, Point, WorkMeter};
use hsk_studio_accord::Unit;
use sha2::{Digest, Sha256};

pub(crate) fn verify_encoded_path(
    bytes: &[u8],
    expected: &Path<'_>,
    limits: &Limits,
    work: &mut WorkMeter<'_>,
) -> Result<[u8; 32], Error> {
    work.check()?;
    let length = u64::try_from(bytes.len()).map_err(|_| Error::Overflow)?;
    if length > limits.input_bytes || limits.recursion_depth < 5 {
        return Err(Error::BudgetExceeded);
    }
    if length > u64::MAX / 8 {
        return Err(Error::Overflow);
    }
    // Validate UTF-8 without copying; charge its complete bounded pass first.
    for _ in bytes {
        work.step()?;
    }
    std::str::from_utf8(bytes).map_err(|_| Error::InvalidEncoding)?;
    work.check()?;
    let mut parser = Parser {
        bytes,
        at: 0,
        mismatch: false,
        limits,
        work,
    };
    parser.path(expected)?;
    parser.space()?;
    if parser.at != bytes.len() {
        return Err(Error::InvalidEncoding);
    }
    if parser.mismatch {
        return Err(Error::ValueMismatch);
    }
    let mut hash = Sha256::new();
    for chunk in bytes.chunks(256) {
        // Reserve work before each nonallocating, bounded provider call.
        for _ in chunk {
            parser.work.step()?;
        }
        hash.update(chunk);
        parser.work.check()?;
    }
    parser.work.step()?;
    let result: [u8; 32] = hash.finalize().into();
    parser.work.check()?;
    Ok(result)
}

struct Parser<'a, 'b, 'c> {
    bytes: &'a [u8],
    at: usize,
    mismatch: bool,
    limits: &'b Limits,
    work: &'b mut WorkMeter<'c>,
}
impl Parser<'_, '_, '_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }
    fn byte(&mut self) -> Result<u8, Error> {
        self.work.step()?;
        let byte = self.peek().ok_or(Error::InvalidEncoding)?;
        self.at = self.at.checked_add(1).ok_or(Error::Overflow)?;
        Ok(byte)
    }
    fn space(&mut self) -> Result<(), Error> {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.byte()?;
        }
        Ok(())
    }
    fn expect(&mut self, byte: u8) -> Result<(), Error> {
        self.space()?;
        if self.byte()? == byte {
            Ok(())
        } else {
            Err(Error::InvalidEncoding)
        }
    }
    fn hex4(&mut self) -> Result<u32, Error> {
        let mut value = 0;
        for _ in 0..4 {
            let digit = match self.byte()? {
                b @ b'0'..=b'9' => u32::from(b - b'0'),
                b @ b'a'..=b'f' => u32::from(b - b'a') + 10,
                b @ b'A'..=b'F' => u32::from(b - b'A') + 10,
                _ => return Err(Error::InvalidEncoding),
            };
            value = value * 16 + digit;
        }
        Ok(value)
    }
    /// Match decoded UTF-8 against at most eight borrowed alternatives in one pass.
    /// Still consumes/validates the entire string after every alternative mismatches.
    fn string(&mut self, choices: &[&str]) -> Result<Option<usize>, Error> {
        if choices.len() > 8 {
            return Err(Error::Overflow);
        }
        self.expect(b'"')?;
        let mut mask = (1_u16 << choices.len()) - 1;
        let mut position = 0_usize;
        loop {
            let b = self.byte()?;
            if b == b'"' {
                break;
            }
            if b < 32 {
                return Err(Error::InvalidEncoding);
            }
            if b == b'\\' {
                let c = match self.byte()? {
                    b'"' => '"',
                    b'\\' => '\\',
                    b'/' => '/',
                    b'b' => '\u{8}',
                    b'f' => '\u{c}',
                    b'n' => '\n',
                    b'r' => '\r',
                    b't' => '\t',
                    b'u' => {
                        let mut value = self.hex4()?;
                        if (0xd800..=0xdbff).contains(&value) {
                            if self.byte()? != b'\\' || self.byte()? != b'u' {
                                return Err(Error::InvalidEncoding);
                            }
                            let low = self.hex4()?;
                            if !(0xdc00..=0xdfff).contains(&low) {
                                return Err(Error::InvalidEncoding);
                            }
                            value = 0x10000 + ((value - 0xd800) << 10) + low - 0xdc00;
                        }
                        char::from_u32(value).ok_or(Error::InvalidEncoding)?
                    }
                    _ => return Err(Error::InvalidEncoding),
                };
                let mut utf8 = [0_u8; 4];
                for byte in c.encode_utf8(&mut utf8).bytes() {
                    self.match_byte(choices, &mut mask, &mut position, byte)?;
                }
            } else {
                self.match_byte(choices, &mut mask, &mut position, b)?;
            }
        }
        Ok(choices.iter().enumerate().find_map(|(i, choice)| {
            (mask & (1 << i) != 0 && choice.len() == position).then_some(i)
        }))
    }
    fn match_byte(
        &mut self,
        choices: &[&str],
        mask: &mut u16,
        position: &mut usize,
        byte: u8,
    ) -> Result<(), Error> {
        for (i, choice) in choices.iter().enumerate() {
            self.work.step()?;
            if choice.as_bytes().get(*position) != Some(&byte) {
                *mask &= !(1 << i);
            }
        }
        *position = position.checked_add(1).ok_or(Error::Overflow)?;
        Ok(())
    }
    fn equal_string(&mut self, expected: Option<&str>) -> Result<(), Error> {
        let result = match expected {
            Some(value) => self.string(&[value])?,
            None => self.string(&[])?,
        };
        if result != Some(0) {
            self.mismatch = true;
        }
        Ok(())
    }
    fn literal(&mut self, literal: &[u8]) -> Result<(), Error> {
        self.space()?;
        for &byte in literal {
            if self.byte()? != byte {
                return Err(Error::InvalidEncoding);
            }
        }
        Ok(())
    }
    fn boolean(&mut self) -> Result<bool, Error> {
        self.space()?;
        match self.peek() {
            Some(b't') => {
                self.literal(b"true")?;
                Ok(true)
            }
            Some(b'f') => {
                self.literal(b"false")?;
                Ok(false)
            }
            _ => Err(Error::InvalidEncoding),
        }
    }
    fn number(&mut self) -> Result<f64, Error> {
        self.space()?;
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.byte()?;
        }
        match self.byte()? {
            b'0' => {}
            b'1'..=b'9' => {
                while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                    self.byte()?;
                }
            }
            _ => return Err(Error::InvalidEncoding),
        }
        if self.peek() == Some(b'.') {
            self.byte()?;
            let before = self.at;
            while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                self.byte()?;
            }
            if self.at == before {
                return Err(Error::InvalidEncoding);
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.byte()?;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.byte()?;
            }
            let before = self.at;
            while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                self.byte()?;
            }
            if self.at == before {
                return Err(Error::InvalidEncoding);
            }
        }
        let token = &self.bytes[start..self.at];
        for _ in token {
            self.work.step()?;
        }
        // Pinned Rust core float parser uses fixed inline storage, not serde's boxed errors.
        let value = std::str::from_utf8(token)
            .map_err(|_| Error::InvalidEncoding)?
            .parse::<f64>()
            .map_err(|_| Error::InvalidEncoding)?;
        self.work.check()?;
        if !value.is_finite() {
            return Err(Error::InvalidGeometry);
        }
        Ok(value)
    }
    fn object(
        &mut self,
        keys: &[&str],
        mut field: impl FnMut(&mut Self, usize) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.expect(b'{')?;
        let mut seen = 0_u16;
        self.space()?;
        if self.peek() != Some(b'}') {
            loop {
                let key = self.string(keys)?.ok_or(Error::UnknownField)?;
                let bit = 1_u16 << key;
                if seen & bit != 0 {
                    return Err(Error::DuplicateField);
                }
                seen |= bit;
                self.expect(b':')?;
                field(self, key)?;
                self.space()?;
                if self.peek() == Some(b'}') {
                    break;
                }
                self.expect(b',')?;
            }
        }
        self.expect(b'}')?;
        if seen != (1_u16 << keys.len()) - 1 {
            return Err(Error::MissingField);
        }
        Ok(())
    }
    fn length(&mut self, expected: Option<hsk_studio_accord::Length>) -> Result<(), Error> {
        self.object(&["value", "unit"], |p, key| {
            if key == 0 {
                let value = p.number()?;
                if expected.is_none_or(|e| e.value() != value || e.unit() != Unit::Points) {
                    p.mismatch = true;
                }
            } else if p.string(&["pt"])? != Some(0) {
                p.mismatch = true;
            }
            Ok(())
        })
    }
    fn point(&mut self, expected: Option<Point>) -> Result<(), Error> {
        self.object(&["x", "y"], |p, key| {
            p.length(expected.map(|e| if key == 0 { e.x } else { e.y }))
        })
    }
    fn anchor(&mut self, expected: Option<&Anchor>) -> Result<(), Error> {
        self.object(
            &["position", "incoming", "outgoing", "handle_mirroring"],
            |p, key| match key {
                0 => p.point(expected.map(|e| e.position)),
                1 => p.point(expected.map(|e| e.incoming)),
                2 => p.point(expected.map(|e| e.outgoing)),
                _ => {
                    let choices = ["NONE", "ANGLE", "ANGLE_AND_LENGTH"];
                    let decoded = p.string(&choices)?.ok_or(Error::InvalidEncoding)?;
                    if expected.is_none_or(|e| e.handle_mirroring.wire() != choices[decoded]) {
                        p.mismatch = true;
                    }
                    Ok(())
                }
            },
        )
    }
    fn anchors(&mut self, expected: &[Anchor]) -> Result<(), Error> {
        self.expect(b'[')?;
        let mut count = 0_usize;
        self.space()?;
        if self.peek() != Some(b']') {
            loop {
                let next = count.checked_add(1).ok_or(Error::Overflow)?;
                if u64::try_from(next).map_err(|_| Error::Overflow)? > self.limits.anchors {
                    return Err(Error::BudgetExceeded);
                }
                self.anchor(expected.get(count))?;
                count = next;
                self.space()?;
                if self.peek() == Some(b']') {
                    break;
                }
                self.expect(b',')?;
            }
        }
        self.expect(b']')?;
        if count != expected.len() {
            self.mismatch = true;
        }
        Ok(())
    }
    fn path(&mut self, expected: &Path<'_>) -> Result<(), Error> {
        self.object(
            &[
                "schema_id",
                "path_id",
                "address",
                "closed",
                "anchors",
                "winding_rule",
                "fill_style_id",
            ],
            |p, key| {
                match key {
                    0 => {
                        p.equal_string(Some("hsk.studio.vector_path@1"))?;
                        if expected.schema_id != "hsk.studio.vector_path@1" {
                            p.mismatch = true;
                        }
                    }
                    1 => p.equal_string(Some(expected.path_id.as_str()))?,
                    2 => p.object(&["layer_id", "object_key", "property"], |p, key| {
                        p.equal_string(Some(match key {
                            0 => expected.address.layer_id.as_str(),
                            1 => expected.address.object_key,
                            _ => "geometry",
                        }))
                    })?,
                    3 => {
                        if p.boolean()? != expected.closed {
                            p.mismatch = true;
                        }
                    }
                    4 => p.anchors(expected.anchors)?,
                    5 => {
                        let choices = ["NONZERO", "EVENODD", "NONE"];
                        let decoded = p.string(&choices)?.ok_or(Error::InvalidEncoding)?;
                        if choices[decoded] != expected.winding_rule.wire() {
                            p.mismatch = true;
                        }
                    }
                    _ => {
                        p.space()?;
                        if p.peek() == Some(b'n') {
                            p.literal(b"null")?;
                            if expected.fill_style_id.is_some() {
                                p.mismatch = true;
                            }
                        } else {
                            p.equal_string(expected.fill_style_id.map(|s| s.as_str()))?;
                        }
                    }
                }
                Ok(())
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path_contract::{EpochPort, GeometryAddress, Mirroring, Winding};
    use hsk_studio_accord::{CancellationToken, DomainId};
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::{
            atomic::{AtomicU64, Ordering},
            mpsc,
        },
        time::Duration,
    };

    fn bounded_case(case: impl FnOnce() + Send + 'static) {
        let (send, receive) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let _ = send.send(catch_unwind(AssertUnwindSafe(case)));
        });
        match receive.recv_timeout(Duration::from_secs(30)) {
            Ok(Ok(())) => {}
            Ok(Err(panic)) => resume_unwind(panic),
            Err(error) => panic!("decoder case deadline/disconnection: {error}"),
        }
    }
    struct Epoch(AtomicU64);
    impl EpochPort for Epoch {
        fn current_epoch(&self) -> Result<u64, Error> {
            Ok(self.0.load(Ordering::Relaxed))
        }
    }
    fn limits() -> Limits {
        Limits {
            input_bytes: 65536,
            operands: 2,
            anchors: 8,
            segments: 8,
            sweep_events: 128,
            intersections: 32,
            output_vertices: 64,
            output_segments: 64,
            output_regions: 8,
            output_loops: 8,
            work_units: 100000,
            recursion_depth: 5,
            requested_allocation_bytes: 0,
        }
    }
    fn anchors() -> [Anchor; 4] {
        [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)].map(|(x, y)| Anchor {
            position: Point::pt(x, y).unwrap(),
            incoming: Point::pt(0.0, 0.0).unwrap(),
            outgoing: Point::pt(0.0, 0.0).unwrap(),
            handle_mirroring: Mirroring::None,
        })
    }
    fn fixture() -> String {
        let anchors = [(0,0),(10,0),(10,10),(0,10)].map(|(x,y)| format!(r#"{{"position":{{"x":{{"value":{x},"unit":"pt"}},"y":{{"value":{y},"unit":"pt"}}}},"incoming":{{"x":{{"value":0,"unit":"pt"}},"y":{{"value":0,"unit":"pt"}}}},"outgoing":{{"x":{{"value":0,"unit":"pt"}},"y":{{"value":0,"unit":"pt"}}}},"handle_mirroring":"NONE"}}"#)).join(",");
        format!(
            r#"{{"fill_style_id":null,"winding_rule":"NONE","anchors":[{anchors}],"closed":true,"address":{{"property":"geometry","object_key":"tile_A","layer_id":"SLYR-01900000-0000-7000-8000-000000000002"}},"path_id":"SVPT-01900000-0000-7000-8000-000000000001","schema_id":"hsk.studio.vector_path@1"}}"#
        )
    }
    fn with_path(case: impl FnOnce(Path<'_>)) {
        let path_id = DomainId::parse("SVPT-01900000-0000-7000-8000-000000000001").unwrap();
        let layer_id = DomainId::parse("SLYR-01900000-0000-7000-8000-000000000002").unwrap();
        let anchors = anchors();
        case(Path {
            schema_id: "hsk.studio.vector_path@1",
            path_id: &path_id,
            address: GeometryAddress {
                layer_id: &layer_id,
                object_key: "tile_A",
            },
            closed: true,
            anchors: &anchors,
            winding_rule: Winding::None,
            fill_style_id: None,
        });
    }
    fn decode(bytes: &[u8], path: &Path<'_>, limits: &Limits) -> Result<[u8; 32], Error> {
        let token = CancellationToken::default();
        let epoch = Epoch(AtomicU64::new(1));
        verify_encoded_path(
            bytes,
            path,
            limits,
            &mut WorkMeter::new(&token, &epoch, 1, limits.work_units),
        )
    }
    #[test]
    fn encoded_strict_equivalence_and_source_hash() {
        bounded_case(|| {
            with_path(|path| {
                let original = fixture();
                let retained = original.clone();
                let first = decode(original.as_bytes(), &path, &limits()).unwrap();
                let expected: [u8; 32] = Sha256::digest(original.as_bytes()).into();
                assert_eq!(first, expected);
                assert_eq!(original, retained);
                let escaped = original
                    .replace("\"path_id\"", r#""path\u005fid""#)
                    .replace("tile_A", r#"tile\u005fA"#)
                    .replace("\"value\":0", "\"value\":-0e+0");
                let second = decode(escaped.as_bytes(), &path, &limits()).unwrap();
                assert_ne!(first, second); // Equal decoded values retain different exact source bytes.
                let reordered =
                    original.replace(r#""value":10,"unit":"pt""#, r#""unit":"pt","value":1e1"#);
                assert!(decode(reordered.as_bytes(), &path, &limits()).is_ok());
                let wrong_then_malformed = original
                    .replace("tile_A", "other")
                    .replace("hsk.studio.vector_path@1", r#"\uD800"#);
                assert_eq!(
                    decode(wrong_then_malformed.as_bytes(), &path, &limits()),
                    Err(Error::InvalidEncoding)
                );
            })
        });
    }
    #[test]
    fn encoded_closed_fields_utf8_and_numbers() {
        bounded_case(|| {
            with_path(|path| {
                let original = fixture();
                for (bytes, error) in [
                    (
                        original.replacen("{", r#"{"closed":true,"clo\u0073ed":true,"#, 1),
                        Error::DuplicateField,
                    ),
                    (
                        original.replacen("{", r#"{"extension":null,"#, 1),
                        Error::UnknownField,
                    ),
                    (
                        original.replace("\"closed\":true,", ""),
                        Error::MissingField,
                    ),
                    (
                        original.replace("tile_A", r#"\uD800"#),
                        Error::InvalidEncoding,
                    ),
                    (
                        original.replace("tile_A", r#"\uDC00"#),
                        Error::InvalidEncoding,
                    ),
                    (
                        original.replace("tile_A", r#"\uD83D\uDE00"#),
                        Error::ValueMismatch,
                    ),
                    (original.replace("tile_A", r#"\q"#), Error::InvalidEncoding),
                    (
                        original.replacen("\"value\":0", "\"value\":01", 1),
                        Error::InvalidEncoding,
                    ),
                    (
                        original.replacen("\"value\":0", "\"value\":1.", 1),
                        Error::InvalidEncoding,
                    ),
                    (
                        original.replacen("\"value\":0", "\"value\":1e+", 1),
                        Error::InvalidEncoding,
                    ),
                    (
                        original.replacen("\"value\":0", "\"value\":1e999", 1),
                        Error::InvalidGeometry,
                    ),
                    (
                        original.replace("\"unit\":\"pt\"", "\"unit\":\"px\""),
                        Error::ValueMismatch,
                    ),
                    (format!("{original} null"), Error::InvalidEncoding),
                ] {
                    assert_eq!(
                        decode(bytes.as_bytes(), &path, &limits()),
                        Err(error),
                        "{bytes}"
                    );
                }
                let mut invalid = original.as_bytes().to_vec();
                invalid[1] = 0xff;
                assert_eq!(
                    decode(&invalid, &path, &limits()),
                    Err(Error::InvalidEncoding)
                );
                let mut richer = anchors();
                richer[0].outgoing = Point::pt(2.0, 3.0).unwrap();
                let typed_richer = Path {
                    anchors: &richer,
                    ..path
                };
                // The decoder compares all handles; support classification is never used as a parse shortcut.
                assert_eq!(
                    decode(original.as_bytes(), &typed_richer, &limits()),
                    Err(Error::ValueMismatch)
                );
                let rich_bytes = original.replacen(
                    r#""outgoing":{"x":{"value":0,"unit":"pt"},"y":{"value":0,"unit":"pt"}}"#,
                    r#""outgoing":{"x":{"value":2,"unit":"pt"},"y":{"value":3,"unit":"pt"}}"#,
                    1,
                );
                assert!(decode(rich_bytes.as_bytes(), &typed_richer, &limits()).is_ok());
            })
        });
    }
    #[test]
    fn encoded_admission_cancel_and_epoch() {
        bounded_case(|| {
            with_path(|path| {
                let bytes = fixture();
                let mut caps = limits();
                caps.recursion_depth = 4;
                assert_eq!(
                    decode(bytes.as_bytes(), &path, &caps),
                    Err(Error::BudgetExceeded)
                );
                caps = limits();
                caps.input_bytes = 1;
                assert_eq!(
                    decode(bytes.as_bytes(), &path, &caps),
                    Err(Error::BudgetExceeded)
                );
                caps = limits();
                caps.anchors = 3;
                assert_eq!(
                    decode(bytes.as_bytes(), &path, &caps),
                    Err(Error::BudgetExceeded)
                );
                caps = limits();
                caps.work_units = 1;
                assert_eq!(
                    decode(bytes.as_bytes(), &path, &caps),
                    Err(Error::BudgetExceeded)
                );
                let token = CancellationToken::default();
                token.cancel();
                let epoch = Epoch(AtomicU64::new(1));
                let mut work = WorkMeter::new(&token, &epoch, 1, 100000);
                assert_eq!(
                    verify_encoded_path(bytes.as_bytes(), &path, &limits(), &mut work),
                    Err(Error::Cancelled)
                );
                assert_eq!(work.used(), 0);
                let token = CancellationToken::default();
                let mut work = WorkMeter::new(&token, &epoch, 2, 100000);
                assert_eq!(
                    verify_encoded_path(bytes.as_bytes(), &path, &limits(), &mut work),
                    Err(Error::StaleEpoch)
                );
                assert_eq!(work.used(), 0);
            })
        });
    }
}
