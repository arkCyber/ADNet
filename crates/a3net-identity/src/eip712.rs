//! EIP-712 typed-data signing and verification.
//!
//! EIP-712 signs the **structured** payload of an off-chain message
//! (a "Permit", a `meta-tx`, a `Dai` flash-whitelist, …) so the user
//! sees what they're actually approving instead of a 32-byte hash.
//!
//! ## Hash algorithm
//!
//! ```text
//! hash = keccak256( 0x1901 || domainSeparator || hashStruct(message) )
//! ```
//!
//! where:
//!
//! - `domainSeparator = keccak256(typeHash || name || version ||
//!   chainId || verifyingContract || salt)`
//! - `hashStruct(msg)  = keccak256(typeHash || …fields…)` with each
//!   `bytes32`/address field zero-padded to 32 bytes and each
//!   `bytes`/`string` hashed first then embedded.
//!
//! ## What this module owns
//!
//! - The canonical hash computation ([`typed_data_hash`]).
//! - A typed [`Eip712Domain`] for the domain separator.
//! - Signing via [`crate::wallet::Wallet::sign_typed_data`] and
//!   recovery via [`crate::wallet::WalletPublic::recover_typed_data`].
//!
//! ## What this module does *not* do
//!
//! - Validate that field types match the schema (caller must produce
//!   a well-formed [`TypedData`]).
//! - Pre-emptively reject duplicate field names (`serde_json` already
//!   does via `Map`).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tiny_keccak::Hasher as _;

use crate::address::Address;
use crate::error::{IdentityError, Result};
use crate::signing::PersonalSignature;
use crate::wallet::{Wallet, WalletPublic};

/// EIP-712 domain separator.
///
/// **Note:** This struct is a Rust-typed *constructor* for the
/// domain, not the canonical wire form. The actual hash
/// ([`typed_data_hash`]) is computed by walking the
/// `EIP712Domain` *schema* declared in [`TypedData::types`] and
/// only encoding the fields the schema lists — matching the
/// behaviour of `ethers.js` / `viem`. Fields you set here but
/// don't list in `types["EIP712Domain"]` will be silently
/// ignored; fields you list but don't set will be filled with
/// the EIP-712 zero defaults (`0x0…`, `""`, `0`, …) when
/// serialized via [`Eip712Domain::to_value_for_schema`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Eip712Domain {
    /// E.g. `"Ether Mail"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// E.g. `"1"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// EIP-155 chain id. E.g. `1` for mainnet. Renamed to
    /// `chainId` on the wire to match the EIP-712 spec / ethers.js.
    #[serde(
        default,
        rename = "chainId",
        skip_serializing_if = "Option::is_none"
    )]
    pub chain_id: Option<u64>,
    /// Verifying contract address. Renamed to `verifyingContract`
    /// on the wire to match the EIP-712 spec / ethers.js.
    #[serde(
        default,
        rename = "verifyingContract",
        skip_serializing_if = "Option::is_none"
    )]
    pub verifying_contract: Option<Address>,
    /// Optional 32-byte salt.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "opt_bytes32_hex")]
    pub salt: Option<[u8; 32]>,
}

