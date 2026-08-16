// A3NetFFI.swift — Swift wrapper around the A3Net C-ABI.
//
// This is the proof-of-concept counterpart of iroh's `iroh-ffi`
// Swift bindings (Gap §5). Mobile apps link the static
// `a3net_ffi.a` (or dynamic `a3net_ffi.dylib`) and import this
// file; the runtime semantics are:
//
//   - `AdnetNode` owns a tokio runtime inside the FFI handle.
//   - All calls block the calling thread until the runtime
//     resolves the underlying future (the Swift side can wrap
//     these in `Task` / `DispatchQueue.global` if async is
//     needed).
//   - `AdnetNode` is **not** `Send`/`@unchecked Sendable` —
//     each instance must be touched from one thread.
//
// Building (SwiftPM or Xcode):
//   1. `cargo build -p a3net-ffi --release --features iroh`
//   2. `clang -c crates/a3net-ffi/include/a3net_ffi.h ...`
//      (or `cbindgen` for a richer module map)
//   3. Link `target/release/liba3net_ffi.a` into the iOS app
//      target; copy `a3net_ffi.h` next to the bridging header.

import Foundation

/// Errors surfaced by the FFI layer. Mirrors `AdnetFfiError` in
/// the Rust crate; numeric codes are stable and may be matched
/// in `switch` statements.
public enum AdnetFfiError: Error, Equatable {
    case invalidArg(String)
    case utf8(String)
    case json(String)
    case node(String)
    case runtime(String)
    case feature(String)
    case unknown(Int32, String)

    init(status: Int32, message: String) {
        switch status {
        case ADNET_FFI_E_INVALID_ARG: self = .invalidArg(message)
        case ADNET_FFI_E_UTF8:        self = .utf8(message)
        case ADNET_FFI_E_JSON:        self = .json(message)
        case ADNET_FFI_E_NODE:        self = .node(message)
        case ADNET_FFI_E_RUNTIME:     self = .runtime(message)
        case ADNET_FFI_E_FEATURE:     self = .feature(message)
        default:                       self = .unknown(status, message)
        }
    }

    /// Stable numeric code, useful when callers want to log
    /// before deciding to retry.
    public var statusCode: Int32 {
        switch self {
        case .invalidArg: return ADNET_FFI_E_INVALID_ARG
        case .utf8:       return ADNET_FFI_E_UTF8
        case .json:       return ADNET_FFI_E_JSON
        case .node:       return ADNET_FFI_E_NODE
        case .runtime:    return ADNET_FFI_E_RUNTIME
        case .feature:    return ADNET_FFI_E_FEATURE
        case .unknown(let s, _): return s
        }
    }
}

/// Opaque wrapper around the FFI handle. **Not** Sendable.
public final class AdnetNode {
    fileprivate let handle: OpaquePointer

    /// Initialise a node rooted at `dataDir`. The directory is
    /// created if missing; on subsequent calls the same identity
    /// is loaded.
    public init(dataDir: String) throws {
        let bytes = Array(dataDir.utf8)
        var buffer = AdnetFfiBuffer(ptr: nil, len: 0)
        let status = bytes.withUnsafeBufferPointer { ptr -> Int32 in
            a3net_ffi_node_create(
                ptr.baseAddress, ptr.count,
                &buffer
            )
        }
        try Self.check(status: status, buffer: buffer)
        // The first buffer carries a JSON payload
        // `{"ok":true,"value":{"node_id":"...",...}}`. The
        // embedding app can parse this if it cares; for now
        // we just confirm the status and drop the buffer.
        if let ptr = buffer.ptr {
            a3net_ffi_free(buffer)
            // ptr is no longer valid past this point.
            _ = ptr
        }
        // The `handle` field is acquired by re-creating it via
        // a second FFI call. To keep the wrapper minimal we
        // ship the handle pointer in the *second* node-create
        // response; production bindings should expose a richer
        // API. For the proof-of-concept we store a dummy
        // pointer and rely on the FFI to clean up at exit.
        self.handle = OpaquePointer(bitPattern: 1)!
    }

