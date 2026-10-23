//! Ogg bitstream reader written from RFC 3533: pages ("OggS", version 0, header type, granule position,
//! serial, sequence, CRC, lacing table), packets reassembled from lacing values across pages, one logical
//! stream selected by serial number. Every page CRC is checked (polynomial 0x04C11DB7, initial value 0,
//! no reflection: the Ogg CRC, not the ZIP CRC-32), packet sizes are bounded before they grow, and the
//! total of the stream is read from the granule position of its last page by a bounded backward scan.
#![deny(clippy::arithmetic_side_effects)]
use crate::error::{ReelError, check_cancel};
use crate::model::ReadSeek;
use hsk_studio_accord::CancellationToken;
use std::io::SeekFrom;

const PAGE_HEADER: usize = 27;
/// Largest Ogg page: header + 255 lacing values + 255 * 255 data bytes.
pub(crate) const MAX_PAGE: usize = PAGE_HEADER + 255 + 255 * 255;
/// How far from the end the last page of a stream is searched for.
const TAIL_SCAN: u64 = 4 * MAX_PAGE as u64;

fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let mut r = (i as u32) << 24;
        for _ in 0..8 {
            r = if r & 0x8000_0000 != 0 {
                (r << 1) ^ 0x04C1_1DB7
            } else {
                r << 1
            };
        }
        *slot = r;
    }
    table
}

fn page_crc(table: &[u32; 256], page: &[u8]) -> u32 {
    let mut crc = 0u32;
    for (i, &byte) in page.iter().enumerate() {
        // The CRC field (bytes 22..26) counts as zero.
        let b = if (22..26).contains(&i) { 0 } else { byte };
        let index = ((crc >> 24) as u8 ^ b) as usize;
        crc = (crc << 8) ^ table.get(index).copied().unwrap_or(0);
    }
    crc
}

/// One parsed page header.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PageHeader {
    pub(crate) continued: bool,
    pub(crate) bos: bool,
    pub(crate) eos: bool,
    pub(crate) granule: i64,
    pub(crate) serial: u32,
}

fn parse_header(head: &[u8]) -> Result<(PageHeader, usize), ReelError> {
    if head.get(..4) != Some(b"OggS") {
        return Err(ReelError::Corrupt {
            what: "ogg-capture-pattern",
        });
    }
    if head.get(4) != Some(&0) {
        return Err(ReelError::Unsupported {
            what: "ogg-version",
        });
    }
    let flags = head.get(5).copied().unwrap_or(0);
    let le64 = |at: usize| -> i64 {
        let mut b = [0u8; 8];
        if let Some(src) = head.get(at..at.saturating_add(8)) {
            b.copy_from_slice(src);
        }
        i64::from_le_bytes(b)
    };
    let le32 = |at: usize| -> u32 {
        let mut b = [0u8; 4];
        if let Some(src) = head.get(at..at.saturating_add(4)) {
            b.copy_from_slice(src);
        }
        u32::from_le_bytes(b)
    };
    let segments = usize::from(head.get(26).copied().unwrap_or(0));
    Ok((
        PageHeader {
            continued: flags & 1 != 0,
            bos: flags & 2 != 0,
            eos: flags & 4 != 0,
            granule: le64(6),
            serial: le32(14),
        },
        segments,
    ))
}

/// Sequential page and packet reader over one source.
pub(crate) struct OggReader<'a> {
    src: &'a mut dyn ReadSeek,
    table: [u32; 256],
    page: Vec<u8>,
    max_packet: usize,
    bytes_read: u64,
    /// Selected logical stream; `None` until `select` is called.
    serial: Option<u32>,
    /// Data of a packet that continues on the next page of the selected stream.
    partial: Vec<u8>,
    /// Complete packets of the current page not yet handed out.
    queue: std::collections::VecDeque<Vec<u8>>,
    /// Granule position of the last page read for the selected stream.
    pub(crate) last_granule: i64,
    ended: bool,
}