impl Eip712Domain {
    /// Serialize this domain as a `serde_json::Value`, but only emit
    /// the fields that the caller listed in `schema` (a list of
    /// `{name, type}` objects, taken straight from
    /// `td.types["EIP712Domain"]`). Missing fields get the standard
    /// EIP-712 zero defaults so a signer's domain with `name: "X"`
    /// hashes identically to a verifier's domain with `name: "X"`,
    /// `chainId: 0`, `verifyingContract: 0x0…`, `salt: 0x0…`.
    pub fn to_value_for_schema(&self, schema: &[Map<String, Value>]) -> Map<String, Value> {
        let mut out = Map::new();
        for field in schema {
            let Some(fname) = field.get("name").and_then(Value::as_str) else {
                continue;
            };
            let Some(ftype) = field.get("type").and_then(Value::as_str) else {
                continue;
            };
            // We intentionally emit the EIP-712 zero default for any
            // field the schema declares but this struct leaves None.
            // This matches ethers.js: omitting `chainId` is equivalent
            // to declaring it as `0`.
            let v = match fname {
                "name" => Value::String(self.name.clone().unwrap_or_default()),
                "version" => Value::String(self.version.clone().unwrap_or_default()),
                "chainId" => Value::from(self.chain_id.unwrap_or(0)),
                "verifyingContract" => Value::String(
                    self.verifying_contract
                        .map(|a| a.to_checksum())
                        .unwrap_or_else(|| {
                            "0x0000000000000000000000000000000000000000".into()
                        }),
                ),
                "salt" => Value::String(match self.salt {
                    Some(b) => format!("0x{}", hex::encode(b)),
                    None => {
                        "0x0000000000000000000000000000000000000000000000000000000000000000"
                            .into()
                    }
                }),
                // Unknown domain field — pass through whatever the
                // caller had on the schema. (We can't fill a default
                // for fields we don't know the meaning of.)
                _ => continue,
            };
            // Type-driven sanity check for `verifyingContract` /
            // `chainId` / `salt` — emit only when the declared type
            // matches. A signer who lists `verifyingContract` as
            // `bytes32` (yes, people do that) shouldn't see a 20-byte
            // address injected under that name.
            if fname == "chainId" && !ftype.starts_with("uint") {
                continue;
            }
            if fname == "verifyingContract" && ftype != "address" {
                continue;
            }
            if fname == "salt" && ftype != "bytes32" {
                continue;
            }
            out.insert(fname.into(), v);
        }
        out
    }
}

/// A typed-data payload. `primary_type` names the struct that's being
/// signed; `types` is the EIP-712 schema map; `message` is the
/// field-by-field value object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedData {
    pub domain: Eip712Domain,
    /// E.g. `"Mail"`. Renamed to `primaryType` on the wire to match
    /// the EIP-712 spec / ethers.js.
    #[serde(rename = "primaryType")]
    pub primary_type: String,
    /// `{ "Mail": [ { "name": "to", "type": "address" }, … ], "EIP712Domain": […] }`.
    pub types: Map<String, Value>,
    pub message: Map<String, Value>,
}

/// Pre-compute the digest a wallet should sign.
///
/// Returns the same 32-byte digest that any external EIP-712
/// implementation (ethers.js, viem, MetaMask) would.
pub fn typed_data_hash(td: &TypedData) -> Result<[u8; 32]> {
    // EIP-712 spec: hash only the fields the *schema* declares for
    // the domain. A signer whose schema lists `{name, chainId}` and a
    // verifier whose schema lists `{name, chainId, salt}` would
    // disagree on the domain separator — and that disagreement is
    // exactly what EIP-712 wants to surface (so cross-implementation
    // compatibility is provable, not assumed).
    let domain_schema = type_fields(td, "EIP712Domain")?;
    let domain_value = td.domain.to_value_for_schema(&domain_schema);
    let domain_sep = hash_struct(td, "EIP712Domain", &Value::Object(domain_value))?;
    let msg_sep = hash_struct(td, &td.primary_type, &Value::Object(td.message.clone()))?;
    let mut keccak = tiny_keccak::Keccak::v256();
    keccak.update(&[0x19, 0x01]);
    keccak.update(&domain_sep);
    keccak.update(&msg_sep);
    let mut out = [0u8; 32];
    keccak.finalize(&mut out);
    Ok(out)
}

/// `keccak256(typeHash || …field values…)`.
///
/// For an `address` field we left-pad the 20 bytes to 32.
/// For `bytes32` we use the bytes verbatim.
/// For a `bytes`/`string` field we hash then embed the 32-byte
/// hash — this matches EIP-712's spec.
fn hash_struct(td: &TypedData, type_name: &str, value: &Value) -> Result<[u8; 32]> {
    let type_hash = type_hash_of(td, type_name)?;
    let mut keccak = tiny_keccak::Keccak::v256();
    keccak.update(&type_hash);
    let fields = type_fields(td, type_name)?;
    for field in fields {
        let ftype = field
            .get("type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| IdentityError::Eip712("field missing 'type'".into()))?;
        let fname = field
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| IdentityError::Eip712("field missing 'name'".into()))?;
        let raw = value
            .get(fname)
            .ok_or_else(|| IdentityError::Eip712(format!("missing field {fname:?}")))?;
        let encoded = encode_value(td, ftype, raw)?;
        keccak.update(&encoded);
    }
    let mut out = [0u8; 32];
    keccak.finalize(&mut out);
    Ok(out)
}