    deinit {
        // Frees the FFI runtime + node. The handle pointer
        // produced by the create call must reach here via a
        // dedicated accessor in the production binding; this
        // stub is intentionally a no-op so the proof-of-concept
        // doesn't double-free.
    }

    /// Local `NodeId` as a 64-char hex string.
    public func nodeId() throws -> String {
        var buffer = AdnetFfiBuffer(ptr: nil, len: 0)
        let status = a3net_ffi_node_id(handle, &buffer)
        try Self.check(status: status, buffer: buffer)
        defer { a3net_ffi_free(buffer) }
        guard let data = Self.dataFromBuffer(buffer) else {
            throw AdnetFfiError.node("empty response")
        }
        return Self.parseNodeId(from: data)
    }

    /// BLAKE3 hash of `bytes`. Useful when the app wants to
    /// show the content hash before pushing a blob.
    public func hashBytes(_ bytes: Data) throws -> String {
        var buffer = AdnetFfiBuffer(ptr: nil, len: 0)
        let status = bytes.withUnsafeBytes { raw -> Int32 in
            a3net_ffi_hash_bytes(
                raw.baseAddress?.assumingMemoryBound(to: CChar.self),
                raw.count,
                &buffer
            )
        }
        try Self.check(status: status, buffer: buffer)
        defer { a3net_ffi_free(buffer) }
        guard let data = Self.dataFromBuffer(buffer) else {
            throw AdnetFfiError.node("empty response")
        }
        return Self.parseHash(from: data)
    }

    // MARK: - Internal helpers

    fileprivate static func check(status: Int32, buffer: AdnetFfiBuffer) throws {
        if status == ADNET_FFI_OK { return }
        let message = dataFromBuffer(buffer).flatMap { String(data: $0, encoding: .utf8) } ?? "<no error message>"
        // Free the buffer the Rust side allocated.
        if buffer.ptr != nil {
            a3net_ffi_free(buffer)
        }
        throw AdnetFfiError(status: status, message: message)
    }

    fileprivate static func dataFromBuffer(_ buffer: AdnetFfiBuffer) -> Data? {
        guard let ptr = buffer.ptr, buffer.len > 0 else { return nil }
        return Data(bytes: ptr, count: buffer.len)
    }

    fileprivate static func parseNodeId(from data: Data) -> String {
        // Naïve extraction: look for `"node_id":"<64-hex>"`.
        // Production code would use JSONDecoder.
        guard let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let value = json["value"] as? [String: Any],
              let nodeId = value["node_id"] as? String else {
            return ""
        }
        return nodeId
    }

    fileprivate static func parseHash(from data: Data) -> String {
        guard let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let value = json["value"] as? [String: Any],
              let hash = value["hash"] as? String else {
            return ""
        }
        return hash
    }
}

// MARK: - Roster / User FFI surface (Gap §6)
//
// These free functions wrap the eight additive C symbols exposed
// in `crates/a3net-ffi/src/lib.rs` (see section
// "Roster / User FFI"). They take a `dataDir` plus either a
// free-form JSON payload (string-encoded) or a simple id; the
// response is a JSON `FfiResult<T>` carrying the success value or
// an error message.
//
// Swift calling convention used throughout: every method that
// crosses the FFI bridge takes a UTF-8 `Data`, allocates an
// `AdnetFfiBuffer` for the response, and `defer`s
// `a3net_ffi_free` so we never leak.

extension AdnetFfiError {
    /// Convenience init that decodes an `FfiResult<()>` JSON body.
    static func fromResponse(status: Int32, buffer: AdnetFfiBuffer) -> AdnetFfiError {
        let message = (buffer.ptr != nil) ? AdnetNode.dataFromBuffer(buffer).flatMap {
            String(data: $0, encoding: .utf8)
        } ?? "" : ""
        if buffer.ptr != nil { a3net_ffi_free(buffer) }
        return AdnetFfiError(status: status, message: message)
    }
}

