//! HMAC-based proof verification for `session.hello`.
//!
//! Client proof is HMAC-SHA256 over the ASCII transcript:
//!   `"superhq:v1:" || host_node_id || ":" || device_id || ":" || timestamp`
//! using the shared `device_key` as the HMAC key.

use base64::{engine::general_purpose::STANDARD, Engine};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

const DOMAIN: &str = "superhq:v1:";

/// ±5-minute timestamp window; tuned to tolerate normal clock drift.
pub const MAX_SKEW_SECS: u64 = 300;

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("timestamp out of window: got {got}, now {now}, max skew {max}s")]
    Skew { got: u64, now: u64, max: u64 },
    #[error("invalid base64 in proof")]
    BadBase64,
    #[error("hmac mismatch")]
    Mismatch,
    #[error("device key wrong length (need 32 bytes)")]
    BadKeyLen,
}

/// Build the proof for a given triple of (host_id, device_id, timestamp).
pub fn compute_proof(
    device_key: &[u8],
    host_node_id: &str,
    device_id: &str,
    timestamp: u64,
) -> Result<String, AuthError> {
    if device_key.len() != 32 {
        return Err(AuthError::BadKeyLen);
    }
    let mut mac = HmacSha256::new_from_slice(device_key)
        .map_err(|_| AuthError::BadKeyLen)?;
    mac.update(DOMAIN.as_bytes());
    mac.update(host_node_id.as_bytes());
    mac.update(b":");
    mac.update(device_id.as_bytes());
    mac.update(b":");
    mac.update(timestamp.to_string().as_bytes());
    let tag = mac.finalize().into_bytes();
    Ok(STANDARD.encode(tag))
}

/// Verify a client-provided proof.
pub fn verify_proof(
    device_key: &[u8],
    host_node_id: &str,
    device_id: &str,
    timestamp: u64,
    proof_b64: &str,
    now_secs: u64,
) -> Result<(), AuthError> {
    // Timestamp window check.
    let skew = if now_secs > timestamp {
        now_secs - timestamp
    } else {
        timestamp - now_secs
    };
    if skew > MAX_SKEW_SECS {
        return Err(AuthError::Skew {
            got: timestamp,
            now: now_secs,
            max: MAX_SKEW_SECS,
        });
    }
    // Decode the claimed proof.
    let claimed = STANDARD
        .decode(proof_b64.as_bytes())
        .map_err(|_| AuthError::BadBase64)?;
    // Recompute on our side and compare (constant-time).
    let mut mac = HmacSha256::new_from_slice(device_key)
        .map_err(|_| AuthError::BadKeyLen)?;
    mac.update(DOMAIN.as_bytes());
    mac.update(host_node_id.as_bytes());
    mac.update(b":");
    mac.update(device_id.as_bytes());
    mac.update(b":");
    mac.update(timestamp.to_string().as_bytes());
    mac.verify_slice(&claimed).map_err(|_| AuthError::Mismatch)
}

/// Get the current UNIX second count.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Generate a random 32-byte device key.
pub fn generate_device_key() -> [u8; 32] {
    use rand::RngCore;
    let mut k = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut k);
    k
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_proof() {
        let key = [7u8; 32];
        let host = "host-abc";
        let device = "dev-xyz";
        let ts = 1_700_000_000u64;
        let proof = compute_proof(&key, host, device, ts).unwrap();
        verify_proof(&key, host, device, ts, &proof, ts).unwrap();
    }

    #[test]
    fn rejects_wrong_key() {
        let proof = compute_proof(&[1u8; 32], "h", "d", 100).unwrap();
        assert!(matches!(
            verify_proof(&[2u8; 32], "h", "d", 100, &proof, 100),
            Err(AuthError::Mismatch)
        ));
    }

    #[test]
    fn rejects_tampered_transcript() {
        let proof = compute_proof(&[7u8; 32], "h", "d", 100).unwrap();
        assert!(matches!(
            verify_proof(&[7u8; 32], "h", "d2", 100, &proof, 100),
            Err(AuthError::Mismatch)
        ));
    }

    #[test]
    fn rejects_stale_timestamp() {
        let proof = compute_proof(&[7u8; 32], "h", "d", 100).unwrap();
        assert!(matches!(
            verify_proof(&[7u8; 32], "h", "d", 100, &proof, 1000),
            Err(AuthError::Skew { .. })
        ));
    }
}