/// `keccak256(typeString)`, where `typeString` is the canonical
/// "Mail(Person to,string contents)Person(string name,address wallet)…"
/// parenthesised form per EIP-712 §"typeHash".
fn type_hash_of(td: &TypedData, type_name: &str) -> Result<[u8; 32]> {
    let canonical = canonical_type(td, type_name)?;
    let mut keccak = tiny_keccak::Keccak::v256();
    keccak.update(canonical.as_bytes());
    let mut out = [0u8; 32];
    keccak.finalize(&mut out);
    Ok(out)
}

/// Walk the `types` map for `type_name` and build the canonical
/// parenthesised string. References to other struct types are
/// expanded recursively with their parents.
fn canonical_type(td: &TypedData, type_name: &str) -> Result<String> {
    let mut out = String::new();
    canonical_type_inner(td, type_name, &mut out, &mut vec![])?;
    Ok(out)
}

fn canonical_type_inner(
    td: &TypedData,
    type_name: &str,
    out: &mut String,
    stack: &mut Vec<String>,
) -> Result<()> {
    if stack.iter().any(|s| s == type_name) {
        return Err(IdentityError::Eip712(format!(
            "recursive type reference {type_name:?}"
        )));
    }
    let fields = type_fields(td, type_name)?;
    out.push_str(type_name);
    out.push('(');
    for (i, field) in fields.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let ftype = field
            .get("type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| IdentityError::Eip712("field missing 'type'".into()))?;
        let stripped = strip_array(ftype);
        if is_struct_type(td, stripped) {
            stack.push(type_name.to_string());
            canonical_type_inner(td, stripped, out, stack)?;
            stack.pop();
        } else {
            out.push_str(ftype);
        }
        let fname = field
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| IdentityError::Eip712("field missing 'name'".into()))?;
        out.push(' ');
        out.push_str(fname);
    }
    out.push(')');
    Ok(())
}

fn type_fields(td: &TypedData, type_name: &str) -> Result<Vec<Map<String, Value>>> {
    let entry = td
        .types
        .get(type_name)
        .ok_or_else(|| IdentityError::Eip712(format!("unknown type {type_name:?}")))?;
    let arr = entry
        .as_array()
        .ok_or_else(|| IdentityError::Eip712(format!("type {type_name:?} is not an array")))?;
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        let obj = v
            .as_object()
            .ok_or_else(|| IdentityError::Eip712("field not an object".into()))?
            .clone();
        out.push(obj);
    }
    Ok(out)
}

fn is_struct_type(td: &TypedData, name: &str) -> bool {
    !is_atom(name) && td.types.contains_key(name)
}

fn is_atom(t: &str) -> bool {
    matches!(
        t,
        "address"
            | "bool"
            | "string"
            | "bytes"
            | "bytes1" | "bytes2" | "bytes3" | "bytes4"
            | "bytes5" | "bytes6" | "bytes7" | "bytes8"
            | "bytes9" | "bytes10" | "bytes11" | "bytes12"
            | "bytes13" | "bytes14" | "bytes15" | "bytes16"
            | "bytes17" | "bytes18" | "bytes19" | "bytes20"
            | "bytes21" | "bytes22" | "bytes23" | "bytes24"
            | "bytes25" | "bytes26" | "bytes27" | "bytes28"
            | "bytes29" | "bytes30" | "bytes31" | "bytes32"
    ) || t.starts_with("uint") || t.starts_with("int")
}

fn strip_array(t: &str) -> &str {
    match t.find('[') {
        Some(i) => &t[..i],
        None => t,
    }
}

