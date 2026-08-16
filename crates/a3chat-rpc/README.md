# `a3chat-rpc`

> JSON-RPC 2.0 server, owner 多路复用通知总线。
>
> **Endpoints**:
>
> - `POST /rpc` — JSON-RPC 2.0 calls (one per request).
> - `GET /rpc/stream` — Server-Sent Events for the authenticated owner.
> - `GET /rpc/health` — Liveness probe.
> - `GET /rpc/version` — Build info.
> - `POST /rpc/notify` — server-side push (internal).
>
> **Methods**: every constant in [`a3chat_core::rpc::A3chatRpcMethod`] is dispatched to the matching [`a3chat_app`] service. The handler is a thin wrapper around [`A3chatApp::dispatch`](a3chat_app::A3chatApp::dispatch).
>
> **Authentication**: owner identity is supplied via the `X-A3Chat-Owner` header (per P0 design). P1 will swap this for Noise_XX-authenticated token exchange.

## Module

| Module | 内容 |
|---|---|
| `error` | `RpcError` — JSON-RPC 2.0 standard error codes + `a3chat-*` extras |
| `server` | `RpcServer` (axum) + builder + lifecycle |
| `dispatch` | `dispatch_rpc_call` — the function the axum handler calls |
| `sse` | `sse_handler` — wires `NotificationBus` onto an SSE stream |
