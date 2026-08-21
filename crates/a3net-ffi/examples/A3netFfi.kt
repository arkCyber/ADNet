package a3net.ffi

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import java.nio.charset.StandardCharsets

/**
 * A3NetFfi — Kotlin wrapper around the A3Net C-ABI.
 *
 * Proof-of-concept counterpart of iroh's `iroh-ffi` Kotlin
 * bindings (Gap §5). Android apps link the shared library
 * `liba3net_ffi.so` (built with `cargo build -p a3net-ffi
 * --release --features iroh`) and call into this object.
 *
 * Threading model mirrors the Swift wrapper: each
 * `A3netFfiHandle` owns a tokio runtime and is **not**
 * thread-safe. Wrap calls in a single-threaded executor
 * (`Executors.newSingleThreadExecutor()`) if the Android
 * lifecycle demands it.
 */
object A3NetFfi {
    init {
        System.loadLibrary("a3net_ffi")
    }

    // ─────────────────────────── JNI signatures ───────────────────────────

    @JvmStatic external fun a3net_ffi_version(): UInt
    @JvmStatic external fun a3net_ffi_free(buf: A3netFfiBuffer)
    @JvmStatic external fun a3net_ffi_node_destroy(handle: A3netFfiHandle?): Int

    @JvmStatic external fun a3net_ffi_node_create(
        dataDirPtr: ByteBuffer, dataDirLen: Int,
        out: A3netFfiBuffer
    ): Int
    @JvmStatic external fun a3net_ffi_node_id(
        handle: A3netFfiHandle,
        out: A3netFfiBuffer
    ): Int
    @JvmStatic external fun a3net_ffi_hash_bytes(
        dataPtr: ByteBuffer, dataLen: Int,
        out: A3netFfiBuffer
    ): Int

    // ─── Roster / User store (Gap §6) ───
    //
    // Standalone SQLite-backed stores that do not require a
    // running node. Mobile callers can poke them from a cold
    // start before `nodeCreate` is called.
    @JvmStatic external fun a3net_ffi_roster_add_contact(
        dataDirPtr: ByteBuffer, dataDirLen: Int,
        payloadPtr: ByteBuffer, payloadLen: Int,
        out: A3netFfiBuffer
    ): Int

    @JvmStatic external fun a3net_ffi_roster_list_contacts(
        dataDirPtr: ByteBuffer, dataDirLen: Int,
        out: A3netFfiBuffer
    ): Int

    @JvmStatic external fun a3net_ffi_roster_list_groups(
        dataDirPtr: ByteBuffer, dataDirLen: Int,
        out: A3netFfiBuffer
    ): Int

    @JvmStatic external fun a3net_ffi_roster_search_contacts(
        dataDirPtr: ByteBuffer, dataDirLen: Int,
        queryPtr: ByteBuffer, queryLen: Int,
        out: A3netFfiBuffer
    ): Int

    @JvmStatic external fun a3net_ffi_roster_delete_contact(
        dataDirPtr: ByteBuffer, dataDirLen: Int,
        contactIdPtr: ByteBuffer, contactIdLen: Int,
        out: A3netFfiBuffer
    ): Int

    @JvmStatic external fun a3net_ffi_user_upsert_profile(
        dataDirPtr: ByteBuffer, dataDirLen: Int,
        payloadPtr: ByteBuffer, payloadLen: Int,
        out: A3netFfiBuffer
    ): Int

    @JvmStatic external fun a3net_ffi_user_list_profiles(
        dataDirPtr: ByteBuffer, dataDirLen: Int,
        out: A3netFfiBuffer
    ): Int

    @JvmStatic external fun a3net_ffi_user_get_profile(
        dataDirPtr: ByteBuffer, dataDirLen: Int,
        userIdPtr: ByteBuffer, userIdLen: Int,
        out: A3netFfiBuffer
    ): Int

    @JvmStatic external fun a3net_ffi_user_ensure_digit(
        dataDirPtr: ByteBuffer, dataDirLen: Int,
        userIdPtr: ByteBuffer, userIdLen: Int,
        out: A3netFfiBuffer
    ): Int

    // ─────────────────────────── C-ABI value types ───────────────────────────

    class A3netFfiBuffer(var ptr: ByteBuffer? = null, var len: Int = 0)

    class A3netFfiHandle {
        // Opaque pointer cast through Long. The Rust side keeps
        // the typed `Box<A3netFfiHandle>`; we never dereference
        // on the JVM side.
        internal var rawHandle: Long = 0

        override fun toString(): String = "A3netFfiHandle(raw=0x${rawHandle.toString(16)})"
    }

    /** Numeric codes from the FFI — stable for switch-style use. */
    object Status {
        const val OK = 0
        const val INVALID_ARG = -1
        const val UTF8 = -2
        const val JSON = -3
        const val NODE = -4
        const val RUNTIME = -5
        const val FEATURE = -6
    }