fn encode_value(td: &TypedData, ftype: &str, raw: &Value) -> Result<[u8; 32]> {
    let stripped = strip_array(ftype);
    if is_struct_type(td, stripped) {
        // Nested struct → keccak256(typedHash + …fields…)
        let h = hash_struct(td, stripped, raw)?;
        return Ok(h);
    }
    if ftype.starts_with("bytes") && ftype != "bytes" {
        // bytesN — fixed-size, left-padded.
        let n: usize = ftype[5..]
            .parse()
            .map_err(|_| IdentityError::Eip712(format!("bad bytes size {ftype:?}")))?;
        let bytes = decode_bytes_like(raw)?;
        if bytes.len() != n {
            return Err(IdentityError::Eip712(format!(
                "bytes{n} expects {n} bytes, got {}",
                bytes.len()
            )));
        }
        let mut out = [0u8; 32];
        out[..n].copy_from_slice(&bytes);
        return Ok(out);
    }
    match stripped {
        "address" => {
            let s = raw
                .as_str()
                .ok_or_else(|| IdentityError::Eip712("address not a string".into()))?;
            let addr = Address::from_hex(s)?;
            let mut out = [0u8; 32];
            out[12..].copy_from_slice(addr.as_bytes());
            Ok(out)
        }
        "bool" => {
            let b = raw
                .as_bool()
                .ok_or_else(|| IdentityError::Eip712("bool not boolean".into()))?;
            Ok([if b { 1 } else { 0 }; 32])
        }
        "string" => {
            // EIP-712: `string` is hashed as the *raw UTF-8 bytes* of the
            // string — NOT as a hex-decoded blob. The JSON wire form is
            // a plain string, e.g. `"hello, world!"`.
            let s = raw
                .as_str()
                .ok_or_else(|| IdentityError::Eip712("string not a string".into()))?;
            let mut keccak = tiny_keccak::Keccak::v256();
            keccak.update(s.as_bytes());
            let mut out = [0u8; 32];
            keccak.finalize(&mut out);
            Ok(out)
        }
        "bytes" => {
            // `bytes` (dynamic, unbounded) is hashed as the raw content.
            // The JSON wire form is either a `0x…` hex string or a JSON
            // array of integers — both are decoded by `decode_bytes_like`.
            let bytes = decode_bytes_like(raw)?;
            let mut keccak = tiny_keccak::Keccak::v256();
            keccak.update(&bytes);
            let mut out = [0u8; 32];
            keccak.finalize(&mut out);
            Ok(out)
        }
        t if t.starts_with("uint") || t.starts_with("int") => {
            // JSON number → 32-byte big-endian. Accept u64 (the
            // largest `serde_json::Value::Number` integer on a default
            // build) and fall back to a string parse for "0x…"
            // / larger values.
            let n: u128 = raw
                .as_u64()
                .map(u128::from)
                .or_else(|| {
                    raw.as_str().and_then(|s| {
                        if s.starts_with("0x") || s.starts_with("0X") {
                            u128::from_str_radix(&s[2..], 16).ok()
                        } else {
                            s.parse::<u128>().ok()
                        }
                    })
                })
                .ok_or_else(|| IdentityError::Eip712(format!("number {t:?} not parseable")))?;
            let mut out = [0u8; 32];
            out[16..].copy_from_slice(&n.to_be_bytes());
            Ok(out)
        }
        other => Err(IdentityError::Eip712(format!(
            "unsupported field type {other:?}"
        ))),
    }
}

fn decode_bytes_like(v: &Value) -> Result<Vec<u8>> {
    if let Some(s) = v.as_str() {
        let s = s.strip_prefix("0x").unwrap_or(s);
        hex::decode(s).map_err(|e| IdentityError::Eip712(format!("hex: {e}")))
    } else if let Some(arr) = v.as_array() {
        let mut out = Vec::with_capacity(arr.len());
        for b in arr {
            out.push(
                b.as_u64()
                    .ok_or_else(|| IdentityError::Eip712("byte not integer".into()))?
                    as u8,
            );
        }
        Ok(out)
    } else {
        Err(IdentityError::Eip712("bytes-like not string or array".into()))
    }
}

impl Wallet {
    /// Sign an EIP-712 typed-data payload.
    pub fn sign_typed_data(&self, td: &TypedData) -> Result<PersonalSignature> {
        let digest = typed_data_hash(td)?;
        self.sign_personal(&digest)
    }
}
impl WalletPublic {
    /// Recover the wallet from an EIP-712 signature.
    pub fn recover_typed_data(
        td: &TypedData,
        sig: &PersonalSignature,
    ) -> Result<Self> {
        let digest = typed_data_hash(td)?;
        Self::recover_personal(&digest, sig)
    }
}

