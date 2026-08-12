# `adnet-qr`

> QR-code generator, scanner, and chatmail-compatible payload parser for ADNet.
> Encodes `mailto:`, `MATMSG:`, `VCARD`, `DCACCOUNT`, `DCLOGIN`, `DCBACKUP`,
> `OPENPGP4FPR:`, `adnet-pairing://`, and ad-hoc text — and renders them as
> Delta-Chat-compatible SVG via `qrcodegen`.

## 概览 (Overview)

`adnet-qr` is the on-the-wire QR layer for ADNet. It sits in two places:

1. **Generic QR rendering/parsing.** Pure-data classification of a raw
   QR string into a typed [`QrPayload`] enum, plus a self-contained SVG
   generator that uses the same `qrcodegen` library chatmail@core does.
2. **ADNet-specific payloads.** Adds `adnet-peer://`, `adnet-blob://`,
   `adnet-signed-peer://`, `adnet-token://`, and `adnet-pairing://` URLs
   so ADNet nodes can share peer tickets, blob tickets, signed peer
   tickets, relay-payment pledges, and pairing invitations through the
   same camera / display UX.

The crate is intentionally minimal: it does **not** decode scanned
images (the caller supplies the bitmap) and it does **not** talk to
chatmail@core. The implementation is a clean-room port of the public URI
scheme specs. License is MPL-2.0 to match upstream — see `LICENSE-MPL-2.0`.

## 特性 (Features)

- Parses the chatmail-compatible family: `mailto:`, `MATMSG:`, `SMTP:`,
  `BEGIN:VCARD`, `DCACCOUNT:`, `DCLOGIN:`, `DCBACKUP*`, `OPENPGP4FPR:`
  (and `https://i.delta.chat/#…`), `mailto:`, `socks5://`, `https://t.me/socks?…`,
  `ss://`, plain `http(s)://`, and free-form text.
- ADNet-native payload variants: `adnet-peer://`, `adnet-addr://`,
  `adnet-blob://`, `adnet-signed-peer://`, `adnet-token://`,
  `adnet-pairing://`.
- Self-contained SVG renderer (`generator::create_qr_svg`) with style
  customization (`QrStyle`) and a chatmail-style card variant
  (`generator::create_qr_card_svg`).
- Optional `mail` feature: convert a `DCLOGIN` payload into an
  `adnet_mail::Account` so a UI flow can dial SMTP/IMAP without
  re-implementing the parser.
- Credential redaction: `Debug`, `Display`, and `Serialize` of `DCLOGIN`
  and `DCBACKUP` payloads never leak passwords or auth tokens (call
  `expose_secrets()` / `expose_auth_token()` to deliberately re-expose).
- Round-trip stable: every parsed payload can be re-encoded into the
  canonical QR string and re-parsed to the same value.

## 安装 (Installation)

```toml
[dependencies]
adnet-qr = { workspace = true }
```

Available optional features:

| feature      | default | description                                                |
|--------------|---------|------------------------------------------------------------|
| `adnet-types`| ✅      | enables `adnet-peer://`, `adnet-blob://`, `adnet-signed-peer://` |
| `adnet-token`| ✅      | enables `adnet-token://` (relay-payment pledges)            |
| `pairing`    | ✅      | enables `adnet-pairing://` envelopes                       |
| `mail`       | ❌      | adds `DCLOGIN → adnet_mail::Account` conversion            |

## 使用 (Usage)

### Parse a raw QR string

```rust
use adnet_qr::{check_qr, QrPayload};

let raw = "mailto:alice@example.com?subject=Hi&body=Hello%20there";
let parsed = check_qr(raw).unwrap();
assert!(matches!(parsed, QrPayload::Email { .. }));
```

### Render as SVG

```rust
use adnet_qr::generator;

let svg = generator::create_qr_svg("https://adnet.example/invite/abc").unwrap();
assert!(svg.starts_with("<svg"));

// Or a chatmail-style card with a description:
let card = generator::create_qr_card_svg(raw, "Scan with Delta Chat").unwrap();
```

### Round-trip a payload

```rust
use adnet_qr::{check_qr, scan::encode_qr};

let parsed = check_qr(raw).unwrap();
let encoded = encode_qr(&parsed).unwrap();
let reparsed = check_qr(&encoded).unwrap();
assert_eq!(parsed, reparsed);
```

### Encode a custom SVG

```rust
use adnet_qr::generator::{create_qr_svg_with_style, QrStyle, QrErrorCorrectionLevel};

let style = QrStyle {
    canvas_size: 256,
    qr_size: 200,
    fg: "#112233",
    bg: "#ffeeaa",
    ecc: QrErrorCorrectionLevel::Medium,
};
let svg = create_qr_svg_with_style(raw, &style).unwrap();
```

## 应用案例 (Use Cases / Examples)

1. **Share a peer ticket via QR code** — call `PeerTicket::encode(...)`,
   feed it into `generator::create_qr_svg`, and ship the resulting SVG.
   On the other side, the scanner receives the raw QR string from the
   camera, `check_qr(&str)` classifies it as `QrPayload::AdnetPeer`,
   and the UI can hand the inner `PeerTicket` to the connection
   layer. Demonstrated in `examples/qr_basic.rs`.

2. **Show a chatmail-account QR** — render a `dclogin://` URL as a
   `qr_card_svg` with a short description (“Scan to register
   `alice@chat.example.com`”), without needing the chatmail daemon
   in-process. Demonstrated in `examples/qr_round_trip.rs`.

3. **Render and parse `adnet-pairing://` invitations** — combine
   `adnet-pairing::SignedInvitation` with `adnet-pairing::wire::PairingInvitation::to_url`
   and `adnet_qr::generator::create_qr_svg`. The resulting SVG travels
   over chat / email and a peer can decode it with `check_qr` → parse
   `QrPayload::AdnetPairing` → `PairingInvitation::decode()`.
   Demonstrated in `examples/qr_round_trip.rs` and the
   `adnet-invite` crate's `complete_invite_workflow` example.

## 许可

Dual-licensed under MIT OR Apache-2.0 (matching `qrcodegen`).
The bundled `LICENSE-MPL-2.0` covers the original `qrcodegen` source
under MPL-2.0.