    class A3NetFfiException(val code: Int, message: String) : RuntimeException("a3net-ffi[$code]: $message")

    // ─────────────────────────── Public Kotlin API ───────────────────────────

    /** Library version. The Android build can refuse to load a
     *  library whose version does not match its compile-time
     *  pin (typically read from a `BuildConfig.A3NET_FFI_VERSION`). */
    fun version(): UInt = a3net_ffi_version()

    /** Boot a node rooted at `dataDir`. */
    fun nodeCreate(dataDir: String): A3netFfiHandle {
        val bytes = dataDir.toByteArray(StandardCharsets.UTF_8)
        val out = A3netFfiBuffer()
        val status = bytes.useBytes { ptr, len -> a3net_ffi_node_create(ptr, len, out) }
        checkStatus(status, out)
        // Parse the result to extract the handle pointer. The
        // FFI JSON response carries `value.handle_raw` as a
        // base-10 string of the opaque pointer address.
        val json = readJsonObject(out)
        val handleJson = json["value"]?.jsonObject ?: error("missing value in node_create response")
        val handleRaw = handleJson["handle_raw"]?.jsonPrimitive?.content?.toLongOrNull()
            ?: error("missing handle_raw in node_create response")
        freeBuffer(out)
        return A3netFfiHandle().apply { rawHandle = handleRaw }
    }

    fun nodeId(handle: A3netFfiHandle): String {
        val out = A3netFfiBuffer()
        checkStatus(a3net_ffi_node_id(handle, out), out)
        val json = readJsonObject(out)
        val value = json["value"]?.jsonObject ?: error("missing value")
        val nodeId = value["node_id"]?.jsonPrimitive?.content ?: ""
        freeBuffer(out)
        return nodeId
    }

    fun hashBytes(bytes: ByteArray): String {
        val out = A3netFfiBuffer()
        val status = bytes.useBytes { ptr, len -> a3net_ffi_hash_bytes(ptr, len, out) }
        checkStatus(status, out)
        val json = readJsonObject(out)
        val value = json["value"]?.jsonObject ?: error("missing value")
        val hash = value["hash"]?.jsonPrimitive?.content ?: ""
        freeBuffer(out)
        return hash
    }

    fun nodeDestroy(handle: A3netFfiHandle?): Int =
        a3net_ffi_node_destroy(handle).also { /* handle is freed inside Rust */ }

    // ─────────────────────────── Roster / User FFI (Gap §6) ───────────────────────────

    /** Add or update a contact. `payload` is a JSON-encoded
     *  `Contact` (camelCase). */
    fun rosterAddContact(dataDir: String, payload: ByteArray) {
        val out = A3netFfiBuffer()
        val status = dataDir.toByteArray(StandardCharsets.UTF_8).useBytes { dp, dl ->
            payload.useBytes { pp, pl -> a3net_ffi_roster_add_contact(dp, dl, pp, pl, out) }
        }
        checkStatus(status, out)
        freeBuffer(out)
    }

    /// List every contact as the raw JSON body. Production code
     *  would decode into a `Contact` DTO. */
    fun rosterListContacts(dataDir: String): String {
        val out = A3netFfiBuffer()
        val status = dataDir.toByteArray(StandardCharsets.UTF_8).useBytes { dp, dl ->
            a3net_ffi_roster_list_contacts(dp, dl, out)
        }
        checkStatus(status, out)
        val body = readJsonText(out)
        freeBuffer(out)
        return body
    }

    /** List every contact group. */
    fun rosterListGroups(dataDir: String): String {
        val out = A3netFfiBuffer()
        val status = dataDir.toByteArray(StandardCharsets.UTF_8).useBytes { dp, dl ->
            a3net_ffi_roster_list_groups(dp, dl, out)
        }
        checkStatus(status, out)
        val body = readJsonText(out)
        freeBuffer(out)
        return body
    }

    /** Search contacts by case-insensitive substring over
     *  name / tags / notes. Empty `query` lists everything. */
    fun rosterSearchContacts(dataDir: String, query: String): String {
        val out = A3netFfiBuffer()
        val status = dataDir.toByteArray(StandardCharsets.UTF_8).useBytes { dp, dl ->
            query.toByteArray(StandardCharsets.UTF_8).useBytes { qp, ql ->
                a3net_ffi_roster_search_contacts(dp, dl, qp, ql, out)
            }
        }
        checkStatus(status, out)
        val body = readJsonText(out)
        freeBuffer(out)
        return body
    }

    /** Delete a contact by id. Throws on storage error;
     *  silently returns when the row was already gone. */
    fun rosterDeleteContact(dataDir: String, contactId: String) {
        val out = A3netFfiBuffer()
        val status = dataDir.toByteArray(StandardCharsets.UTF_8).useBytes { dp, dl ->
            contactId.toByteArray(StandardCharsets.UTF_8).useBytes { idp, idl ->
                a3net_ffi_roster_delete_contact(dp, dl, idp, idl, out)
            }
        }
        checkStatus(status, out)
        freeBuffer(out)
    }

