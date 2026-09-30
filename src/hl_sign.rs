//! L1 signing adapted from XEMM 7863f14bc104c85a5015c2f283f77453f3ad67ec.
//! Field order in MessagePack is part of the signature; SDK vectors are below.
use anyhow::{Context, Result};
use k256::ecdsa::SigningKey;
use serde::Serialize;
use tiny_keccak::{Hasher, Keccak};

pub fn keccak(bytes: &[u8]) -> [u8; 32] {
    let mut hash = Keccak::v256();
    let mut out = [0; 32];
    hash.update(bytes);
    hash.finalize(&mut out);
    out
}
pub fn action_hash(
    packed: &[u8],
    nonce: u64,
    vault: Option<&[u8; 20]>,
    expiry: Option<u64>,
) -> [u8; 32] {
    let mut bytes = packed.to_vec();
    bytes.extend_from_slice(&nonce.to_be_bytes());
    if let Some(address) = vault {
        bytes.push(1);
        bytes.extend_from_slice(address);
    } else {
        bytes.push(0);
    }
    if let Some(expiry) = expiry {
        bytes.push(0);
        bytes.extend_from_slice(&expiry.to_be_bytes());
    }
    keccak(&bytes)
}
fn digest(connection: &[u8; 32], mainnet: bool) -> [u8; 32] {
    let mut domain = [0; 160];
    domain[..32].copy_from_slice(&keccak(
        b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
    ));
    domain[32..64].copy_from_slice(&keccak(b"Exchange"));
    domain[64..96].copy_from_slice(&keccak(b"1"));
    domain[120..128].copy_from_slice(&1337u64.to_be_bytes());
    let mut agent = [0; 96];
    agent[..32].copy_from_slice(&keccak(b"Agent(string source,bytes32 connectionId)"));
    agent[32..64].copy_from_slice(&keccak(if mainnet { b"a" } else { b"b" }));
    agent[64..].copy_from_slice(connection);
    let mut bytes = vec![0x19, 0x01];
    bytes.extend_from_slice(&keccak(&domain));
    bytes.extend_from_slice(&keccak(&agent));
    keccak(&bytes)
}
#[derive(Serialize)]
pub struct Signature {
    pub r: String,
    pub s: String,
    pub v: u8,
}
pub fn sign<A: Serialize>(
    key: &SigningKey,
    action: &A,
    nonce: u64,
    vault: Option<&[u8; 20]>,
    expiry: Option<u64>,
) -> Result<Signature> {
    let packed = rmp_serde::to_vec_named(action)?;
    let (signature, recovery) = key
        .sign_prehash_recoverable(&digest(&action_hash(&packed, nonce, vault, expiry), true))
        .context("Hyperliquid action signing failed")?;
    Ok(Signature {
        r: format!("0x{}", hex::encode(signature.r().to_bytes())),
        s: format!("0x{}", hex::encode(signature.s().to_bytes())),
        v: 27 + recovery.to_byte(),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sdk_digest_and_vault_expiry_vectors() {
        let connection = std::array::from_fn(|i| i as u8);
        assert_eq!(
            hex::encode(digest(&connection, true)),
            "b9e7c81cff512fa0969928e37d7c2475af657f1b314b7458c8dd7a023044cac0"
        );
        assert_eq!(
            hex::encode(digest(&connection, false)),
            "4384ea9179d358ab65dd0375834d2e206330bd138f8e05c763d97f6e3f54bac1"
        );
        let packed = hex::decode("83a474797065a56f72646572a66f72646572739187a16105a162c3a170a63132332e3435a173a3302e35a172c2a17481a56c696d697481a3746966a3496f63a163d92230783030303130323033303430353036303730383039306130623063306430653066a867726f7570696e67a26e61").unwrap();
        assert_eq!(
            hex::encode(action_hash(
                &packed,
                1234567,
                Some(&[0x11; 20]),
                Some(9999999)
            )),
            "e82fb2a10383cb80986147f11cb09de79436a1dd8c976f3ab29bf64678b8836f"
        );
        let mut raw = [0; 32];
        raw[31] = 1;
        let key = SigningKey::from_slice(&raw).unwrap();
        let (signature, recovery) = key
            .sign_prehash_recoverable(&digest(&connection, true))
            .unwrap();
        assert_eq!(
            hex::encode(signature.r().to_bytes()),
            "6fac96b099be7b8cdf3bc5ccec1b7966b2543f97941734f89f106b3f18e28d3b"
        );
        assert_eq!(
            hex::encode(signature.s().to_bytes()),
            "1ab95dff17bf2a60a06d9471d504e174cabc3496da75c50c04fe21ddb30f13ef"
        );
        assert_eq!(27 + recovery.to_byte(), 28);
    }
}
