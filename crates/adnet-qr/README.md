# `adnet-qr`

> QR-code generator and scanner for ADNet pairing invitations,
> share tickets, and `dclogin://` scheme strings. Pure Rust,
> no system-level image processing — the scanner side uses the
> host process's stdin / a file path; encoding is done via
> `qrcodegen`.

## Modules

| module             | purpose                                       |
|--------------------|-----------------------------------------------|
| `lib`              | re-exports + `QrError`                        |
| `generator`        | encode text → SVG / PNG / ASCII matrix        |
| `scan`             | decode text (host supplies the bitmap)        |
| `payload`          | typed `QrPayload` enum (peer / ticket / …)    |
| `adnet`            | ADNet-specific invite payload                 |
| `chatmail`         | chatmail-account payload                      |
| `dclogin_scheme`   | `dclogin://` URL builder                      |
| `error`            | typed errors                                  |

## Testing

```bash
cargo test -p adnet-qr
```

Includes QR round-trip tests and pairing round-trip tests
across `dclogin://`, chatmail-account, and ADNet invite
schemes.

## License

Dual-licensed under MIT OR Apache-2.0 (matching `qrcodegen`).
The bundled `LICENSE-MPL-2.0` covers the original
`qrcodegen` source under MPL-2.0.