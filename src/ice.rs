//! ICE server configuration shared by the server's peer connection and the
//! browser client (sent to it after authentication), so both sides gather
//! the same kinds of candidates.
//!
//! `RING2ZERO_ICE_SERVERS` is a comma-separated list of
//! `stun:host[:port]`, `turn:user:pass@host[:port][?transport=udp|tcp]` or
//! `turns:…` entries. Unset keeps the historical host-candidates-only mode,
//! which works on a LAN or over Tailscale but not across NAT.

use serde::Serialize;

/// One entry of `RTCConfiguration.iceServers`, in the browser's shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct IceServer {
    pub urls: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub username: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub credential: String,
}

/// Parses `RING2ZERO_ICE_SERVERS`. Returns an error naming the bad entry.
pub fn parse_ice_servers(spec: &str) -> Result<Vec<IceServer>, String> {
    spec.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(parse_one)
        .collect()
}

fn parse_one(entry: &str) -> Result<IceServer, String> {
    let (scheme, rest) = entry
        .split_once(':')
        .ok_or_else(|| format!("ICE server `{entry}`: missing scheme (stun:, turn: or turns:)"))?;
    match scheme {
        "stun" | "stuns" => {
            if rest.is_empty() || rest.contains('@') {
                return Err(format!("ICE server `{entry}`: expected stun:host[:port]"));
            }
            Ok(IceServer { urls: vec![entry.to_owned()], username: String::new(), credential: String::new() })
        }
        "turn" | "turns" => {
            let (creds, host) = rest
                .rsplit_once('@')
                .ok_or_else(|| format!("ICE server `{scheme}:…`: TURN needs user:pass@host"))?;
            let (user, pass) = creds
                .split_once(':')
                .filter(|(u, p)| !u.is_empty() && !p.is_empty())
                .ok_or_else(|| format!("ICE server `{scheme}:…@{host}`: TURN needs user:pass@host"))?;
            if host.is_empty() {
                return Err(format!("ICE server `{scheme}:…`: missing host"));
            }
            Ok(IceServer { urls: vec![format!("{scheme}:{host}")], username: user.to_owned(), credential: pass.to_owned() })
        }
        _ => Err(format!("ICE server `{entry}`: unknown scheme `{scheme}` (stun:, turn: or turns:)")),
    }
}

impl From<&IceServer> for webrtc::ice_transport::ice_server::RTCIceServer {
    fn from(s: &IceServer) -> Self {
        Self { urls: s.urls.clone(), username: s.username.clone(), credential: s.credential.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_spec_means_no_servers() {
        assert_eq!(parse_ice_servers(""), Ok(vec![]));
        assert_eq!(parse_ice_servers(" , "), Ok(vec![]));
    }

    #[test]
    fn stun_and_turn_entries() {
        let v = parse_ice_servers("stun:stun.l.google.com:19302, turn:alice:s3cr:et@turn.example.org:3478?transport=tcp").unwrap();
        assert_eq!(v[0].urls, ["stun:stun.l.google.com:19302"]);
        assert!(v[0].username.is_empty());
        assert_eq!(v[1].urls, ["turn:turn.example.org:3478?transport=tcp"]);
        assert_eq!(v[1].username, "alice");
        assert_eq!(v[1].credential, "s3cr:et", "password may contain ':'");
    }

    #[test]
    fn password_may_contain_at_sign() {
        let v = parse_ice_servers("turns:bob:p@ss@relay.example.org").unwrap();
        assert_eq!((v[0].credential.as_str(), v[0].urls[0].as_str()), ("p@ss", "turns:relay.example.org"));
    }

    #[test]
    fn bad_entries_are_rejected_without_leaking_credentials() {
        for bad in ["stun.example.org", "http:x", "stun:", "turn:host", "turn::pw@host", "turn:user:@host", "turn:u:p@"] {
            assert!(parse_ice_servers(bad).is_err(), "{bad}");
        }
        let err = parse_ice_servers("turn:user:hunter2@").unwrap_err();
        assert!(!err.contains("hunter2"), "{err}");
    }

    #[test]
    fn serializes_in_the_browser_shape() {
        let v = parse_ice_servers("stun:a, turn:u:p@b").unwrap();
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            r#"[{"urls":["stun:a"]},{"urls":["turn:b"],"username":"u","credential":"p"}]"#
        );
    }
}
