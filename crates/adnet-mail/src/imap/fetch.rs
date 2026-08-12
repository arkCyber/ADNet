//! Fetch decoded messages from the selected folder.
//!
//! Pulled from `chatmail@core/src/imap.rs` `Imap::fetch_new_messages`
//! path: search for `\Unseen` UIDs, FETCH them in one round-trip,
//! decode each into our [`crate::mime::Mail`], and expose the raw
//! `UID` / `SEQ` so the caller can mark / delete them.

use async_imap::types::Flag;
use futures::StreamExt;

use crate::error::{MailError, Result};
use crate::imap::ImapSession;
use crate::mime::Mail;

/// One message returned by the IMAP FETCH.
#[derive(Debug, Clone)]
pub struct FetchedMessage {
    /// IMAP UID of the message (stable across folder renames).
    pub uid: u32,
    /// Sequence number within the current mailbox (changes on delete).
    pub seq: u32,
    /// Size in bytes (from `RFC822.SIZE`), if the server reported it.
    pub size: Option<u32>,
    /// Decoded MIME body. `None` if parsing failed (we still keep
    /// the UID so the caller can delete or re-fetch).
    pub mail: Option<Mail>,
    /// Raw error from parsing, preserved for diagnostics.
    pub parse_error: Option<String>,
    /// Was the `\Seen` flag set before we fetched?
    pub was_seen: bool,
}

/// Default ceiling on a single message's `RFC822.SIZE` before we
/// refuse to download its body.
///
/// Aerospace-grade defensive programming: an IMAP server (malicious,
/// compromised, or simply misbehaving) can report an arbitrarily
/// large mailbox. Without a cap, `BODY.PEEK[]` on a multi-gigabyte
/// message would buffer the whole thing in memory before we ever get
/// a chance to reject it in [`crate::mime::Mail::from_wire_bytes`],
/// turning a single hostile or corrupted message into an OOM kill of
/// the whole process. 50 MiB comfortably covers real-world messages
/// (including generous attachments) while still bounding worst case.
pub const DEFAULT_MAX_MESSAGE_SIZE: u32 = 50 * 1024 * 1024;

/// Returned by [`ImapSession::open_inbox`] so callers can pull messages
/// one by one or in a streaming fashion.
pub struct FetchHandle<'a> {
    session: &'a mut ImapSession,
    folder: String,
    /// Ceiling on `RFC822.SIZE` before we skip downloading a
    /// message's body. See [`DEFAULT_MAX_MESSAGE_SIZE`].
    max_message_size: u32,
}

impl<'a> FetchHandle<'a> {
    pub fn new(session: &'a mut ImapSession, info: crate::imap::SelectInfo) -> Self {
        Self {
            session,
            folder: info.folder,
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
        }
    }

    /// The folder this handle is bound to.
    pub fn folder(&self) -> &str {
        &self.folder
    }

    /// Override the per-message size ceiling (default
    /// [`DEFAULT_MAX_MESSAGE_SIZE`]). Messages whose server-reported
    /// `RFC822.SIZE` exceeds this are never downloaded — they come
    /// back as a [`FetchedMessage`] with `mail: None` and a
    /// descriptive `parse_error`, so the caller can decide whether to
    /// delete them, alert an operator, or fetch a truncated range.
    pub fn with_max_message_size(mut self, max_bytes: u32) -> Self {
        self.max_message_size = max_bytes;
        self
    }

    /// Fetch all messages in the selected folder that lack the `\Seen`
    /// flag, decode each one, and return them in UID-ascending order.
    ///
    /// The caller is responsible for marking messages `\Seen` afterwards
    /// via [`FetchHandle::mark_seen`] — we do not mutate state inside
    /// `fetch_new` because some clients want to filter first.
    pub async fn fetch_new(&mut self) -> Result<Vec<FetchedMessage>> {
        let uids: Vec<u32> = {
            let raw = self.session.raw_mut()?;
            raw.uid_search("UNSEEN")
                .await
                .map_err(MailError::Imap)?
                .into_iter()
                .collect()
        };
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        // UID set string: "1,2,3"
        let uid_set = uids
            .iter()
            .map(|u| u.to_string())
            .collect::<Vec<_>>()
            .join(",");
        self.fetch_uid_set(&uid_set).await
    }

    /// Fetch *all* messages in the folder regardless of the `\Seen`
    /// flag, decode them, and return them in UID-ascending order.
    /// Used for full-mailbox sync.
    pub async fn fetch_all(&mut self) -> Result<Vec<FetchedMessage>> {
        self.fetch_uid_set("1:*").await
    }

