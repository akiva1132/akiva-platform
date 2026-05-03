//! Geolocation — Stone 14.6.
//!
//! Apps with `akiva.permission.LOCATION` declared in their manifest
//! can read the device's current position via
//! `navigator.geolocation.getCurrentPosition`. Two layers of
//! permission: the system grant (auto-granted today, Normal
//! protection) and the browser's own permission prompt, which fires
//! the first time the API is called.
//!
//! ```ignore
//! use akiva_sdk::geolocation;
//!
//! async fn locate() -> Result<(), geolocation::GeoError> {
//!     let p = geolocation::get_position().await?;
//!     log_info!("at {:.4}, {:.4} (±{:.0}m)", p.latitude, p.longitude, p.accuracy_m);
//!     Ok(())
//! }
//! ```

use core::future::Future;
use core::pin::Pin;
use core::task::{Context as TaskContext, Poll};

use crate::op_kinds::io;
#[cfg(target_arch = "wasm32")]
use crate::op_kinds::{status, unpack_poll};

#[derive(Debug, Clone, PartialEq)]
pub struct Position {
    pub latitude: f64,
    pub longitude: f64,
    pub accuracy_m: f64,
    /// Altitude in meters above the WGS-84 ellipsoid. `None` on
    /// devices/browsers that don't report it (most desktops).
    pub altitude_m: Option<f64>,
    /// Speed in m/s. `None` if not available.
    pub speed_m_s: Option<f64>,
    /// Heading in degrees, clockwise from true north. `None` if
    /// the device is stationary or the browser doesn't report it.
    pub heading_deg: Option<f64>,
    /// Timestamp of the position fix, milliseconds since UNIX epoch.
    pub timestamp_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeoError {
    /// The package doesn't hold `akiva.permission.LOCATION`, or
    /// the host rejected the call before posting a future.
    Denied,
    /// Browser-level: the user denied the geolocation permission
    /// prompt, the position was unavailable (no GPS / no network
    /// signal), or the request timed out. Reason is the browser's
    /// `PositionError.message`.
    Host(String),
    /// Stone 11.6.8 #2 — slot backpressure.
    Busy,
}

/// Read the device's current position. Resolves on the browser's
/// success callback; ERROR with the browser's reason on permission
/// denial, position unavailable, or timeout.
pub fn get_position() -> GetPositionFuture {
    let fid = io_start(io::GEO_GET_POSITION, &[]);
    if fid == 0 {
        return GetPositionFuture::pre_resolved(Err(GeoError::Denied));
    }
    GetPositionFuture { future_id: fid, cached: None }
}

#[must_use]
pub struct GetPositionFuture {
    future_id: u64,
    cached: Option<Result<Position, GeoError>>,
}

impl GetPositionFuture {
    fn pre_resolved(r: Result<Position, GeoError>) -> Self {
        Self { future_id: 0, cached: Some(r) }
    }
}

impl Future for GetPositionFuture {
    type Output = Result<Position, GeoError>;

    fn poll(mut self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        if let Some(cached) = self.cached.take() {
            self.cached = Some(cached.clone());
            return Poll::Ready(cached);
        }
        match poll_get_position_inner(self.future_id) {
            None => Poll::Pending,
            Some(result) => {
                self.cached = Some(result.clone());
                Poll::Ready(result)
            }
        }
    }
}

/// Decode the [`io::GEO_GET_POSITION`] reply. Public to the crate
/// for testing.
pub(crate) fn decode_position(buf: &[u8]) -> Result<Position, GeoError> {
    if buf.len() < 24 + 1 + 1 + 1 + 8 {
        return Err(GeoError::Host(format!("position reply truncated ({} bytes)", buf.len())));
    }
    let mut cur = 0;
    let latitude = read_f64(buf, &mut cur);
    let longitude = read_f64(buf, &mut cur);
    let accuracy_m = read_f64(buf, &mut cur);
    let altitude_m = read_optional_f64(buf, &mut cur);
    let speed_m_s = read_optional_f64(buf, &mut cur);
    let heading_deg = read_optional_f64(buf, &mut cur);
    if cur + 8 > buf.len() {
        return Err(GeoError::Host("position reply: timestamp truncated".into()));
    }
    let timestamp_ms = read_f64(buf, &mut cur);
    Ok(Position {
        latitude, longitude, accuracy_m,
        altitude_m, speed_m_s, heading_deg,
        timestamp_ms,
    })
}

fn read_f64(buf: &[u8], cur: &mut usize) -> f64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&buf[*cur..*cur + 8]);
    *cur += 8;
    f64::from_le_bytes(bytes)
}