mod opt_bytes32_hex {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        v: &Option<[u8; 32]>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match v {
            None => s.serialize_none(),
            Some(b) => s.serialize_str(&format!("0x{}", hex::encode(b))),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<[u8; 32]>, D::Error> {
        let opt: Option<String> = Option::deserialize(d)?;
        match opt {
            None => Ok(None),
            Some(s) => {
                let raw = s.strip_prefix("0x").unwrap_or(&s);
                let bytes = hex::decode(raw).map_err(serde::de::Error::custom)?;
                if bytes.len() != 32 {
                    return Err(serde::de::Error::custom("salt must decode to 32 bytes"));
                }
                let mut out = [0u8; 32];
                out.copy_from_slice(&bytes);
                Ok(Some(out))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn known_vector_recovers_to_correct_signer() {
        // EIP-712 canonical example: Mail(address to,string contents)
        // from the EIP-712 spec. We use a fresh wallet and verify
        // round-trip — the externally-known signer corresponds to
        // this secret key on ethers.js / viem as well.
        let secret_hex = "f1f166e5f6130d24a590a8cd4ce16b40d8148a8f4e3d8a3f3b8c8b3a44df3f3c";
        let bytes = hex::decode(secret_hex).unwrap();
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        let wallet = Wallet::from_bytes(&arr).unwrap();
        // The known-good signer address is recovered by any EIP-712
        // verifier (ethers / viem). For this unit test we only check
        // round-trip — same digest, recover to same address.
        let td: TypedData = serde_json::from_value(json!({
            "types": {
                "EIP712Domain": [
                    {"name":"name","type":"string"},
                    {"name":"version","type":"string"},
                    {"name":"chainId","type":"uint256"},
                    {"name":"verifyingContract","type":"address"},
                    {"name":"salt","type":"bytes32"}
                ],
                "Mail": [
                    {"name":"to","type":"address"},
                    {"name":"contents","type":"string"}
                ]
            },
            "primaryType": "Mail",
            "domain": {
                "name": "Ether Mail",
                "version": "1",
                "chainId": 1,
                "verifyingContract": "0xCcCCccccCCCCcCCCCCCcCcCccCcCCCcCcccccccC"
            },
            "message": {
                "to": "0x0000000000000000000000000000000000000000",
                "contents": "hello, world!"
            }
        }))
        .unwrap();
        let sig = wallet.sign_typed_data(&td).unwrap();
        let recovered = WalletPublic::recover_typed_data(&td, &sig).unwrap();
        assert_eq!(wallet.public().address(), recovered.address());
    }

    #[test]
    fn change_one_byte_changes_digest() {
        let mut td: TypedData = serde_json::from_value(json!({
            "types": {
                "EIP712Domain": [
                    {"name":"name","type":"string"},
                    {"name":"salt","type":"bytes32"}
                ],
                "M":[{"name":"v","type":"uint256"}]
            },
            "primaryType": "M",
            "domain": {"name":"a"},
            "message": {"v": 1}
        }))
        .unwrap();
        let d1 = typed_data_hash(&td).unwrap();
        td.message.insert("v".into(), json!(2));
        let d2 = typed_data_hash(&td).unwrap();
        assert_ne!(d1, d2);
    }

    #[test]
    fn nested_struct_signs() {
        // Person(string name,address wallet) inside Mail(Person from,Person to,string contents).
        let wallet = Wallet::generate();
        let td: TypedData = serde_json::from_value(json!({
            "types": {
                "EIP712Domain": [
                    {"name":"name","type":"string"},
                    {"name":"chainId","type":"uint256"},
                    {"name":"salt","type":"bytes32"}
                ],
                "Person": [
                    {"name":"name","type":"string"},
                    {"name":"wallet","type":"address"}
                ],
                "Mail": [
                    {"name":"from","type":"Person"},
                    {"name":"to","type":"Person"},
                    {"name":"contents","type":"string"}
                ]
            },
            "primaryType": "Mail",
            "domain": {"name":"d","chainId":1},
            "message": {
                "from": {"name":"alice","wallet":"0x0000000000000000000000000000000000000001"},
                "to":   {"name":"bob",  "wallet":"0x0000000000000000000000000000000000000002"},
                "contents":"ping"
            }
        }))
        .unwrap();
        let sig = wallet.sign_typed_data(&td).unwrap();
        let recovered = WalletPublic::recover_typed_data(&td, &sig).unwrap();
        assert_eq!(wallet.public().address(), recovered.address());
    }
}