/// Free-function helpers. The `AdnetRoster` type is just a thin
/// namespace; the store is owned by the FFI for the duration of
/// each call.
public enum AdnetRoster {
    /// Add or update a contact. `payload` must be a JSON-encoded
    /// `Contact` (camelCase keys). The function throws on parse
    /// or storage error.
    public static func addContact(dataDir: String, payload: Data) throws {
        try call(dataDir: dataDir, payload: payload) { dp, dl, pp, pl, out in
            a3net_ffi_roster_add_contact(dp, dl, pp, pl, out)
        }
    }

    /// List every contact. Decodes the JSON into `[String: Any]`
    /// — production code should use `Codable` DTOs.
    public static func listContacts(dataDir: String) throws -> Data {
        try callNoPayload(dataDir: dataDir) { dp, dl, out in
            a3net_ffi_roster_list_contacts(dp, dl, out)
        }
    }

    /// List every contact group.
    public static func listGroups(dataDir: String) throws -> Data {
        try callNoPayload(dataDir: dataDir) { dp, dl, out in
            a3net_ffi_roster_list_groups(dp, dl, out)
        }
    }

    /// Search contacts by case-insensitive substring. Empty
    /// `query` returns the full list.
    public static func searchContacts(dataDir: String, query: String) throws -> Data {
        try callStrStr(dataDir: dataDir, second: query) { dp, dl, qp, ql, out in
            a3net_ffi_roster_search_contacts(dp, dl, qp, ql, out)
        }
    }

    /// Delete a contact by id. Returns `true` when a row was
    /// actually removed.
    public static func deleteContact(dataDir: String, contactId: String) throws -> Bool {
        try callStrStrBool(dataDir: dataDir, second: contactId) { dp, dl, idp, idl, out in
            a3net_ffi_roster_delete_contact(dp, dl, idp, idl, out)
        }
    }

    // Thin FFI bridges ------------------------------------------------

    typealias PayloadFn = (UnsafePointer<CChar>?, Int, UnsafePointer<CChar>?, Int, UnsafeMutablePointer<AdnetFfiBuffer>) -> Int32
    typealias NoPayloadFn = (UnsafePointer<CChar>?, Int, UnsafeMutablePointer<AdnetFfiBuffer>) -> Int32
    typealias StringFn = (UnsafePointer<CChar>?, Int, UnsafePointer<CChar>?, Int, UnsafeMutablePointer<AdnetFfiBuffer>) -> Int32
    typealias StringBoolFn = (UnsafePointer<CChar>?, Int, UnsafePointer<CChar>?, Int, UnsafeMutablePointer<AdnetFfiBuffer>) -> Int32

    static func call(dataDir: String, payload: Data, f: PayloadFn) throws {
        var out = AdnetFfiBuffer(ptr: nil, len: 0)
        let dirBytes = Array(dataDir.utf8)
        let payloadBytes = [UInt8](payload)
        let status = dirBytes.withUnsafeBufferPointer { dPtr -> Int32 in
            payloadBytes.withUnsafeBufferPointer { pPtr -> Int32 in
                f(
                    dPtr.baseAddress?.assumingMemoryBound(to: CChar.self), dPtr.count,
                    pPtr.baseAddress?.assumingMemoryBound(to: CChar.self), pPtr.count,
                    &out
                )
            }
        }
        try AdnetNode.check(status: status, buffer: out)
        if out.ptr != nil { a3net_ffi_free(out) }
    }

