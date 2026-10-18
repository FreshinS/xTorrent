use std::net::{Ipv4Addr, Ipv6Addr};
use std::str;

use reqwest::Client;
use thiserror::Error;

use crate::{
    torrent::{
        bencode::{BencodeValue, DecodeError, bdecode, value_type},
        metainfo::TorrentMeta,
    },
    utils::percent_encode_bytes,
};

pub struct AnnounceResponse {
    pub interval: u64,
    pub peers: Vec<Peer>,
    pub complete: Option<u64>,
    pub incomplete: Option<u64>,
}

pub struct Peer {
    pub host: String,
    pub port: u16,
    pub peer_id: Option<[u8; 20]>,
}

#[derive(Debug, Clone, Copy)]
pub enum AnnounceEvent {
    Started,
    Completed,
    Stopped,
}

impl AnnounceEvent {
    fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Completed => "completed",
            Self::Stopped => "stopped",
        }
    }
}

pub struct AnnounceOptions {
    pub client: Option<Client>,
    pub peer_id: Option<[u8; 20]>,
    pub port: u16,
    pub uploaded: u64,
    pub downloaded: u64,
    pub left: u64,
    pub event: Option<AnnounceEvent>,
}

#[derive(Debug, Error)]
pub enum TrackerError {
    #[error("Failed to make request: {0}")]
    Request(#[from] reqwest::Error),

    #[error("Torrent has no announce URL")]
    MissingAnnounceUrl,

    #[error("Announce URL is invalid: {reason}")]
    InvalidAnnounceUrl { reason: String },

    #[error("Bencode decoding failed: {0}")]
    Decode(#[from] DecodeError),

    #[error("Required tracker response field '{field}' is missing")]
    MissingField { field: String },

    #[error("Tracker response field '{field}': expected {expected}, found {actual}")]
    UnexpectedFieldType {
        field: String,
        expected: &'static str,
        actual: &'static str,
    },

    #[error("Tracker response field '{field}' is not valid UTF-8: {source}")]
    InvalidFieldUtf8 {
        field: String,
        #[source]
        source: std::str::Utf8Error,
    },

    #[error("Tracker reported failure: {reason}")]
    FailureReason { reason: String },

    #[error("Tracker response field 'interval' must be a non-negative integer, found {value}")]
    InvalidInterval { value: i64 },

    #[error("Tracker response field '{field}' must be a non-negative integer, found {value}")]
    InvalidCounter { field: String, value: i64 },

    #[error("Tracker peer port at '{field}' must be in 1..=65535, found {value}")]
    InvalidPeerPort { field: String, value: i64 },

    #[error("Tracker peer host at '{field}' must not be empty")]
    EmptyPeerHost { field: String },

    #[error("Tracker peer id at '{field}' must contain exactly 20 bytes, found {actual}")]
    InvalidPeerIdLength { field: String, actual: usize },

    #[error(
        "Compact IPv{version} peer data must contain a multiple of {record_size} bytes, found {actual}"
    )]
    InvalidCompactPeersLength {
        version: u8,
        record_size: usize,
        actual: usize,
    },

