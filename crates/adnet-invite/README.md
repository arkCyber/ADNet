# `adnet-invite`

> Pairing-invite pipeline — text-code generation, mail
> delivery, and pairing-exchange bookkeeping. Composes with
> `adnet-pairing` (which produces / consumes the structured
> `PairingInvitation`) and `adnet-mail` (which actually
> delivers the text code to the recipient's inbox).

## Modules

| module        | purpose                                            |
|---------------|----------------------------------------------------|
| `lib`         | re-exports                                         |
| `mailer`      | SMTP mailer (text-code + invite link delivery)     |
| `error`       | typed errors (`InviteError`)                       |

## Examples

| example                          | purpose                                  |
|----------------------------------|------------------------------------------|
| `send_pairing_invite.rs`         | mint and "send" (mock SMTP) an invite    |
| `email_pairing_exchange.rs`      | bidirectional invite exchange via email  |
| `complete_invite_workflow.rs`    | full Alice→Bob handshake simulation     |

## Testing

```bash
cargo test -p adnet-invite
```

## License

Same as the workspace root.