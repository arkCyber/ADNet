/*
 * adnet_ffi.h — C ABI for the ADNet mobile surface (Gap §5).
 *
 * This header is the contract Swift / Kotlin / WASM / Unity
 * embedders consume. The Rust crate `adnet-ffi` produces the
 * matching symbols; cbindgen (or the equivalent of) would
 * generate this file from the Rust source — for now we hand-write
 * it so reviewers can read the C side without a build step.
 *
 * Versioning
 * ----------
 * ADNET_FFI_VERSION is bumped on every breaking ABI change.
 * Embedders should refuse to load a library whose version does
 * not match a compile-time `#define ADNET_FFI_VERSION_MIN` they
 * pin in the build script.
 *
 * Threading
 * ---------
 * Functions are `extern "C"` and **not** thread-safe per handle.
 * Each `adnet_ffi_node_create` produces a handle that owns its
 * own tokio runtime; the embedder is expected to call functions
 * on that handle from a single OS thread. Multiple handles from
 * different threads are fine (each runtime is independent).
 *
 * Memory ownership
 * ----------------
 * Functions that allocate a result buffer (`*_out`) return an
 * `AdnetFfiBuffer { ptr, len }`. The embedder MUST eventually
 * call `adnet_ffi_free(buf)` to release the bytes. Failing to
 * do so leaks. Passing a buffer that was not produced by this
 * library is undefined behaviour.
 *
 * Errors
 * ------
 * Each function returns an `int32_t` status code:
 *   0    OK
 *  -1    INVALID_ARG (NULL pointer / empty input)
 *  -2    UTF8       (input was not valid UTF-8)
 *  -3    JSON       (output encoding failed; should never happen)
 *  -4    NODE       (adnet-node error — see the error buffer)
 *  -5    RUNTIME    (tokio runtime could not be created)
 *  -6    FEATURE    (required build feature is disabled)
 *
 * On non-zero status, `*out` (if non-NULL) holds a JSON-encoded
 * `FfiResult<()>` whose `error` field is human-readable.
 */

#ifndef ADNET_FFI_H
#define ADNET_FFI_H

#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Status codes — must match `AdnetFfiStatus` in lib.rs. */
#define ADNET_FFI_OK              0
#define ADNET_FFI_E_INVALID_ARG  -1
#define ADNET_FFI_E_UTF8         -2
#define ADNET_FFI_E_JSON         -3
#define ADNET_FFI_E_NODE         -4
#define ADNET_FFI_E_RUNTIME      -5
#define ADNET_FFI_E_FEATURE      -6

/* Pinned version. Bump on breaking ABI change. */
#define ADNET_FFI_VERSION 1u

/* C-side opaque result buffer. */
typedef struct AdnetFfiBuffer {
    char *ptr;
    size_t len;
} AdnetFfiBuffer;

/* Forward-declared opaque handle. */
typedef struct AdnetFfiHandle AdnetFfiHandle;

/* ───── Lifecycle ───── */
uint32_t              adnet_ffi_version(void);
void                  adnet_ffi_free(AdnetFfiBuffer buf);
AdnetFfiStatus        adnet_ffi_node_destroy(AdnetFfiHandle *handle);

/* ───── Always-available surface (no feature flag) ───── */
AdnetFfiStatus        adnet_ffi_node_create(
                          const char *data_dir_ptr, size_t data_dir_len,
                          AdnetFfiBuffer *out);
AdnetFfiStatus        adnet_ffi_node_id(
                          AdnetFfiHandle *handle,
                          AdnetFfiBuffer *out);
AdnetFfiStatus        adnet_ffi_hash_bytes(
                          const char *data_ptr, size_t data_len,
                          AdnetFfiBuffer *out);

/* ───── iroh-feature surface (mirrors iroh-ffi; Gap §5) ───── */
#ifdef ADNET_FFI_ENABLE_IROH
AdnetFfiStatus        adnet_ffi_node_addr(
                          AdnetFfiHandle *handle,
                          AdnetFfiBuffer *out);
AdnetFfiStatus        adnet_ffi_dial(
                          AdnetFfiHandle *handle,
                          const char *node_id_ptr, size_t node_id_len,
                          AdnetFfiBuffer *out);
#endif

/* ───── Contact roster (Gap §6) ─────
 *
 * Standalone SQLite-backed stores; mobile callers use these
 * without spinning up an iroh endpoint. All calls take UTF-8
 * `(ptr, len)` JSON payloads and return JSON-encoded
 * `FfiResult<T>` in `*out`.
 *
 * The `Contact` payload schema matches `adnet_roster::Contact`
 * (camelCase), and `UserProfile` matches
 * `adnet_userstore::UserProfile`.
 *
 * Functions are additive — `ADNET_FFI_VERSION` is unchanged.
 */
AdnetFfiStatus        adnet_ffi_roster_add_contact(
                          const char *data_dir_ptr, size_t data_dir_len,
                          const char *payload_ptr, size_t payload_len,
                          AdnetFfiBuffer *out);
AdnetFfiStatus        adnet_ffi_roster_list_contacts(
                          const char *data_dir_ptr, size_t data_dir_len,
                          AdnetFfiBuffer *out);
AdnetFfiStatus        adnet_ffi_roster_list_groups(
                          const char *data_dir_ptr, size_t data_dir_len,
                          AdnetFfiBuffer *out);
AdnetFfiStatus        adnet_ffi_roster_search_contacts(
                          const char *data_dir_ptr, size_t data_dir_len,
                          const char *query_ptr, size_t query_len,
                          AdnetFfiBuffer *out);
AdnetFfiStatus        adnet_ffi_roster_delete_contact(
                          const char *data_dir_ptr, size_t data_dir_len,
                          const char *contact_id_ptr, size_t contact_id_len,
                          AdnetFfiBuffer *out);

/* ───── User-profile store (Gap §6) ───── */
AdnetFfiStatus        adnet_ffi_user_upsert_profile(
                          const char *data_dir_ptr, size_t data_dir_len,
                          const char *payload_ptr, size_t payload_len,
                          AdnetFfiBuffer *out);
AdnetFfiStatus        adnet_ffi_user_list_profiles(
                          const char *data_dir_ptr, size_t data_dir_len,
                          AdnetFfiBuffer *out);
AdnetFfiStatus        adnet_ffi_user_get_profile(
                          const char *data_dir_ptr, size_t data_dir_len,
                          const char *user_id_ptr, size_t user_id_len,
                          AdnetFfiBuffer *out);
AdnetFfiStatus        adnet_ffi_user_ensure_digit(
                          const char *data_dir_ptr, size_t data_dir_len,
                          const char *user_id_ptr, size_t user_id_len,
                          AdnetFfiBuffer *out);

#ifdef __cplusplus
}
#endif

#endif /* ADNET_FFI_H */
