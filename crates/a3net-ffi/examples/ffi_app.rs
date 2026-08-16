//! Real-world example — call every always-available C-ABI
//! function that does **not** require a booted node, from pure
//! Rust. This is the golden suite the Swift / Kotlin demos are
//! tested against. Works on the default build
//! (`cargo build -p a3net-ffi`) without `--features iroh` or
//! `--features news`.
//!
//! Run with:
//!   cargo run -p a3net-ffi --example ffi_app

use std::ffi::CString;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use a3net_ffi::{
    a3net_ffi_hash_bytes, a3net_ffi_node_destroy, a3net_ffi_roster_add_contact,
    a3net_ffi_roster_delete_contact, a3net_ffi_roster_list_contacts,
    a3net_ffi_roster_list_groups, a3net_ffi_user_ensure_digit, a3net_ffi_user_get_profile,
    a3net_ffi_user_upsert_profile, AdnetFfiBuffer, ADNET_FFI_E_INVALID_ARG, ADNET_FFI_OK,
};

fn main() {
    println!("=== a3net-ffi app example (no node required) ===");

    let tmp = temp_dir("a3net-ffi-app");
    let data_dir = tmp.to_string_lossy().to_string();

    // ─── 1. Hash arbitrary bytes ────────────────────────────────
    // `a3net_ffi_hash_bytes(ptr, len, out)` is the v0 hash
    // shim the embedders call before pushing a blob. We invoke
    // it directly so the Swift / Kotlin parity test stays
    // honest.
    let payload = b"a3net-ffi-app hello world".to_vec();
    let mut out = AdnetFfiBuffer {
        ptr: std::ptr::null_mut(),
        len: 0,
    };
    let status = unsafe {
        a3net_ffi_hash_bytes(
            payload.as_ptr() as *const std::os::raw::c_char,
            payload.len(),
            &mut out,
        )
    };
    assert_eq!(status, ADNET_FFI_OK);
    let body = decode_buffer(out);
    println!("hash_bytes      → {body}");

    // ─── 2. Roster round-trip ───────────────────────────────────
    // JSON-encode a Contact that matches the FFI's expected
    // schema, push it in, list it, search it, delete it.
    let contact = serde_json::json!({
        "contactId": "alice",
        "name": "Alice",
        "contactType": "human",
        "agentDeploymentType": null,
        "agentIds": [],
        "nodeId": "node-alice",
        "groups": [],
        "tags": ["vip"],
        "notes": "met at conf",
        "isFavorite": false,
        "isBlocked": false,
        "createdAt": 0u64,
        "lastContacted": 0u64,
        "contactCount": 0u32,
        "publicAccountId": null,
        "iotDeviceType": null,
        "iotProtocol": null,
        "iotStatus": null,
        "iotLastSeen": null,
        "iotCapabilities": null,
        "iotLocation": null,
    })
    .to_string();

    {
        let mut out = AdnetFfiBuffer {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        let status = unsafe {
            a3net_ffi_roster_add_contact(
                data_dir.as_ptr() as *const _,
                data_dir.len(),
                contact.as_ptr() as *const _,
                contact.len(),
                &mut out,
            )
        };
        assert_eq!(status, ADNET_FFI_OK);
        let body = decode_buffer(out);
        assert!(body.contains("\"ok\":true"));
        println!("roster.add      → {body}");
    }

    {
        let mut out = AdnetFfiBuffer {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        let status = unsafe {
            a3net_ffi_roster_list_contacts(
                data_dir.as_ptr() as *const _,
                data_dir.len(),
                &mut out,
            )
        };
        assert_eq!(status, ADNET_FFI_OK);
        let body = decode_buffer(out);
        assert!(body.contains("\"contactId\":\"alice\""));
        println!("roster.list     → {}", truncate(&body, 80));
    }

    {
        let mut out = AdnetFfiBuffer {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        let status = unsafe {
            a3net_ffi_roster_list_groups(
                data_dir.as_ptr() as *const _,
                data_dir.len(),
                &mut out,
            )
        };
        assert_eq!(status, ADNET_FFI_OK);
        println!("roster.groups   → {}", decode_buffer(out));
    }

    {
        let mut out = AdnetFfiBuffer {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        let status = unsafe {
            a3net_ffi_roster_delete_contact(
                data_dir.as_ptr() as *const _,
                data_dir.len(),
                b"alice".as_ptr() as *const _,
                b"alice".len(),
                &mut out,
            )
        };
        assert_eq!(status, ADNET_FFI_OK);
        println!("roster.delete   → {}", decode_buffer(out));
    }

    // ─── 3. User store round-trip ───────────────────────────────
    let profile = serde_json::json!({
        "userId": "u1",
        "username": "alice",
        "displayName": "Alice",
        "avatar": null,
        "bio": "hi",
        "preferences": {
            "theme": "auto",
            "locale": "en-US",
            "notificationsEnabled": true,
            "readReceiptsEnabled": true,
            "typingIndicatorsEnabled": true,
            "experimentalJson": "{}",
        },
        "createdAt": 0u64,
        "updatedAt": 0u64,
    })
    .to_string();

    {
        let mut out = AdnetFfiBuffer {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        let status = unsafe {
            a3net_ffi_user_upsert_profile(
                data_dir.as_ptr() as *const _,
                data_dir.len(),
                profile.as_ptr() as *const _,
                profile.len(),
                &mut out,
            )
        };
        assert_eq!(status, ADNET_FFI_OK);
        let body = decode_buffer(out);
        assert!(body.contains("\"ok\":true"));
        println!("user.upsert     → {body}");
    }

    {
        let mut out = AdnetFfiBuffer {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        let status = unsafe {
            a3net_ffi_user_get_profile(
                data_dir.as_ptr() as *const _,
                data_dir.len(),
                b"u1".as_ptr() as *const _,
                2,
                &mut out,
            )
        };
        assert_eq!(status, ADNET_FFI_OK);
        let body = decode_buffer(out);
        assert!(body.contains("\"userId\":\"u1\""));
        println!("user.get        → {}", truncate(&body, 80));
    }

    {
        let mut out = AdnetFfiBuffer {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        let status = unsafe {
            a3net_ffi_user_ensure_digit(
                data_dir.as_ptr() as *const _,
                data_dir.len(),
                b"u1".as_ptr() as *const _,
                2,
                &mut out,
            )
        };
        assert_eq!(status, ADNET_FFI_OK);
        let body = decode_buffer(out);
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("json");
        let digit = parsed["value"].as_str().expect("value").to_string();
        assert!(!digit.is_empty(), "digit should be non-empty");
        println!("user.ensure_digit → {digit}");
    }

    // ─── 4. Boundary cases the embedder must handle ─────────────
    // a) `a3net_ffi_node_destroy(NULL)` is a documented no-op.
    let status = unsafe { a3net_ffi_node_destroy(std::ptr::null_mut()) };
    assert_eq!(status, ADNET_FFI_OK);
    println!("destroy(null)   → ok (no-op contract upheld)");

    // b) `bytes_to_string`-style inputs surface as `E_INVALID_ARG`.
    let mut out = AdnetFfiBuffer {
        ptr: std::ptr::null_mut(),
        len: 0,
    };
    let status = unsafe {
        a3net_ffi_user_ensure_digit(std::ptr::null(), 5, b"u1".as_ptr() as *const _, 2, &mut out)
    };
    assert_eq!(status, ADNET_FFI_E_INVALID_ARG);
    println!("null data_dir   → E_INVALID_ARG as documented");

    // Cleanup the temp dir (RAII guard fires on drop too).
    let _ = std::fs::remove_dir_all(&tmp);
}

// ─────────────────────── helpers ───────────────────────

/// RAII tempdir for the example. We avoid pulling in
/// `tempfile` as an example-dep; the path is unique enough
/// (pid + nanos) for `cargo test` parallelism.
struct TempDir {
    path: PathBuf,
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

impl std::ops::Deref for TempDir {
    type Target = std::path::Path;
    fn deref(&self) -> &Self::Target {
        &self.path
    }
}

impl AsRef<std::path::Path> for TempDir {
    fn as_ref(&self) -> &std::path::Path {
        &self.path
    }
}

fn temp_dir(prefix: &str) -> TempDir {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("{prefix}-{}-{}", std::process::id(), nanos));
    std::fs::create_dir_all(&path).expect("create tempdir");
    TempDir { path }
}

fn decode_buffer(buf: AdnetFfiBuffer) -> String {
    // The FFI contract is: `ptr` was produced by `CString::into_raw`,
    // `len` bytes are valid UTF-8. We copy into Rust memory, then
    // free the C string with `CString::from_raw` to avoid leaking.
    let s = unsafe {
        let slice = std::slice::from_raw_parts(buf.ptr as *const u8, buf.len);
        std::str::from_utf8(slice).expect("utf-8").to_string()
    };
    let _ = unsafe { CString::from_raw(buf.ptr) };
    s
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}
