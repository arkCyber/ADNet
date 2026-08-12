# `adnet-webtransport`

WebTransport (HTTP/3) transport for ADNet. Browser-friendly, symmetric
between native and browser, built on `wtransport`.

## Status (Round-1)

| Component | State |
|-----------|-------|
| `WebTransportConfig` (serde, defaults) | ✅ |
| `WebTransportError` | ✅ |
| `ConnectToken` (HMAC-SHA256 signed) | ✅ + 5 tests |
| `wt_server::WtServer::bind` (real `wtransport` wiring) | ⏳ Round-2 |
| `wt_client::WtClient::connect` | ⏳ Round-2 |
| `adnet-transport::webtransport::WebTransportAdapter` | ⏳ Round-2 |
| Browser demo | ⏳ Round-3 |

## Features

- `default = []` — types + connect-token. No `wtransport` dep.
- `webtransport = ["dep:wtransport"]` — full runtime.

## Connect-token format

```
adnet-wt-v1:<base64url(payload_json)> \x00 <base64url(hmac_sha256(secret, payload_json))>
```

`payload_json`:
```json
{ "nodeId": "<hex>", "issuedAt": <unix_seconds>, "ttlSeconds": 60 }
```

Inspect any token by base64-decoding the payload half — useful when
debugging authorization issues in the browser console.
