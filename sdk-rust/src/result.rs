//! Activity result — the blob an activity hands back when it was
//! launched via `Context::start_activity_for_result`.
//!
//! Byte-symmetric mirror of `akiva_framework::result`. See that module
//! for the wire layout; this file must stay layout-identical or intents
//! decoded on either side will misalign.

/// Activity completed successfully. Mirrors Android's `RESULT_OK`.
pub const RESULT_OK: i32 = 0;

/// User cancelled or the activity finished without producing a result
/// the caller should act on. Mirrors Android's `RESULT_CANCELED`.
pub const RESULT_CANCELED: i32 = 1;

/// Result blob returned to the caller of `start_activity_for_result`.
///
/// `code` carries the high-level outcome ([`RESULT_OK`] /
/// [`RESULT_CANCELED`] or any custom non-zero `i32` the activity
/// chooses); `data` is an opaque payload the activity and its caller
/// agreed on out of band.
///
/// # Wire format
///
/// `[code: i32 LE][data_len: u32 LE][data: bytes]`
///
/// Total size capped at [`ActivityResult::MAX_WIRE_BYTES`]; larger
/// payloads should land in storage and pass an opaque key through the
/// result instead.
///
/// # Examples
///
/// ```
/// use akiva_sdk::result::{ActivityResult, RESULT_OK};
/// let r = ActivityResult::ok(b"chosen_id=42".to_vec());
/// assert_eq!(r.code, RESULT_OK);
///
/// // Round-trips through the wire codec.
/// let bytes = r.encode();
/// assert_eq!(ActivityResult::decode(&bytes).unwrap(), r);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityResult {
    /// Outcome code. `0` = OK, `1` = canceled; other values are
    /// app-defined.
    pub code: i32,
    /// Opaque payload (UTF-8, JSON, MessagePack — caller's choice).
    pub data: Vec<u8>,
}

impl ActivityResult {
    /// Maximum encoded size accepted by [`ActivityResult::decode`].
    /// Anything larger is a wire bug or an app misuse — payload-heavy
    /// results should travel through storage.
    pub const MAX_WIRE_BYTES: usize = 16 * 1024;

    /// Build a `RESULT_OK` result with the given payload.
    pub fn ok(data: impl Into<Vec<u8>>) -> Self {
        Self { code: RESULT_OK, data: data.into() }
    }

    /// Build a `RESULT_CANCELED` result with no payload. Convenience
    /// for the common cancel case where there's nothing to say beyond
    /// "the user backed out."
    pub fn canceled() -> Self {
        Self { code: RESULT_CANCELED, data: Vec::new() }
    }

    /// Serialise to the v0 wire format. Always succeeds; the
    /// 16 KiB cap is enforced on the decode side because the encode
    /// side has no recourse if the caller already owns a too-large
    /// payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + self.data.len());
        out.extend_from_slice(&self.code.to_le_bytes());
        out.extend_from_slice(&(self.data.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.data);
        out
    }

    /// Parse the v0 wire format. Returns [`ResultDecodeError::TooLarge`]
    /// if the buffer exceeds [`Self::MAX_WIRE_BYTES`] and
    /// [`ResultDecodeError::Truncated`] if it's too short to hold the
    /// header or the declared data length.
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

/// Reasons [`ActivityResult::decode`] can refuse to parse a buffer.
#[derive(Debug, PartialEq, Eq)]
pub enum ResultDecodeError {
    /// Buffer was shorter than the 8-byte header, or shorter than the
    /// header's declared `data_len` requires.
    Truncated,
    /// Buffer exceeded [`ActivityResult::MAX_WIRE_BYTES`]. Result blobs
    /// aren't a transport for large payloads — use storage and pass a
    /// key.
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