    #[error("Announce response root must be a dictionary, found {actual}")]
    RootNotDictionary { actual: &'static str },
}

pub async fn announce(
    torrent: &TorrentMeta,
    options: AnnounceOptions,
) -> Result<(AnnounceResponse, Client, [u8; 20], u16), TrackerError> {
    let client = options.client.unwrap_or_default();

    let announce_url = torrent
        .announce
        .as_deref()
        .ok_or(TrackerError::MissingAnnounceUrl)?;
    let peer_id = options.peer_id.unwrap_or_else(generate_peer_id);
    let uploaded = options.uploaded;
    let downloaded = options.downloaded;
    let left = options.left;
    let port = options.port;

    let mut url =
        reqwest::Url::parse(announce_url).map_err(|error| TrackerError::InvalidAnnounceUrl {
            reason: error.to_string(),
        })?;

    let mut query = url.query().unwrap_or_default().to_owned();
    if !query.is_empty() {
        query.push('&');
    }

    query.push_str(&format!(
        "info_hash={}&peer_id={}&port={}&uploaded={}&downloaded={}&left={}",
        percent_encode_bytes(&torrent.info_hash),
        percent_encode_bytes(&peer_id),
        port,
        uploaded,
        downloaded,
        left,
    ));

    if let Some(event) = options.event {
        query.push_str("&event=");
        query.push_str(event.as_str());
    }

    query.push_str("&compact=1");

    url.set_query(Some(&query));

    let res = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    let (decoded_res, last_index) = bdecode(&res, 0)?;

    if last_index != res.len() {
        return Err(DecodeError::TrailingData { offset: last_index }.into());
    }

    Ok((parse_announce(&decoded_res)?, client, peer_id, port))
}

fn missing_field(field: &str) -> TrackerError {
    TrackerError::MissingField {
        field: field.to_owned(),
    }
}

fn unexpected_type(value: &BencodeValue, field: &str, expected: &'static str) -> TrackerError {
    TrackerError::UnexpectedFieldType {
        field: field.to_owned(),
        expected,
        actual: value_type(value),
    }
}

fn parse_tracker_string(value: &BencodeValue, field: &str) -> Result<String, TrackerError> {
    let BencodeValue::Bytes(bytes) = value else {
        return Err(unexpected_type(value, field, "byte string"));
    };

    let text = str::from_utf8(bytes).map_err(|source| TrackerError::InvalidFieldUtf8 {
        field: field.to_owned(),
        source,
    })?;

    Ok(text.to_owned())
}

fn parse_tracker_integer(value: &BencodeValue, field: &str) -> Result<i64, TrackerError> {
    let BencodeValue::Integer(number) = value else {
        return Err(unexpected_type(value, field, "integer"));
    };

    Ok(*number)
}

fn parse_counter(value: &BencodeValue, field: &str) -> Result<u64, TrackerError> {
    let raw_value = parse_tracker_integer(value, field)?;
    u64::try_from(raw_value).map_err(|_| TrackerError::InvalidCounter {
        field: field.to_owned(),
        value: raw_value,
    })
}

fn valid_peer_port(value: i64, field: &str) -> Result<u16, TrackerError> {
    let port = u16::try_from(value).map_err(|_| TrackerError::InvalidPeerPort {
        field: field.to_owned(),
        value,
    })?;
    if port == 0 {
        return Err(TrackerError::InvalidPeerPort {
            field: field.to_owned(),
            value,
        });
    }
    Ok(port)
}

fn parse_announce(announce: &BencodeValue) -> Result<AnnounceResponse, TrackerError> {
    let mut interval_field = None;
    let mut peers_field_seen = false;
    let mut peers = Vec::new();
    let mut complete = None;
    let mut incomplete = None;

    let BencodeValue::Dictionary(decoded_announce) = announce else {
        return Err(TrackerError::RootNotDictionary {
            actual: value_type(announce),
        });
    };

    for (key, value) in decoded_announce {
        match key.as_slice() {
            b"failure reason" => {
                return Err(TrackerError::FailureReason {
                    reason: parse_tracker_string(value, "failure reason")?,
                });
            }
            b"interval" => {
                let raw_interval = parse_tracker_integer(value, "interval")?;
                interval_field = Some(u64::try_from(raw_interval).map_err(|_| {
                    TrackerError::InvalidInterval {
                        value: raw_interval,
                    }
                })?);
            }
            b"peers" => {
                peers_field_seen = true;
                peers.extend(parse_peers(value)?);
            }
            b"peers6" => {
                peers.extend(parse_peers6(value)?);
            }
            b"complete" => {
                complete = Some(parse_counter(value, "complete")?);
            }
            b"incomplete" => {
                incomplete = Some(parse_counter(value, "incomplete")?);
            }
            _ => {}
        }
    }

    let interval = interval_field.ok_or_else(|| missing_field("interval"))?;

    if !peers_field_seen {
        return Err(missing_field("peers"));
    }

    Ok(AnnounceResponse {
        interval,
        peers,
        complete,
        incomplete,
    })
}

fn parse_peers_string(raw_peers: &[u8]) -> Result<Vec<Peer>, TrackerError> {
    let (raw_peer_chunks, remainder) = raw_peers.as_chunks::<6>();
    if !remainder.is_empty() {
        return Err(TrackerError::InvalidCompactPeersLength {
            version: 4,
            record_size: 6,
            actual: raw_peers.len(),
        });
    }

    let mut peers = Vec::new();

    for raw_peer in raw_peer_chunks {
        let ip_bytes: [u8; 4] = raw_peer[..4].try_into().unwrap();
        let port_bytes: [u8; 2] = raw_peer[4..6].try_into().unwrap();

        let ip = Ipv4Addr::from(ip_bytes).to_string();
        let port = valid_peer_port(u16::from_be_bytes(port_bytes) as i64, "peers")?;

        peers.push(Peer {
            host: ip,
            port,
            peer_id: None,
        });
    }

    Ok(peers)
}

fn parse_peers_list(raw_peers: &[BencodeValue]) -> Result<Vec<Peer>, TrackerError> {
    let mut peers = Vec::new();

    for (index, peer) in raw_peers.iter().enumerate() {
        let peer_field = format!("peers[{index}]");
        let BencodeValue::Dictionary(peer_dict) = peer else {
            return Err(unexpected_type(peer, &peer_field, "dictionary"));
        };

        let mut peer_id_field = None;
        let mut ip_field = None;
        let mut port_field = None;

        for (key, value) in peer_dict {
            match key.as_slice() {
                b"ip" => {
                    ip_field = Some(parse_tracker_string(value, &format!("{peer_field}.ip"))?);
                }
                b"port" => {
                    let field = format!("{peer_field}.port");
                    let raw_port = parse_tracker_integer(value, &field)?;
                    port_field = Some(valid_peer_port(raw_port, &field)?);
                }
                b"peer id" => {
                    let BencodeValue::Bytes(peer_id_value) = value else {
                        return Err(unexpected_type(
                            value,
                            &format!("{peer_field}.peer id"),
                            "byte string",
                        ));
                    };
                    peer_id_field = Some(peer_id_value.as_slice().try_into().map_err(|_| {
                        TrackerError::InvalidPeerIdLength {
                            field: format!("{peer_field}.peer id"),
                            actual: peer_id_value.len(),
                        }
                    })?);
                }
                _ => {}
            }
        }

        let peer_id =
            peer_id_field.ok_or_else(|| missing_field(&format!("{peer_field}.peer id")))?;
        let ip = ip_field.ok_or_else(|| missing_field(&format!("{peer_field}.ip")))?;
        if ip.is_empty() {
            return Err(TrackerError::EmptyPeerHost {
                field: format!("{peer_field}.ip"),
            });
        }
        let port = port_field.ok_or_else(|| missing_field(&format!("{peer_field}.port")))?;

        peers.push(Peer {
            host: ip,
            port,
            peer_id: Some(peer_id),
        })
    }

    Ok(peers)
}

fn parse_peers(value: &BencodeValue) -> Result<Vec<Peer>, TrackerError> {
    match value {
        BencodeValue::Bytes(peers) => parse_peers_string(peers),
        BencodeValue::List(peers) => parse_peers_list(peers),
        _ => Err(unexpected_type(value, "peers", "byte string or list")),
    }
}

fn parse_peers6(value: &BencodeValue) -> Result<Vec<Peer>, TrackerError> {
    let BencodeValue::Bytes(raw_peers) = value else {
        return Err(unexpected_type(value, "peers6", "byte string"));
    };

    let (raw_peer_chunks, remainder) = raw_peers.as_chunks::<18>();
    if !remainder.is_empty() {
        return Err(TrackerError::InvalidCompactPeersLength {
            version: 6,
            record_size: 18,
            actual: raw_peers.len(),
        });
    }

    let mut peers = Vec::new();

    for raw_peer in raw_peer_chunks {
        let ip_bytes: [u8; 16] = raw_peer[..16].try_into().unwrap();
        let port_bytes: [u8; 2] = raw_peer[16..18].try_into().unwrap();

        let ip = Ipv6Addr::from(ip_bytes).to_string();
        let port = valid_peer_port(u16::from_be_bytes(port_bytes) as i64, "peers6")?;

        peers.push(Peer {
            host: ip,
            port,
            peer_id: None,
        })
    }

    Ok(peers)
}

// [TODO] Need to add 6 bytes of client version
fn generate_peer_id() -> [u8; 20] {
    rand::random()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn dictionary(fields: Vec<(&[u8], BencodeValue)>) -> BencodeValue {
        BencodeValue::Dictionary(
            fields
                .into_iter()
                .map(|(key, value)| (key.to_vec(), value))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    fn expect_error<T>(result: Result<T, TrackerError>) -> TrackerError {
        match result {
            Err(error) => error,
            Ok(_) => panic!("expected an error, but parsing succeeded"),
        }
    }

    #[test]
    fn announce_event_uses_tracker_protocol_names() {
        assert_eq!(AnnounceEvent::Started.as_str(), "started");
        assert_eq!(AnnounceEvent::Completed.as_str(), "completed");
        assert_eq!(AnnounceEvent::Stopped.as_str(), "stopped");
    }

    #[test]
    fn parse_announce_combines_ipv4_and_ipv6_compact_peers() {
        let mut ipv4_peers = vec![127_u8, 0, 0, 1];
        ipv4_peers.extend(6881_u16.to_be_bytes());

        let mut ipv6_peers = vec![0_u8; 16];
        ipv6_peers[15] = 1;
        ipv6_peers.extend(51413_u16.to_be_bytes());

        let announce = dictionary(vec![
            (b"complete", BencodeValue::Integer(7)),
            (b"incomplete", BencodeValue::Integer(2)),
            (b"interval", BencodeValue::Integer(1800)),
            (b"peers", BencodeValue::Bytes(ipv4_peers)),
            (b"peers6", BencodeValue::Bytes(ipv6_peers)),
        ]);

        let response = parse_announce(&announce).unwrap();

        assert_eq!(response.interval, 1800);
        assert_eq!(response.complete, Some(7));
        assert_eq!(response.incomplete, Some(2));
        assert_eq!(response.peers.len(), 2);
        assert_eq!(response.peers[0].host, "127.0.0.1");
        assert_eq!(response.peers[0].port, 6881);
        assert!(response.peers[0].peer_id.is_none());
        assert_eq!(response.peers[1].host, "::1");
        assert_eq!(response.peers[1].port, 51413);
        assert!(response.peers[1].peer_id.is_none());
    }

    #[test]
    fn parse_announce_accepts_dictionary_peers_with_peer_ids() {
        let peer_id = [b'x'; 20];
        let peer = dictionary(vec![
            (b"ip", BencodeValue::Bytes(b"peer.example".to_vec())),
            (b"peer id", BencodeValue::Bytes(peer_id.to_vec())),
            (b"port", BencodeValue::Integer(51413)),
        ]);
        let announce = dictionary(vec![
            (b"interval", BencodeValue::Integer(60)),
            (b"peers", BencodeValue::List(vec![peer])),
        ]);

        let response = parse_announce(&announce).unwrap();

        assert_eq!(response.interval, 60);
        assert_eq!(response.peers.len(), 1);
        assert_eq!(response.peers[0].host, "peer.example");
        assert_eq!(response.peers[0].port, 51413);
        assert_eq!(response.peers[0].peer_id, Some(peer_id));
        assert_eq!(response.complete, None);
        assert_eq!(response.incomplete, None);
    }

    #[test]
    fn parse_announce_accepts_empty_compact_peer_list() {
        let announce = dictionary(vec![
            (b"interval", BencodeValue::Integer(30)),
            (b"peers", BencodeValue::Bytes(Vec::new())),
        ]);

        let response = parse_announce(&announce).unwrap();

        assert!(response.peers.is_empty());
    }

    #[test]
    fn parse_announce_rejects_non_dictionary_root() {
        let error = expect_error(parse_announce(&BencodeValue::List(Vec::new())));

        assert!(
            matches!(error, TrackerError::RootNotDictionary { actual: "list" }),
            "{error:?}"
        );
    }

    #[test]
    fn parse_announce_requires_interval_and_peers_fields() {
        let missing_interval = dictionary(vec![(b"peers", BencodeValue::Bytes(Vec::new()))]);
        assert!(matches!(
            expect_error(parse_announce(&missing_interval)),
            TrackerError::MissingField { ref field } if field == "interval"
        ));

        let missing_peers = dictionary(vec![(b"interval", BencodeValue::Integer(30))]);
        assert!(matches!(
            expect_error(parse_announce(&missing_peers)),
            TrackerError::MissingField { ref field } if field == "peers"
        ));
    }

    #[test]
    fn parse_announce_rejects_invalid_intervals() {
        let negative_interval = dictionary(vec![
            (b"interval", BencodeValue::Integer(-1)),
            (b"peers", BencodeValue::Bytes(Vec::new())),
        ]);
        assert!(matches!(
            expect_error(parse_announce(&negative_interval)),
            TrackerError::InvalidInterval { value: -1 }
        ));

        let wrong_type = dictionary(vec![
            (b"interval", BencodeValue::Bytes(b"30".to_vec())),
            (b"peers", BencodeValue::Bytes(Vec::new())),
        ]);
        assert!(matches!(
            expect_error(parse_announce(&wrong_type)),
            TrackerError::UnexpectedFieldType {
                ref field,
                expected: "integer",
                actual: "byte string",
            } if field == "interval"
        ));
    }

    #[test]
    fn parse_announce_rejects_failure_reason() {
        let announce = dictionary(vec![
            (
                b"failure reason",
                BencodeValue::Bytes(b"unavailable".to_vec()),
            ),
            (b"interval", BencodeValue::Integer(30)),
            (b"peers", BencodeValue::Bytes(Vec::new())),
        ]);

        assert!(matches!(
            expect_error(parse_announce(&announce)),
            TrackerError::FailureReason { ref reason } if reason == "unavailable"
        ));
    }

    #[test]
    fn parse_compact_ipv4_peers_rejects_incomplete_records() {
        for length in [1, 5, 7, 11] {
            let error = expect_error(parse_peers_string(&vec![0; length]));
            assert!(
                matches!(
                    error,
                    TrackerError::InvalidCompactPeersLength {
                        version: 4,
                        record_size: 6,
                        actual,
                    } if actual == length
                ),
                "{length} bytes: {error:?}"
            );
        }
    }

    #[test]
    fn parse_compact_ipv6_peers_rejects_wrong_type_and_incomplete_records() {
        let wrong_type = expect_error(parse_peers6(&BencodeValue::List(Vec::new())));
        assert!(matches!(
            wrong_type,
            TrackerError::UnexpectedFieldType {
                ref field,
                expected: "byte string",
                actual: "list",
            } if field == "peers6"
        ));

        for length in [1, 17, 19, 35] {
            let error = expect_error(parse_peers6(&BencodeValue::Bytes(vec![0; length])));
            assert!(
                matches!(
                    error,
                    TrackerError::InvalidCompactPeersLength {
                        version: 6,
                        record_size: 18,
                        actual,
                    } if actual == length
                ),
                "{length} bytes: {error:?}"
            );
        }
    }

    #[test]
    fn parse_peer_list_rejects_non_dictionary_items() {
        let error = expect_error(parse_peers_list(&[BencodeValue::Integer(1)]));

        assert!(matches!(
            error,
            TrackerError::UnexpectedFieldType {
                ref field,
                expected: "dictionary",
                actual: "integer",
            } if field == "peers[0]"
        ));
    }

    #[test]
    fn parse_peer_list_requires_valid_ip_port_and_peer_id() {
        let invalid_peers = vec![
            dictionary(vec![
                (b"ip", BencodeValue::Bytes(b"127.0.0.1".to_vec())),
                (b"port", BencodeValue::Integer(6881)),
            ]),
            dictionary(vec![
                (b"ip", BencodeValue::Bytes(b"127.0.0.1".to_vec())),
                (b"peer id", BencodeValue::Bytes(vec![0; 20])),
            ]),
            dictionary(vec![
                (b"ip", BencodeValue::Bytes(b"127.0.0.1".to_vec())),
                (b"port", BencodeValue::Integer(6881)),
                (b"peer id", BencodeValue::Bytes(vec![0; 19])),
            ]),
            dictionary(vec![
                (b"ip", BencodeValue::Integer(1)),
                (b"port", BencodeValue::Integer(6881)),
                (b"peer id", BencodeValue::Bytes(vec![0; 20])),
            ]),
            dictionary(vec![
                (b"ip", BencodeValue::Bytes(b"127.0.0.1".to_vec())),
                (b"port", BencodeValue::Integer(65536)),
                (b"peer id", BencodeValue::Bytes(vec![0; 20])),
            ]),
            dictionary(vec![
                (b"ip", BencodeValue::Bytes(b"127.0.0.1".to_vec())),
                (b"port", BencodeValue::Bytes(b"6881".to_vec())),
                (b"peer id", BencodeValue::Bytes(vec![0; 20])),
            ]),
        ];

        for peer in invalid_peers {
            let error = expect_error(parse_peers_list(&[peer]));
            assert!(
                matches!(
                    error,
                    TrackerError::MissingField { .. }
                        | TrackerError::InvalidPeerIdLength { .. }
                        | TrackerError::UnexpectedFieldType { .. }
                        | TrackerError::InvalidPeerPort { .. }
                        | TrackerError::EmptyPeerHost { .. }
                ),
                "{error:?}"
            );
        }
    }

    #[test]
    fn parse_peer_list_rejects_non_utf8_ip() {
        let peer = dictionary(vec![
            (b"ip", BencodeValue::Bytes(vec![0xff])),
            (b"peer id", BencodeValue::Bytes(vec![0; 20])),
            (b"port", BencodeValue::Integer(6881)),
        ]);
        let error = expect_error(parse_peers_list(&[peer]));

        assert!(
            matches!(
                error,
                TrackerError::InvalidFieldUtf8 { ref field, .. } if field == "peers[0].ip"
            ),
            "{error:?}"
        );
    }
}