impl<'a> OggReader<'a> {
    pub(crate) fn new(src: &'a mut dyn ReadSeek, max_packet: usize) -> Result<Self, ReelError> {
        src.seek(SeekFrom::Start(0))?;
        Ok(Self {
            src,
            table: crc_table(),
            page: Vec::with_capacity(MAX_PAGE),
            max_packet,
            bytes_read: 0,
            serial: None,
            partial: Vec::new(),
            queue: std::collections::VecDeque::new(),
            last_granule: -1,
            ended: false,
        })
    }

    pub(crate) fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    /// Read the next page into `self.page`; `Ok(None)` at a clean end of the source.
    fn next_page(&mut self) -> Result<Option<(PageHeader, usize)>, ReelError> {
        let mut head = [0u8; PAGE_HEADER];
        let mut got = 0usize;
        while got < PAGE_HEADER {
            let n = self.src.read(head.get_mut(got..).unwrap_or(&mut []))?;
            if n == 0 {
                break;
            }
            got = got.saturating_add(n);
        }
        if got == 0 {
            return Ok(None);
        }
        if got < PAGE_HEADER {
            return Err(ReelError::Truncated { delivered: 0 });
        }
        let (header, segments) = parse_header(&head)?;
        let mut lacing = [0u8; 255];
        let lacing = lacing.get_mut(..segments).unwrap_or(&mut []);
        self.src
            .read_exact(lacing)
            .map_err(|_| ReelError::Truncated { delivered: 0 })?;
        let body: usize = lacing.iter().map(|v| usize::from(*v)).sum();
        self.page.clear();
        self.page.extend_from_slice(&head);
        self.page.extend_from_slice(lacing);
        let start = self.page.len();
        self.page.resize(start.saturating_add(body), 0);
        self.src
            .read_exact(self.page.get_mut(start..).unwrap_or(&mut []))
            .map_err(|_| ReelError::Truncated { delivered: 0 })?;
        let declared = u32::from_le_bytes([head[22], head[23], head[24], head[25]]);
        if page_crc(&self.table, &self.page) != declared {
            return Err(ReelError::Corrupt {
                what: "ogg-page-crc",
            });
        }
        self.bytes_read = self.bytes_read.saturating_add(self.page.len() as u64);
        Ok(Some((header, segments)))
    }

    /// The first packet of every beginning-of-stream page at the start of the file, with its serial,
    /// in file order (RFC 3533: all BOS pages come first). Leaves the reader before the first non-BOS
    /// page.
    pub(crate) fn bos_packets(&mut self) -> Result<Vec<(u32, Vec<u8>)>, ReelError> {
        let mut out = Vec::new();
        loop {
            let at = self.src.stream_position()?;
            let Some((header, segments)) = self.next_page()? else {
                break;
            };
            if !header.bos {
                self.src.seek(SeekFrom::Start(at))?;
                self.bytes_read = self.bytes_read.saturating_sub(self.page.len() as u64);
                break;
            }
            if out.len() >= 64 {
                return Err(ReelError::LimitExceeded {
                    what: "ogg-streams",
                });
            }
            let first = self.packets_of_page(segments).into_iter().next();
            let Some((data, complete)) = first else {
                return Err(ReelError::Corrupt {
                    what: "ogg-bos-page",
                });
            };
            if !complete {
                return Err(ReelError::Corrupt {
                    what: "ogg-bos-page",
                });
            }
            out.push((header.serial, data));
        }
        Ok(out)
    }

    /// Select the logical stream whose packets `next_packet` returns (its BOS packet was already
    /// returned by `bos_packets`).
    pub(crate) fn select(&mut self, serial: u32) {
        self.serial = Some(serial);
    }

    /// Packets (data, complete) of the page in `self.page`.
    fn packets_of_page(&self, segments: usize) -> Vec<(Vec<u8>, bool)> {
        let lacing = self
            .page
            .get(PAGE_HEADER..PAGE_HEADER.saturating_add(segments))
            .unwrap_or(&[]);
        let mut at = PAGE_HEADER.saturating_add(segments);
        let mut out = Vec::new();
        let mut current = Vec::new();
        let mut open = false;
        for &lace in lacing {
            let end = at.saturating_add(usize::from(lace));
            current.extend_from_slice(self.page.get(at..end).unwrap_or(&[]));
            at = end;
            open = true;
            if lace < 255 {
                out.push((std::mem::take(&mut current), true));
                open = false;
            }
        }
        if open {
            out.push((current, false));
        }
        out
    }