    /// Two-phase fetch for a UID set: first `RFC822.SIZE` only (cheap,
    /// bounded by the number of messages, not their content), then
    /// `BODY.PEEK[]` only for the messages that pass
    /// [`Self::max_message_size`]. This bounds worst-case memory to
    /// `max_message_size * (messages fetched in this batch)` instead
    /// of the sum of every message in the mailbox, regardless of how
    /// large a hostile or corrupted message claims to be.
    async fn fetch_uid_set(&mut self, uid_set: &str) -> Result<Vec<FetchedMessage>> {
        // ---- Phase 1: sizes + flags only, no body -----------------------
        let size_fetches: Vec<async_imap::types::Fetch> = {
            let raw = self.session.raw_mut()?;
            let stream = raw
                .uid_fetch(uid_set, "(FLAGS UID RFC822.SIZE)")
                .await
                .map_err(MailError::Imap)?;
            let mut stream = Box::pin(stream);
            let mut out = Vec::new();
            while let Some(item) = stream.next().await {
                out.push(item.map_err(MailError::Imap)?);
            }
            out
        };

        let mut oversized: Vec<FetchedMessage> = Vec::new();
        let mut fetchable_uids: Vec<u32> = Vec::new();
        for f in &size_fetches {
            let uid = f.uid.unwrap_or(0);
            let size = f.size;
            if size.is_some_and(|s| s > self.max_message_size) {
                oversized.push(FetchedMessage {
                    uid,
                    seq: f.message,
                    size,
                    mail: None,
                    parse_error: Some(format!(
                        "message size {} exceeds max_message_size {} — body not downloaded",
                        size.unwrap_or(0),
                        self.max_message_size
                    )),
                    was_seen: f.flags().any(|fl| matches!(fl, Flag::Seen)),
                });
            } else if uid != 0 {
                fetchable_uids.push(uid);
            }
        }

        // ---- Phase 2: body only for messages under the size cap ---------
        let mut out = oversized;
        if !fetchable_uids.is_empty() {
            let fetch_set = fetchable_uids
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
                .join(",");
            let raw = self.session.raw_mut()?;
            let stream = raw
                .uid_fetch(&fetch_set, "(FLAGS BODY.PEEK[] UID RFC822.SIZE)")
                .await
                .map_err(MailError::Imap)?;
            let mut stream = Box::pin(stream);
            while let Some(item) = stream.next().await {
                out.push(decode_fetch(item.map_err(MailError::Imap)?));
            }
        }

        // Server may return UIDs out of order; sort ascending.
        out.sort_by_key(|m| m.uid);
        Ok(out)
    }

    /// Mark a single message as `\Seen`. Returns `Ok(true)` if the
    /// flag was newly set, `Ok(false)` if it was already set.
    pub async fn mark_seen(&mut self, uid: u32) -> Result<bool> {
        let raw = self.session.raw_mut()?;
        let uid_s = uid.to_string();
        let stream = raw
            .uid_store(&uid_s, "+FLAGS (\\Seen)")
            .await
            .map_err(MailError::Imap)?;
        let mut stream = Box::pin(stream);
        let mut changed = false;
        while let Some(item) = stream.next().await {
            let _ = item.map_err(MailError::Imap)?;
            changed = true;
        }
        Ok(changed)
    }

    /// Mark a message `\Deleted` (will be expunged on `CLOSE` /
    /// mailbox close).
    pub async fn mark_deleted(&mut self, uid: u32) -> Result<()> {
        let raw = self.session.raw_mut()?;
        let uid_s = uid.to_string();
        let stream = raw
            .uid_store(&uid_s, "+FLAGS (\\Deleted)")
            .await
            .map_err(MailError::Imap)?;
        let mut stream = Box::pin(stream);
        while let Some(item) = stream.next().await {
            let _ = item.map_err(MailError::Imap)?;
        }
        Ok(())
    }

    /// Expunge all messages marked `\Deleted` from the folder.
    pub async fn expunge(&mut self) -> Result<()> {
        let raw = self.session.raw_mut()?;
        let stream = raw.expunge().await.map_err(MailError::Imap)?;
        let mut stream = Box::pin(stream);
        while let Some(_item) = stream.next().await {
            // drain
        }
        Ok(())
    }
}

fn decode_fetch(fetch: async_imap::types::Fetch) -> FetchedMessage {
    let uid = fetch.uid.unwrap_or(0);
    let seq = fetch.message;
    let size = fetch.size;
    let was_seen = fetch.flags().any(|f| matches!(f, Flag::Seen));

    let (mail_opt, err_opt) = match fetch.body() {
        Some(raw_bytes) => match Mail::from_wire_bytes(raw_bytes) {
            Ok(m) => (Some(m), None),
            Err(e) => (None, Some(e.to_string())),
        },
        None => (None, Some("FETCH response missing BODY".into())),
    };

    FetchedMessage {
        uid,
        seq,
        size,
        mail: mail_opt,
        parse_error: err_opt,
        was_seen,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetched_message_carries_uid() {
        let fm = FetchedMessage {
            uid: 42,
            seq: 1,
            size: Some(1234),
            mail: None,
            parse_error: Some("oops".into()),
            was_seen: false,
        };
        assert_eq!(fm.uid, 42);
        assert_eq!(fm.size, Some(1234));
        assert!(fm.mail.is_none());
    }
}
