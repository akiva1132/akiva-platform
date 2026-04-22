//! Activity result — the payload an activity hands back to whatever
//! launched it via `start_activity_for_result`.
//!
//! Wire layout is deliberately tiny and opaque:
//!
//! ```text
//!   i32 code   (little-endian, 4 bytes)
//!   u32 data_len + <data_len> bytes
//! ```
//!
//! `code` uses our own constants (not Android's `-1 / 0`) so we don't
//! bake a sign-bit convention into the wire:
//!   - `RESULT_OK       = 0`
//!   - `RESULT_CANCELED = 1`
//!   - anything else is app-defined (caller + callee agree out-of-band).
//!
//! `data` is opaque bytes. Apps serialise whatever payload they like —
//! a single integer, a struct encoded as bincode, a JSON blob. The
//! framework never looks at it.
//!
//! Max wire size matches [`crate::intent::Intent::MAX_WIRE_BYTES`]
//! (16 KiB) — a picker result exchanging a number or a short string
//! comes in at tens of bytes; anything much larger should go through
//! shared storage instead.

pub const RESULT_OK: i32 = 0;
pub const RESULT_CANCELED: i32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityResult {
    pub code: i32,
    pub data: Vec<u8>,
}

impl ActivityResult {
    pub const MAX_WIRE_BYTES: usize = 16 * 1024;

    /// Convenience for the "callee finished without calling set_result"
    /// synthesis path in `finish_top`. Empty data, canceled code.
    pub fn canceled() -> Self {
        Self { code: RESULT_CANCELED, data: Vec::new() }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + self.data.len());
        out.extend_from_slice(&self.code.to_le_bytes());
        out.extend_from_slice(&(self.data.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.data);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ResultDecodeError> {
        if bytes.len() > Self::MAX_WIRE_BYTES {
            return Err(ResultDecodeError::TooLarge);
        }
        if bytes.len() < 8 {
            return Err(ResultDecodeError::Truncated);
        }
        let code = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let data_len = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
        let expected = 8usize.checked_add(data_len).ok_or(ResultDecodeError::Truncated)?;
        if bytes.len() < expected {
            return Err(ResultDecodeError::Truncated);
        }
        Ok(Self {
            code,
            data: bytes[8..expected].to_vec(),
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ResultDecodeError {
    Truncated,
    TooLarge,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_basic() {
        let r = ActivityResult { code: RESULT_OK, data: b"42".to_vec() };
        assert_eq!(ActivityResult::decode(&r.encode()).unwrap(), r);
    }

    #[test]
    fn roundtrip_empty_data() {
        let r = ActivityResult::canceled();
        assert_eq!(ActivityResult::decode(&r.encode()).unwrap(), r);
    }

    #[test]
    fn roundtrip_custom_code_and_large_data() {
        let r = ActivityResult { code: 42, data: vec![0xAB; 4000] };
        assert_eq!(ActivityResult::decode(&r.encode()).unwrap(), r);
    }

    #[test]
    fn roundtrip_negative_code() {
        // i32 space includes negatives. Encoded as two's-complement LE,
        // decode restores the sign.
        let r = ActivityResult { code: -7, data: vec![1, 2, 3] };
        assert_eq!(ActivityResult::decode(&r.encode()).unwrap(), r);
    }

    #[test]
    fn decode_rejects_truncated_header() {
        assert_eq!(ActivityResult::decode(&[]), Err(ResultDecodeError::Truncated));
        assert_eq!(ActivityResult::decode(&[0u8; 7]), Err(ResultDecodeError::Truncated));
    }

    #[test]
    fn decode_rejects_truncated_data() {
        // code=0, data_len=10, but only 2 bytes of data follow.
        let mut bytes = vec![0, 0, 0, 0, 10, 0, 0, 0];
        bytes.extend_from_slice(&[0xFF, 0xFF]);
        assert_eq!(ActivityResult::decode(&bytes), Err(ResultDecodeError::Truncated));
    }

    #[test]
    fn decode_rejects_too_large() {
        let oversized = vec![0u8; ActivityResult::MAX_WIRE_BYTES + 1];
        assert_eq!(ActivityResult::decode(&oversized), Err(ResultDecodeError::TooLarge));
    }

    #[test]
    fn constants_match_the_contract_in_the_doc_comment() {
        // Locks the wire values so a future accidental bump doesn't
        // silently break every app that compares against OK / CANCELED.
        assert_eq!(RESULT_OK, 0);
        assert_eq!(RESULT_CANCELED, 1);
    }
}