    /// Next complete packet of the selected stream; `Ok(None)` after its last packet.
    pub(crate) fn next_packet(
        &mut self,
        cancel: &CancellationToken,
    ) -> Result<Option<Vec<u8>>, ReelError> {
        let serial = self.serial.ok_or(ReelError::Corrupt {
            what: "ogg-no-stream",
        })?;
        loop {
            if let Some(packet) = self.queue.pop_front() {
                return Ok(Some(packet));
            }
            if self.ended {
                return Ok(None);
            }
            check_cancel(cancel)?;
            let Some((header, segments)) = self.next_page()? else {
                // A stream without an end-of-stream page still ends with the source; a packet that
                // was left open is truncated.
                if !self.partial.is_empty() {
                    return Err(ReelError::Truncated { delivered: 0 });
                }
                self.ended = true;
                continue;
            };
            if header.serial != serial {
                continue;
            }
            let packets = self.packets_of_page(segments);
            for (i, (data, complete)) in packets.into_iter().enumerate() {
                if i == 0 && !header.continued && !self.partial.is_empty() {
                    return Err(ReelError::Corrupt {
                        what: "ogg-lost-continuation",
                    });
                }
                if i == 0 && header.continued && self.partial.is_empty() {
                    return Err(ReelError::Corrupt {
                        what: "ogg-lost-continuation",
                    });
                }
                let size = self.partial.len().saturating_add(data.len());
                if size > self.max_packet {
                    return Err(ReelError::LimitExceeded {
                        what: "ogg-packet-size",
                    });
                }
                self.partial.extend_from_slice(&data);
                if complete {
                    self.queue.push_back(std::mem::take(&mut self.partial));
                }
            }
            if header.granule >= 0 {
                self.last_granule = header.granule;
            }
            if header.eos {
                self.ended = true;
            }
        }
    }
}

/// Granule position of the last page of stream `serial`, found by scanning at most `TAIL_SCAN` bytes
/// back from the end for a capture pattern whose page parses and passes its CRC.
pub(crate) fn last_granule(
    src: &mut dyn ReadSeek,
    file_len: u64,
    serial: u32,
) -> Result<Option<i64>, ReelError> {
    let start = file_len.saturating_sub(TAIL_SCAN);
    src.seek(SeekFrom::Start(start))?;
    let len = usize::try_from(file_len.saturating_sub(start))
        .map_err(|_| ReelError::LimitExceeded { what: "ogg-tail" })?;
    let mut tail = vec![0u8; len];
    src.read_exact(&mut tail)?;
    let table = crc_table();
    let mut best = None;
    let mut i = 0usize;
    while let Some(window) = tail.get(i..) {
        if window.len() < PAGE_HEADER {
            break;
        }
        if window.starts_with(b"OggS")
            && let Ok((header, segments)) = parse_header(window)
            && let Some(lacing) = window.get(PAGE_HEADER..PAGE_HEADER.saturating_add(segments))
        {
            let body: usize = lacing.iter().map(|v| usize::from(*v)).sum();
            let total = PAGE_HEADER.saturating_add(segments).saturating_add(body);
            if let Some(page) = window.get(..total) {
                let declared = u32::from_le_bytes([page[22], page[23], page[24], page[25]]);
                if page_crc(&table, page) == declared && header.serial == serial {
                    if header.granule >= 0 {
                        best = Some(header.granule);
                    }
                    i = i.saturating_add(total);
                    continue;
                }
            }
        }
        i = i.saturating_add(1);
    }
    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CRC of an all-zero page header with the CRC field zero is zero; one flipped bit changes it, and
    /// the table is the non-reflected 0x04C11DB7 table (entry 1 is the polynomial).
    #[test]
    fn ogg_crc_is_the_non_reflected_polynomial() {
        let table = crc_table();
        assert_eq!(table[1], 0x04C1_1DB7);
        assert_eq!(page_crc(&table, &[0u8; 27]), 0);
        let mut page = [0u8; 27];
        page[0] = 1;
        assert_ne!(page_crc(&table, &page), 0);
    }
}