fn read_optional_f64(buf: &[u8], cur: &mut usize) -> Option<f64> {
    if *cur >= buf.len() { return None; }
    let present = buf[*cur];
    *cur += 1;
    if present == 1 && *cur + 8 <= buf.len() {
        Some(read_f64(buf, cur))
    } else {
        None
    }
}

// ── wasm/non-wasm glue ─────────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
fn io_start(op_kind: u32, args: &[u8]) -> u64 { crate::runtime::host_io_start(op_kind, args) }

#[cfg(not(target_arch = "wasm32"))]
fn io_start(_op_kind: u32, _args: &[u8]) -> u64 { 0 }

#[cfg(target_arch = "wasm32")]
fn poll_get_position_inner(future_id: u64) -> Option<Result<Position, GeoError>> {
    use crate::runtime::host_io_poll;
    if future_id == 0 { return Some(Err(GeoError::Denied)); }
    let mut buf = vec![0u8; 64];
    let packed = host_io_poll(future_id, &mut buf);
    let (st, len) = unpack_poll(packed);
    let len = len as usize;
    match st {
        status::PENDING => None,
        status::READY => {
            if len > buf.len() {
                buf = vec![0u8; len];
                let packed = host_io_poll(future_id, &mut buf);
                let (st2, len2) = unpack_poll(packed);
                if st2 != status::READY {
                    return Some(Err(GeoError::Host("retry not READY".into())));
                }
                buf.truncate((len2 as usize).min(buf.len()));
            } else {
                buf.truncate(len);
            }
            Some(decode_position(&buf))
        }
        status::ERROR => {
            buf.truncate(len.min(buf.len()));
            Some(Err(GeoError::Host(String::from_utf8_lossy(&buf).into_owned())))
        }
        status::BUSY => Some(Err(GeoError::Busy)),
        _ => Some(Err(GeoError::Host("unknown status".into()))),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn poll_get_position_inner(_future_id: u64) -> Option<Result<Position, GeoError>> {
    Some(Err(GeoError::Denied))
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::task::{RawWaker, RawWakerVTable, Waker};

    fn noop_waker() -> Waker {
        static VT: RawWakerVTable = RawWakerVTable::new(
            |_| RawWaker::new(core::ptr::null(), &VT),
            |_| {}, |_| {}, |_| {},
        );
        unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &VT)) }
    }

    #[test]
    fn decode_position_minimal_no_optionals() {
        // [lat:f64][lon:f64][acc:f64][has_alt:0][has_speed:0][has_head:0][ts:f64]
        let mut wire = Vec::new();
        wire.extend_from_slice(&31.7683_f64.to_le_bytes()); // Jerusalem-ish
        wire.extend_from_slice(&35.2137_f64.to_le_bytes());
        wire.extend_from_slice(&12.5_f64.to_le_bytes());
        wire.push(0); wire.push(0); wire.push(0);
        wire.extend_from_slice(&1_700_000_000_000.0_f64.to_le_bytes());
        let p = decode_position(&wire).unwrap();
        assert!((p.latitude - 31.7683).abs() < 1e-9);
        assert!((p.longitude - 35.2137).abs() < 1e-9);
        assert_eq!(p.accuracy_m, 12.5);
        assert_eq!(p.altitude_m, None);
        assert_eq!(p.speed_m_s, None);
        assert_eq!(p.heading_deg, None);
        assert_eq!(p.timestamp_ms, 1_700_000_000_000.0);
    }

    #[test]
    fn decode_position_with_altitude_and_heading() {
        let mut wire = Vec::new();
        wire.extend_from_slice(&0.0_f64.to_le_bytes());
        wire.extend_from_slice(&0.0_f64.to_le_bytes());
        wire.extend_from_slice(&5.0_f64.to_le_bytes());
        wire.push(1); wire.extend_from_slice(&750.0_f64.to_le_bytes()); // alt
        wire.push(0); // no speed
        wire.push(1); wire.extend_from_slice(&90.0_f64.to_le_bytes()); // heading east
        wire.extend_from_slice(&42.0_f64.to_le_bytes());
        let p = decode_position(&wire).unwrap();
        assert_eq!(p.altitude_m, Some(750.0));
        assert_eq!(p.speed_m_s, None);
        assert_eq!(p.heading_deg, Some(90.0));
    }

    #[test]
    fn decode_position_truncated_returns_host_error() {
        let r = decode_position(&[0; 10]); // way too short
        assert!(matches!(r, Err(GeoError::Host(_))));
    }

    #[test]
    fn get_position_on_non_wasm_resolves_denied() {
        let mut f = get_position();
        let waker = noop_waker();
        let mut cx = TaskContext::from_waker(&waker);
        match Pin::new(&mut f).poll(&mut cx) {
            Poll::Ready(Err(GeoError::Denied)) => {}
            other => panic!("expected Ready(Err(Denied)), got {other:?}"),
        }
    }
}