    /** Add or update a user profile (`UserProfile` JSON). */
    fun userUpsertProfile(dataDir: String, payload: ByteArray) {
        val out = A3netFfiBuffer()
        val status = dataDir.toByteArray(StandardCharsets.UTF_8).useBytes { dp, dl ->
            payload.useBytes { pp, pl -> a3net_ffi_user_upsert_profile(dp, dl, pp, pl, out) }
        }
        checkStatus(status, out)
        freeBuffer(out)
    }

    /** List every profile as the raw JSON body. */
    fun userListProfiles(dataDir: String): String {
        val out = A3netFfiBuffer()
        val status = dataDir.toByteArray(StandardCharsets.UTF_8).useBytes { dp, dl ->
            a3net_ffi_user_list_profiles(dp, dl, out)
        }
        checkStatus(status, out)
        val body = readJsonText(out)
        freeBuffer(out)
        return body
    }

    /** Fetch a single profile as a JSON `FfiResult<Option<…>>`.
     *  Decoding into a typed DTO is left to the caller. */
    fun userGetProfile(dataDir: String, userId: String): String {
        val out = A3netFfiBuffer()
        val status = dataDir.toByteArray(StandardCharsets.UTF_8).useBytes { dp, dl ->
            userId.toByteArray(StandardCharsets.UTF_8).useBytes { idp, idl ->
                a3net_ffi_user_get_profile(dp, dl, idp, idl, out)
            }
        }
        checkStatus(status, out)
        val body = readJsonText(out)
        freeBuffer(out)
        return body
    }

    /** Compute / fetch the 12-digit Exodus id for `userId`.
     *  Idempotent — the store persists the mapping. */
    fun userEnsureDigit(dataDir: String, userId: String): String {
        val out = A3netFfiBuffer()
        val status = dataDir.toByteArray(StandardCharsets.UTF_8).useBytes { dp, dl ->
            userId.toByteArray(StandardCharsets.UTF_8).useBytes { idp, idl ->
                a3net_ffi_user_ensure_digit(dp, dl, idp, idl, out)
            }
        }
        checkStatus(status, out)
        val json = readJsonObject(out)
        val digit = json["value"]?.jsonPrimitive?.content
            ?: throw A3NetFfiException(Status.JSON, "missing value in ensure_digit")
        freeBuffer(out)
        return digit
    }

    // ─────────────────────────── Helpers ───────────────────────────

    private fun checkStatus(status: Int, out: A3netFfiBuffer) {
        if (status == Status.OK) return
        val msg = readErrorMessage(out)
        freeBuffer(out)
        throw A3NetFfiException(status, msg)
    }

    private fun readErrorMessage(buf: A3netFfiBuffer): String {
        val obj = runCatching { readJsonObject(buf) }.getOrNull() ?: return "<no error message>"
        return obj["error"]?.jsonPrimitive?.content ?: "<no error message>"
    }

    private fun readJsonObject(buf: A3netFfiBuffer): JsonObject {
        val text = readJsonText(buf)
        return Json.parseToJsonElement(text).jsonObject
    }

    /** Decode the buffer body as UTF-8 text without parsing
     *  it. Useful for endpoints that return a JSON `value`
     *  the caller wants to forward verbatim. */
    private fun readJsonText(buf: A3netFfiBuffer): String {
        val ptr = buf.ptr ?: return ""
        if (buf.len == 0) return ""
        val src = ptr.duplicate().order(java.nio.ByteOrder.nativeOrder())
        val bytes = ByteArray(buf.len)
        src.get(bytes)
        val decoder = StandardCharsets.UTF_8.newDecoder().apply {
            onMalformedInput(CodingErrorAction.REPORT)
            onUnmappableCharacter(CodingErrorAction.REPORT)
        }
        return decoder.decode(java.nio.ByteBuffer.wrap(bytes)).toString()
    }

    private fun freeBuffer(buf: A3netFfiBuffer) {
        if (buf.ptr != null) {
            a3net_ffi_free(buf)
            buf.ptr = null
            buf.len = 0
        }
    }

    /** Run `block` with a heap-backed direct ByteBuffer view of
     *  `bytes`. The buffer is allocated via `ByteBuffer.allocateDirect`
     *  to avoid GC pressure during hot paths. */
    private inline fun <R> ByteArray.useBytes(block: (ByteBuffer, Int) -> R): R {
        val buf = ByteBuffer.allocateDirect(size).order(java.nio.ByteOrder.nativeOrder())
        buf.put(this)
        buf.flip()
        return try {
            block(buf, size)
        } finally {
            // Direct buffer is GC'd; no explicit free needed.
        }
    }
}
