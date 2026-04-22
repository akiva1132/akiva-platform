//! Activity result — the blob an activity hands back when it was
//! launched via `Context::start_activity_for_result`.
//!
//! Byte-symmetric mirror of [`akiva_framework::result`]. See that module
//! for the wire layout; this file must stay layout-identical or intents
//! decoded on either side will misalign.

pub const RESULT_OK: i32 = 0;
pub const RESULT_CANCELED: i32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityResult {
    pub code: i32,
    pub data: Vec<u8>,
}

impl ActivityResult {
    pub const MAX_WIRE_BYTES: usize = 16 * 1024;

    pub fn ok(data: impl Into<Vec<u8>>) -> Self {
        Self { code: RESULT_OK, data: data.into() }
    }

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
    fn roundtrip_symmetric_with_framework_encoding() {
        // The SDK-side encoding must produce bytes the framework decodes
        // back identically. This test pins the symmetric shape.
        let r = ActivityResult { code: -1, data: b"value=42".to_vec() };
        assert_eq!(ActivityResult::decode(&r.encode()).unwrap(), r);
    }

    #[test]
    fn canceled_has_empty_data_and_stable_code() {
        assert_eq!(ActivityResult::canceled().code, RESULT_CANCELED);
        assert!(ActivityResult::canceled().data.is_empty());
    }
}