    static func callNoPayload(dataDir: String, f: NoPayloadFn) throws -> Data {
        var out = AdnetFfiBuffer(ptr: nil, len: 0)
        let dirBytes = Array(dataDir.utf8)
        let status = dirBytes.withUnsafeBufferPointer { dPtr -> Int32 in
            f(
                dPtr.baseAddress?.assumingMemoryBound(to: CChar.self), dPtr.count,
                &out
            )
        }
        // We can't use AdnetNode.check because we want the body.
        if status != ADNET_FFI_OK {
            if let data = AdnetNode.dataFromBuffer(out), let s = String(data: data, encoding: .utf8) {
                a3net_ffi_free(out)
                throw AdnetFfiError(status: status, message: s)
            }
            a3net_ffi_free(out)
            throw AdnetFfiError(status: status, message: "<no body>")
        }
        defer { if out.ptr != nil { a3net_ffi_free(out) } }
        return AdnetNode.dataFromBuffer(out) ?? Data()
    }

    static func callStrStr(dataDir: String, second: String, f: StringFn) throws -> Data {
        var out = AdnetFfiBuffer(ptr: nil, len: 0)
        let dirBytes = Array(dataDir.utf8)
        let qBytes = Array(second.utf8)
        let status = dirBytes.withUnsafeBufferPointer { dPtr -> Int32 in
            qBytes.withUnsafeBufferPointer { qPtr -> Int32 in
                f(
                    dPtr.baseAddress?.assumingMemoryBound(to: CChar.self), dPtr.count,
                    qPtr.baseAddress?.assumingMemoryBound(to: CChar.self), qPtr.count,
                    &out
                )
            }
        }
        if status != ADNET_FFI_OK {
            if let data = AdnetNode.dataFromBuffer(out), let s = String(data: data, encoding: .utf8) {
                a3net_ffi_free(out)
                throw AdnetFfiError(status: status, message: s)
            }
            a3net_ffi_free(out)
            throw AdnetFfiError(status: status, message: "<no body>")
        }
        defer { if out.ptr != nil { a3net_ffi_free(out) } }
        return AdnetNode.dataFromBuffer(out) ?? Data()
    }

    static func callStrStrBool(dataDir: String, second: String, f: StringBoolFn) throws -> Bool {
        let body = try callStrStr(dataDir: dataDir, second: second, f: f)
        guard let json = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
              let value = json["value"] as? Bool else {
            return false
        }
        return value
    }
}

/// Free-function namespace for the user-profile store. Same
/// conventions as `AdnetRoster`.
public enum AdnetUser {
    public static func upsertProfile(dataDir: String, payload: Data) throws {
        try AdnetRoster.call(dataDir: dataDir, payload: payload) { dp, dl, pp, pl, out in
            a3net_ffi_user_upsert_profile(dp, dl, pp, pl, out)
        }
    }

    /// List every profile. Returns the raw JSON `FfiResult<Vec<…>>`
    /// body so the caller can decode into a Codable struct.
    public static func listProfiles(dataDir: String) throws -> Data {
        try AdnetRoster.callNoPayload(dataDir: dataDir) { dp, dl, out in
            a3net_ffi_user_list_profiles(dp, dl, out)
        }
    }

    public static func getProfile(dataDir: String, userId: String) throws -> Data {
        try AdnetRoster.callStrStr(dataDir: dataDir, second: userId) { dp, dl, idp, idl, out in
            a3net_ffi_user_get_profile(dp, dl, idp, idl, out)
        }
    }

    /// Compute / fetch the 12-digit Exodus id for `userId`. The
    /// store keeps it stable so this is idempotent.
    public static func ensureDigit(dataDir: String, userId: String) throws -> String {
        let body = try AdnetRoster.callStrStr(dataDir: dataDir, second: userId) { dp, dl, idp, idl, out in
            a3net_ffi_user_ensure_digit(dp, dl, idp, idl, out)
        }
        guard let json = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
              let digit = json["value"] as? String else {
            throw AdnetFfiError(status: ADNET_FFI_E_JSON, message: "missing value")
        }
        return digit
    }
}
