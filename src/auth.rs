//! First-message WebSocket authentication.
//!
//! The token used to travel in the upgrade URL (`?token=…`), where it lands
//! in browser history, proxy access logs and `Referer`-adjacent tooling. Now
//! the upgrade is accepted unconditionally and the client must send
//! `{"type":"auth","token":"…"}` as its very first message within
//! [`AUTH_TIMEOUT`]. A failed attempt is answered with a WebSocket close
//! frame carrying [`CLOSE_UNAUTHORIZED`], which — unlike a rejected HTTP
//! upgrade, whose status code the browser WebSocket API hides — lets the
//! client tell "wrong password" apart from "server unreachable".

use std::time::Duration;

use futures_util::{Stream, StreamExt};
use subtle::ConstantTimeEq;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

/// How long a freshly upgraded socket may stay silent before it's dropped.
pub const AUTH_TIMEOUT: Duration = Duration::from_secs(5);

/// Application close code (4000-4999 is the private-use range) for a
/// missing or wrong token.
pub const CLOSE_UNAUTHORIZED: u16 = 4001;

/// Delay before answering a failed attempt, so a user-chosen (and possibly
/// weak) `RING2ZERO_TOKEN` can't be brute-forced at line rate.
pub const FAILURE_DELAY: Duration = Duration::from_secs(1);

/// Constant-time token comparison. Lengths aren't secret (the default token
/// is always 32 hex chars), so a length mismatch may short-circuit.
pub fn token_matches(expected: &str, provided: &str) -> bool {
    expected.as_bytes().ct_eq(provided.as_bytes()).into()
}

/// Extracts the token from an auth message, or `None` if `text` isn't one.
pub fn parse_auth_message(text: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    if json.get("type")?.as_str()? != "auth" {
        return None;
    }
    Some(json.get("token")?.as_str()?.to_owned())
}

#[derive(Debug, PartialEq, Eq)]
pub enum AuthOutcome {
    Accepted,
    /// Wrong token, or a first message that wasn't an auth message at all.
    Rejected,
    /// Nothing arrived within [`AUTH_TIMEOUT`], or the socket closed/errored.
    Gone,
}

/// Waits for the client's first message and checks it against `expected`.
pub async fn authenticate<S>(ws_receiver: &mut S, expected: &str) -> AuthOutcome
where
    S: Stream<Item = Result<Message, WsError>> + Unpin,
{
    let first = match tokio::time::timeout(AUTH_TIMEOUT, ws_receiver.next()).await {
        Ok(Some(Ok(msg))) => msg,
        _ => return AuthOutcome::Gone,
    };
    match first {
        Message::Text(text) => match parse_auth_message(&text) {
            Some(token) if token_matches(expected, &token) => AuthOutcome::Accepted,
            _ => AuthOutcome::Rejected,
        },
        Message::Close(_) => AuthOutcome::Gone,
        _ => AuthOutcome::Rejected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_matches_only_the_exact_token() {
        assert!(token_matches("abc123", "abc123"));
        assert!(!token_matches("abc123", "abc124"));
        assert!(!token_matches("abc123", "abc12"));
        assert!(!token_matches("abc123", ""));
    }

    #[test]
    fn parse_auth_message_extracts_the_token() {
        assert_eq!(
            parse_auth_message(r#"{"type":"auth","token":"t0k"}"#),
            Some("t0k".into())
        );
    }

    #[test]
    fn parse_auth_message_rejects_other_shapes() {
        assert_eq!(
            parse_auth_message(r#"{"type":"answer","token":"t0k"}"#),
            None
        );
        assert_eq!(parse_auth_message(r#"{"type":"auth"}"#), None);
        assert_eq!(parse_auth_message(r#"{"type":"auth","token":42}"#), None);
        assert_eq!(parse_auth_message("not json"), None);
    }

    fn stream_of(msgs: Vec<Message>) -> impl Stream<Item = Result<Message, WsError>> + Unpin {
        futures_util::stream::iter(msgs.into_iter().map(Ok))
    }

    #[tokio::test]
    async fn authenticate_accepts_the_right_token() {
        let mut s = stream_of(vec![Message::Text(
            r#"{"type":"auth","token":"good"}"#.into(),
        )]);
        assert_eq!(authenticate(&mut s, "good").await, AuthOutcome::Accepted);
    }

    #[tokio::test]
    async fn authenticate_rejects_a_wrong_token_or_non_auth_first_message() {
        let mut s = stream_of(vec![Message::Text(
            r#"{"type":"auth","token":"bad"}"#.into(),
        )]);
        assert_eq!(authenticate(&mut s, "good").await, AuthOutcome::Rejected);
        let mut s = stream_of(vec![Message::Binary(vec![1, 2, 3])]);
        assert_eq!(authenticate(&mut s, "good").await, AuthOutcome::Rejected);
    }

    #[tokio::test]
    async fn authenticate_reports_a_closed_socket_as_gone() {
        let mut s = stream_of(vec![]);
        assert_eq!(authenticate(&mut s, "good").await, AuthOutcome::Gone);
    }

    #[tokio::test(start_paused = true)]
    async fn authenticate_times_out_a_silent_client() {
        let mut s = futures_util::stream::pending::<Result<Message, WsError>>();
        assert_eq!(authenticate(&mut s, "good").await, AuthOutcome::Gone);
    }
}
