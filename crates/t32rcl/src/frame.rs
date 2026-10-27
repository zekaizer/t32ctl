// SPDX-License-Identifier: Apache-2.0
// Ported from t32mcp src/t32api.rs (Apache-2.0, (c) 2026 Lauterbach GmbH) and pyrcl 1.1.6
// _rc/hlinknet.py (MIT, (c) 2020 Lauterbach GmbH). See NOTICE.
//! NETTCP framing. Sans-IO: bytes in, frames out.
//!
//! ```text
//! | u32 le length | u32 le type | payload (length bytes) | zero pad to a multiple of 8 |
//!   type 0x10 REQ (client to PowerView), 0x11 RESP, 0x12 NOTIFY
//! ```
//!
//! Source: t32api.rs `transmit`/`extract_messages`, hlinknet.py `CommunicationTcp`. Deviations:
//! a frame is returned only once its pad has arrived too (t32api.rs drains a partial pad and then
//! misreads the rest as the next header), and a length above `MAX_PAYLOAD` is a violation as soon
//! as the header is in, so a non-RCL peer cannot make the client buffer or wait for gigabytes.

use crate::error::Violation;

pub(crate) const HEADER_LEN: usize = 8;
pub(crate) const ALIGN: usize = 8;
/// Sanity bound, not a protocol limit: every reply the client provokes states its size in a u16
/// (memory read length, EXP reply length) after the 2-byte `[err][id]`. Neither reference bounds
/// the length. ASSUMED(macro): PowerView sends a long EXP reply as one frame; the probe's
/// 60000-byte value is the longest reply it provokes.
pub(crate) const MAX_PAYLOAD: usize = 2 + u16::MAX as usize;

pub(crate) const TYPE_REQ: u32 = 0x10;
pub(crate) const TYPE_RESP: u32 = 0x11;
pub(crate) const TYPE_NOTIFY: u32 = 0x12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FrameKind {
    Response,
    Notify,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    pub kind: FrameKind,
    pub payload: Vec<u8>,
}

fn padded_len(payload_len: usize) -> usize {
    (HEADER_LEN + payload_len).next_multiple_of(ALIGN)
}

/// Wrap one RCL message into a frame of type `ty`.
pub(crate) fn encode(ty: u32, message: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(padded_len(message.len()));
    out.extend_from_slice(&(message.len() as u32).to_le_bytes());
    out.extend_from_slice(&ty.to_le_bytes());
    out.extend_from_slice(message);
    out.resize(padded_len(message.len()), 0);
    out
}

pub(crate) fn encode_request(message: &[u8]) -> Vec<u8> {
    encode(TYPE_REQ, message)
}

/// Incremental frame decoder. Feed arbitrary slices, pull complete frames.
#[derive(Debug, Default)]
pub(crate) struct Framer {
    buf: Vec<u8>,
}

impl Framer {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Next complete frame including its pad, `Ok(None)` if more bytes are needed.
    /// ASSUMED(connect): PowerView always sends the pad, as both references expect.
    /// An error is terminal for the stream; the caller poisons the link.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, Violation> {
        let Some(header) = self.buf.get(..HEADER_LEN) else { return Ok(None) };
        let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
        let ty = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
        if len as usize > MAX_PAYLOAD {
            return Err(Violation::FrameTooLarge(len));
        }
        let kind = match ty {
            TYPE_RESP => FrameKind::Response,
            TYPE_NOTIFY => FrameKind::Notify,
            other => return Err(Violation::BadFrameType(other)),
        };
        let len = len as usize;
        let total = padded_len(len);
        if self.buf.len() < total {
            return Ok(None);
        }
        let payload = self.buf[HEADER_LEN..HEADER_LEN + len].to_vec();
        self.buf.drain(..total);
        Ok(Some(Frame { kind, payload }))
    }

    /// First bytes seen, for `NotRclEvidence::Banner`: printable ASCII only, at most 32 chars.
    pub fn banner(&self) -> Option<String> {
        let head: String =
            self.buf.iter().take(32).take_while(|b| (0x20..0x7f).contains(*b)).map(|b| *b as char).collect();
        (head.len() >= 4).then_some(head)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream() -> (Vec<u8>, Vec<Frame>) {
        let frames = vec![
            Frame { kind: FrameKind::Response, payload: vec![0, 1, 7, 8] },
            Frame { kind: FrameKind::Notify, payload: (0..8).collect() },
            Frame { kind: FrameKind::Response, payload: vec![0xFE] },
            Frame { kind: FrameKind::Response, payload: Vec::new() },
            Frame { kind: FrameKind::Response, payload: (0..13).collect() },
        ];
        let mut bytes = Vec::new();
        for f in &frames {
            let ty = if f.kind == FrameKind::Response { TYPE_RESP } else { TYPE_NOTIFY };
            bytes.extend(encode(ty, &f.payload));
        }
        (bytes, frames)
    }

    fn drain(framer: &mut Framer, out: &mut Vec<Frame>) {
        while let Some(f) = framer.next_frame().expect("valid stream") {
            out.push(f);
        }
    }

    #[test]
    fn every_two_way_split_yields_the_same_frames() {
        let (bytes, want) = stream();
        for cut in 0..=bytes.len() {
            let mut framer = Framer::default();
            let mut got = Vec::new();
            framer.push(&bytes[..cut]);
            drain(&mut framer, &mut got);
            framer.push(&bytes[cut..]);
            drain(&mut framer, &mut got);
            assert_eq!(got, want, "split at {cut}");
        }
    }

    #[test]
    fn byte_by_byte_feed_yields_the_same_frames() {
        let (bytes, want) = stream();
        let mut framer = Framer::default();
        let mut got = Vec::new();
        for b in bytes {
            framer.push(&[b]);
            drain(&mut framer, &mut got);
        }
        assert_eq!(got, want);
    }

    #[test]
    fn a_frame_waits_for_its_pad() {
        let bytes = encode(TYPE_RESP, &[0, 1, 2]);
        assert_eq!(bytes.len(), 16);
        let mut framer = Framer::default();
        framer.push(&bytes[..11]);
        assert_eq!(framer.next_frame(), Ok(None));
        framer.push(&bytes[11..]);
        assert_eq!(framer.next_frame(), Ok(Some(Frame { kind: FrameKind::Response, payload: vec![0, 1, 2] })));
    }

    #[test]
    fn oversized_length_and_unknown_type_fail_at_the_header() {
        let mut framer = Framer::default();
        framer.push(b"HTTP/1.1 400 Bad Request");
        assert_eq!(framer.next_frame(), Err(Violation::FrameTooLarge(0x5054_5448)));
        assert_eq!(framer.banner().as_deref(), Some("HTTP/1.1 400 Bad Request"));

        let mut framer = Framer::default();
        framer.push(&[4, 0, 0, 0, 0x10, 0, 0, 0]);
        assert_eq!(framer.next_frame(), Err(Violation::BadFrameType(0x10)));
    }

    #[test]
    fn request_frame_golden() {
        assert_eq!(encode_request(&[2, 0x71, 1, 1]), [4, 0, 0, 0, 0x10, 0, 0, 0, 2, 0x71, 1, 1, 0, 0, 0, 0]);
    }
}
