//! Content-stream tokenizer (ISO 32000 7.8.2). Produces operations with their source byte spans
//! so unchanged operations can be re-emitted verbatim. Nothing is executed or interpreted here.
use crate::error::{Limits, PressError, Result};
use crate::syntax::{Lexer, Object, is_regular, is_ws};

#[derive(Clone, Debug)]
pub(crate) struct Op {
    pub name: Vec<u8>,
    pub operands: Vec<Object>,
    /// Byte span (operands through operator) in the tokenized data.
    pub start: usize,
    pub end: usize,
}

pub(crate) fn parse_content(data: &[u8], limits: &Limits) -> Result<Vec<Op>> {
    let mut lx = Lexer::new(data, 0, limits.max_depth);
    let mut ops: Vec<Op> = Vec::new();
    loop {
        lx.skip_ws();
        if lx.peek().is_none() {
            break;
        }
        let start = lx.pos;
        let mut operands: Vec<Object> = Vec::new();
        let name = loop {
            lx.skip_ws();
            let Some(b) = lx.peek() else {
                // Trailing operands without an operator: ignore, as viewers do.
                return Ok(ops);
            };
            match b {
                b'/' | b'(' | b'<' | b'[' | b'+' | b'-' | b'.' | b'0'..=b'9' => {
                    operands.push(lx.parse_object(0, false)?);
                    if operands.len() > 256 {
                        return Err(PressError::InputLimit("operands"));
                    }
                }
                _ if is_regular(b) => {
                    let kw = lx.read_keyword();
                    match kw {
                        b"true" => operands.push(Object::Bool(true)),
                        b"false" => operands.push(Object::Bool(false)),
                        b"null" => operands.push(Object::Null),
                        _ => break kw.to_vec(),
                    }
                }
                _ => return Err(PressError::Malformed("content_token")),
            }
        };
        if name == b"BI" {
            // Inline image: key/value pairs, `ID`, binary data, `EI`.
            loop {
                lx.skip_ws();
                if lx.starts_with(b"ID") && lx.data.get(lx.pos + 2).is_none_or(|b| !is_regular(*b)) {
                    lx.pos += 2;
                    break;
                }
                if lx.peek().is_none() {
                    return Err(PressError::Malformed("inline_image"));
                }
                lx.parse_object(0, false)?;
            }
            if lx.peek().is_some_and(is_ws) {
                lx.pos += 1;
            }
            let mut p = lx.pos;
            let end = loop {
                let Some(at) = crate::reader::find(&data[p..], b"EI") else {
                    return Err(PressError::Malformed("inline_image"));
                };
                let q = p + at;
                let before_ok = q == 0 || is_ws(data[q - 1]);
                let after_ok = data.get(q + 2).is_none_or(|b| !is_regular(*b));
                if before_ok && after_ok {
                    break q + 2;
                }
                p = q + 2;
            };
            lx.pos = end;
            operands.clear();
        }
        ops.push(Op { name, operands, start, end: lx.pos });
        if ops.len() > limits.max_content_ops {
            return Err(PressError::InputLimit("max_content_ops"));
        }
    }
    Ok(ops)
}
