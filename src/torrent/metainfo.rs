use std::ops::Range;

use crate::torrent::bencode::{
    BencodeValue, DecodeError, bdecode,
    parse::{ParseError, missing_field, parse_integer, parse_string},
    value_type,
};
use crate::torrent::parsers::{
    parse_announce_list, parse_http_seeds, parse_info, parse_nodes, parse_url_list,
};
use sha1::{Digest, Sha1};
use thiserror::Error;

pub struct TorrentMeta {
    pub announce: Option<String>,
    pub announce_list: Vec<Vec<String>>,

    pub creation_date: Option<i64>,
    pub comment: Option<String>,
    pub created_by: Option<String>,
    pub encoding: Option<String>,

    pub nodes: Vec<(String, u16)>,
    pub web_seeds: Vec<String>,
    pub http_seeds: Vec<String>,

    pub info: TorrentInfoV1,

    pub info_hash: [u8; 20],
    pub total_size: u64,
}

pub struct TorrentInfoV1 {
    pub name: String,
    pub piece_length: u64,
    pub piece_hashes: Vec<[u8; 20]>,
    pub files: Vec<TorrentFile>,
    pub private: bool,
}

pub struct TorrentFile {
    pub path: Vec<String>,
    pub length: u64,
    pub offset: u64,
}

#[derive(Debug, Error)]
pub enum MetainfoError {
    #[error("Bencode decoding failed: {0}")]
    Decode(#[from] DecodeError),

    #[error("Torrent root must be a dictionary, found {actual}")]
    RootNotDictionary { actual: &'static str },

    #[error(transparent)]
    Parse(#[from] ParseError),

    #[error("Field 'nodes[{index}]' must contain exactly 2 elements [host, port], found {actual}")]
    InvalidNodeLength { index: usize, actual: usize },

    #[error("Field 'nodes[{index}][0]' must contain a non-empty host")]
    EmptyNodeHost { index: usize },

    #[error("Field 'nodes[{index}][1]' must be a port in 1..=65535, found {value}")]
    InvalidNodePort { index: usize, value: i64 },

    #[error("Field 'info.pieces' must contain a multiple of 20 bytes, found {actual} bytes")]
    InvalidPiecesLength { actual: usize },

    #[error("Field 'info.piece length' must be greater than zero, found {value}")]
    InvalidPieceLength { value: i64 },

    #[error("Piece hash count does not match total size: expected {expected}, found {actual}")]
    PieceCountMismatch { expected: u64, actual: usize },

    #[error("Field 'info' must contain exactly one of 'length' or 'files': {reason}")]
    InvalidFileLayout { reason: &'static str },

    #[error("Field '{field}' must be a non-negative file size, found {value}")]
    InvalidFileLength { field: String, value: i64 },

    #[error("Field '{field}' must contain at least one path component")]
    EmptyFilePath { field: String },

    #[error("Total torrent size exceeds u64::MAX when adding file at index {file_index}")]
    TotalSizeOverflow { file_index: usize },
}

pub fn parse(data: &[u8]) -> Result<TorrentMeta, MetainfoError> {
    let mut announce = None;
    let mut announce_list = Vec::new();
    let mut creation_date = None;
    let mut comment = None;
    let mut created_by = None;
    let mut encoding = None;
    let mut info = None;
    let mut nodes = Vec::new();
    let mut web_seeds = Vec::new();
    let mut http_seeds = Vec::new();

    let (decoded, last_index) = bdecode(data, 0)?;

    if last_index != data.len() {
        return Err(DecodeError::TrailingData { offset: last_index }.into());
    }

    let BencodeValue::Dictionary(decoded_dict) = decoded else {
        return Err(MetainfoError::RootNotDictionary {
            actual: value_type(&decoded),
        });
    };

    let info_range = find_info_range(data)?;

    for (key, value) in &decoded_dict {
        match key.as_slice() {
            b"announce" => announce = Some(parse_string(value, "announce")?),
            b"announce-list" => announce_list = parse_announce_list(value)?,
            b"creation date" => creation_date = Some(parse_integer(value, "creation date")?),
            b"comment" => comment = Some(parse_string(value, "comment")?),
            b"created by" => created_by = Some(parse_string(value, "created by")?),
            b"encoding" => encoding = Some(parse_string(value, "encoding")?),
            b"nodes" => nodes = parse_nodes(value)?,
            b"url-list" => web_seeds = parse_url_list(value)?,
            b"httpseeds" => http_seeds = parse_http_seeds(value)?,
            b"info" => info = Some(parse_info(value)?),
            _ => {}
        }
    }

    let torrent_info = info.ok_or_else(|| missing_field("info"))?;

    let total_size =
        torrent_info
            .files
            .iter()
            .enumerate()
            .try_fold(0u64, |total, (file_index, file)| {
                total
                    .checked_add(file.length)
                    .ok_or(MetainfoError::TotalSizeOverflow { file_index })
            })?;

    let expected = total_size.div_ceil(torrent_info.piece_length);
    let actual = torrent_info.piece_hashes.len();
    let piece_count = usize::try_from(expected)
        .map_err(|_| MetainfoError::PieceCountMismatch { expected, actual })?;

    if actual != piece_count {
        return Err(MetainfoError::PieceCountMismatch { expected, actual });
    }

    let mut hasher = Sha1::new();
    hasher.update(&data[info_range]);
    let info_hash: [u8; 20] = hasher.finalize().into();

    Ok(TorrentMeta {
        announce,
        announce_list,
        creation_date,
        comment,
        created_by,
        encoding,
        nodes,
        web_seeds,
        http_seeds,
        info: torrent_info,
        info_hash,
        total_size,
    })
}

/// Finds the original encoded info value after the root dictionary has been validated.
pub fn find_info_range(data: &[u8]) -> Result<Range<usize>, MetainfoError> {
    let mut index = 1;

    loop {
        match data.get(index) {
            Some(b'e') => break,
            Some(_) => {
                let (raw_key, value_start) = bdecode(data, index)?;
                let BencodeValue::Bytes(key) = raw_key else {
                    return Err(DecodeError::InvalidDictionaryKey { offset: index }.into());
                };
                let (_raw_value, value_end) = bdecode(data, value_start)?;
                if key.as_slice() == b"info" {
                    return Ok(value_start..value_end);
                }
                index = value_end;
            }
            None => return Err(DecodeError::UnexpectedEof { offset: index }.into()),
        }
    }

    Err(missing_field("info").into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::torrent::bencode::BencodeValue::{Bytes, Dictionary, Integer, List};
    use std::collections::BTreeMap;

    // SHA-1 of "abcd" and "e", respectively.
    const PIECE_HASHES: [[u8; 20]; 2] = [
        [
            0x81, 0xfe, 0x8b, 0xfe, 0x87, 0x57, 0x6c, 0x3e, 0xcb, 0x22, 0x42, 0x6f, 0x8e, 0x57,
            0x84, 0x73, 0x82, 0x91, 0x7a, 0xcf,
        ],
        [
            0x58, 0xe6, 0xb3, 0xa4, 0x14, 0xa1, 0xe0, 0x90, 0xdf, 0xc6, 0x02, 0x9a, 0xdd, 0x0f,
            0x35, 0x55, 0xcc, 0xba, 0x12, 0x7f,
        ],
    ];

    // Independently calculated SHA-1 of the bencoded single_info() fixture.
    const SINGLE_INFO_HASH: [u8; 20] = [
        0xfe, 0x7a, 0x5e, 0x5e, 0x2b, 0x8a, 0xc3, 0x25, 0x74, 0x8d, 0x81, 0x3d, 0x52, 0xa4, 0x61,
        0x0b, 0x90, 0x93, 0x2a, 0x86,
    ];

    fn bytes(value: &[u8]) -> BencodeValue {
        Bytes(value.to_vec())
    }

    fn dictionary(entries: Vec<(&str, BencodeValue)>) -> BencodeValue {
        Dictionary(
            entries
                .into_iter()
                .map(|(key, value)| (key.as_bytes().to_vec(), value))
                .collect(),
        )
    }

    fn single_info() -> BTreeMap<Vec<u8>, BencodeValue> {
        BTreeMap::from([
            (b"length".to_vec(), Integer(5)),
            (b"name".to_vec(), bytes(b"test.bin")),
            (b"piece length".to_vec(), Integer(4)),
            (b"pieces".to_vec(), Bytes(PIECE_HASHES.concat())),
        ])
    }

    fn file_entry(length: i64, path: &[&str]) -> BencodeValue {
        dictionary(vec![
            ("length", Integer(length)),
            (
                "path",
                List(path.iter().map(|part| bytes(part.as_bytes())).collect()),
            ),
        ])
    }

    fn multi_info(files: Vec<BencodeValue>) -> BTreeMap<Vec<u8>, BencodeValue> {
        let mut info = single_info();
        info.remove(b"length".as_slice());
        info.insert(b"name".to_vec(), bytes(b"bundle"));
        info.insert(b"files".to_vec(), List(files));
        info
    }

    // Fixture encoder only: it preserves binary strings and sorts dictionary keys.
    fn encode(value: &BencodeValue) -> Vec<u8> {
        let mut output = Vec::new();
        encode_into(value, &mut output);
        output
    }

    fn encode_into(value: &BencodeValue, output: &mut Vec<u8>) {
        match value {
            Integer(number) => output.extend_from_slice(format!("i{number}e").as_bytes()),
            Bytes(value) => {
                output.extend_from_slice(format!("{}:", value.len()).as_bytes());
                output.extend_from_slice(value);
            }
            List(items) => {
                output.push(b'l');
                for item in items {
                    encode_into(item, output);
                }
                output.push(b'e');
            }
            Dictionary(fields) => {
                output.push(b'd');
                for (key, value) in fields {
                    output.extend_from_slice(format!("{}:", key.len()).as_bytes());
                    output.extend_from_slice(key);
                    encode_into(value, output);
                }
                output.push(b'e');
            }
        }
    }

    fn torrent(info: BTreeMap<Vec<u8>, BencodeValue>) -> Vec<u8> {
        encode(&dictionary(vec![("info", Dictionary(info))]))
    }

    // Result::unwrap_err would require Debug on the successful metadata structs.
    fn expect_error<T>(result: Result<T, MetainfoError>) -> MetainfoError {
        match result {
            Err(error) => error,
            Ok(_) => panic!("expected an error, but parsing succeeded"),
        }
    }

    #[test]
    fn parse_single_file_metadata() {
        let meta = parse(&torrent(single_info())).expect("valid single-file torrent");

        assert_eq!(meta.info.name, "test.bin");
        assert_eq!(meta.info.piece_length, 4);
        assert_eq!(meta.info.piece_hashes, PIECE_HASHES.to_vec());
        assert!(!meta.info.private);
        assert_eq!(meta.total_size, 5);
        assert_eq!(meta.info.files.len(), 1);
        assert_eq!(meta.info.files[0].path, vec!["test.bin".to_owned()]);
        assert_eq!(meta.info.files[0].length, 5);
        assert_eq!(meta.info.files[0].offset, 0);
        assert!(meta.announce.is_none());
        assert!(meta.announce_list.is_empty());
        assert!(meta.creation_date.is_none());
        assert!(meta.comment.is_none());
        assert!(meta.created_by.is_none());
        assert!(meta.encoding.is_none());
        assert!(meta.nodes.is_empty());
        assert!(meta.web_seeds.is_empty());
        assert!(meta.http_seeds.is_empty());
    }

    #[test]
    fn parse_multiple_files_preserves_paths_offsets_and_zero_length_files() {
        let mut info = multi_info(vec![
            file_entry(5, &["dir", "first.bin"]),
            file_entry(0, &["empty.txt"]),
            file_entry(5, &["dir", "second.bin"]),
        ]);
        // Ten bytes form three pieces of length four, across file boundaries.
        info.insert(b"pieces".to_vec(), Bytes(vec![1; 60]));

        let meta = parse(&torrent(info)).expect("valid multi-file torrent");

        assert_eq!(meta.info.name, "bundle");
        assert_eq!(meta.total_size, 10);
        assert_eq!(meta.info.piece_hashes.len(), 3);
        assert_eq!(
            meta.info
                .files
                .iter()
                .map(|file| (file.length, file.offset))
                .collect::<Vec<_>>(),
            vec![(5, 0), (0, 5), (5, 5)]
        );
        assert_eq!(
            meta.info.files[0].path,
            vec!["dir".to_owned(), "first.bin".to_owned()]
        );
        assert_eq!(meta.info.files[1].path, vec!["empty.txt".to_owned()]);
        assert_eq!(
            meta.info.files[2].path,
            vec!["dir".to_owned(), "second.bin".to_owned()]
        );
    }

    #[test]
    fn parse_optional_metadata_and_tracker_tiers() {
        let data = encode(&dictionary(vec![
            ("announce", bytes(b"http://tracker-a")),
            (
                "announce-list",
                List(vec![
                    List(vec![bytes(b"http://tracker-a"), bytes(b"udp://tracker-b")]),
                    List(vec![bytes(b"https://backup")]),
                ]),
            ),
            ("creation date", Integer(1_700_000_000)),
            ("comment", bytes("Описание торрента".as_bytes())),
            ("created by", bytes(b"xtorrent tests")),
            ("encoding", bytes(b"UTF-8")),
            ("info", Dictionary(single_info())),
            (
                "nodes",
                List(vec![
                    List(vec![bytes(b"127.0.0.1"), Integer(6881)]),
                    List(vec![bytes(b"2001:db8::1"), Integer(65535)]),
                    List(vec![bytes(b"router.example"), Integer(1)]),
                ]),
            ),
            (
                "url-list",
                List(vec![
                    bytes(b"https://mirror-a/"),
                    bytes(b"https://mirror-b/"),
                ]),
            ),
            ("httpseeds", List(vec![bytes(b"https://seed/seed.php")])),
        ]));

        let meta = parse(&data).expect("valid optional metadata");

        assert_eq!(meta.announce.as_deref(), Some("http://tracker-a"));
        assert_eq!(
            meta.announce_list,
            vec![
                vec!["http://tracker-a".to_owned(), "udp://tracker-b".to_owned()],
                vec!["https://backup".to_owned()],
            ]
        );
        assert_eq!(meta.creation_date, Some(1_700_000_000));
        assert_eq!(meta.comment.as_deref(), Some("Описание торрента"));
        assert_eq!(meta.created_by.as_deref(), Some("xtorrent tests"));
        assert_eq!(meta.encoding.as_deref(), Some("UTF-8"));
        assert_eq!(
            meta.nodes,
            vec![
                ("127.0.0.1".to_owned(), 6881),
                ("2001:db8::1".to_owned(), 65535),
                ("router.example".to_owned(), 1),
            ]
        );
        assert_eq!(
            meta.web_seeds,
            vec![
                "https://mirror-a/".to_owned(),
                "https://mirror-b/".to_owned()
            ]
        );
        assert_eq!(meta.http_seeds, vec!["https://seed/seed.php".to_owned()]);
    }

    #[test]
    fn parse_trackerless_torrent_with_nodes() {
        let data = encode(&dictionary(vec![
            ("info", Dictionary(single_info())),
            (
                "nodes",
                List(vec![List(vec![bytes(b"router.example"), Integer(6881)])]),
            ),
        ]));
        let meta = parse(&data).expect("trackerless torrent with bootstrap node");

        assert!(meta.announce.is_none());
        assert_eq!(meta.nodes, vec![("router.example".to_owned(), 6881)]);
    }

    #[test]
    fn parse_private_flag() {
        for (flag, expected) in [(0, false), (1, true)] {
            let mut info = single_info();
            info.insert(b"private".to_vec(), Integer(flag));
            let meta = parse(&torrent(info)).expect("valid private flag");
            assert_eq!(meta.info.private, expected);
        }
    }

    #[test]
    fn parse_utf8_file_name() {
        let mut info = single_info();
        info.insert(b"name".to_vec(), bytes("файл.bin".as_bytes()));
        let meta = parse(&torrent(info)).expect("UTF-8 file name");

        assert_eq!(meta.info.name, "файл.bin");
        assert_eq!(meta.info.files[0].path, vec!["файл.bin".to_owned()]);
    }

    #[test]
    fn parse_zero_length_file_with_present_empty_pieces() {
        let mut info = single_info();
        info.insert(b"length".to_vec(), Integer(0));
        info.insert(b"pieces".to_vec(), Bytes(Vec::new()));
        let meta = parse(&torrent(info)).expect("empty file with empty pieces");

        assert_eq!(meta.total_size, 0);
        assert!(meta.info.piece_hashes.is_empty());
        assert_eq!(meta.info.files.len(), 1);
        assert_eq!(meta.info.files[0].length, 0);
        assert_eq!(meta.info.files[0].offset, 0);
    }

    #[test]
    fn piece_count_rounds_up_only_for_a_partial_last_piece() {
        for (length, hash_count) in [(4, 1), (5, 2), (8, 2)] {
            let mut info = single_info();
            info.insert(b"length".to_vec(), Integer(length));
            info.insert(b"pieces".to_vec(), Bytes(vec![0; hash_count * 20]));
            let meta = parse(&torrent(info)).expect("correct piece count");

            assert_eq!(meta.total_size, u64::try_from(length).unwrap());
            assert_eq!(meta.info.piece_hashes.len(), hash_count);
        }
    }

    #[test]
    fn info_hash_matches_independent_known_digest() {
        let meta = parse(&torrent(single_info())).expect("valid torrent");
        assert_eq!(meta.info_hash, SINGLE_INFO_HASH);
    }

    #[test]
    fn info_hash_ignores_outer_metadata() {
        let first = encode(&dictionary(vec![
            ("announce", bytes(b"http://tracker-a")),
            ("comment", bytes(b"first comment")),
            ("info", Dictionary(single_info())),
        ]));
        let second = encode(&dictionary(vec![
            ("announce", bytes(b"http://tracker-b")),
            ("comment", bytes(b"different comment")),
            ("info", Dictionary(single_info())),
        ]));

        assert_ne!(first, second);
        assert_eq!(parse(&first).unwrap().info_hash, SINGLE_INFO_HASH);
        assert_eq!(parse(&second).unwrap().info_hash, SINGLE_INFO_HASH);
    }

    #[test]
    fn info_hash_includes_unknown_fields_inside_info() {
        let original = parse(&torrent(single_info())).unwrap();
        let mut info = single_info();
        info.insert(b"extra".to_vec(), Integer(42));
        let extended = parse(&torrent(info)).expect("unknown info field is allowed");

        assert_eq!(extended.info.name, original.info.name);
        assert_eq!(extended.total_size, original.total_size);
        assert_ne!(extended.info_hash, original.info_hash);
    }

    #[test]
    fn find_info_range_skips_decoys_and_excludes_outer_dictionary() {
        let info_bytes = encode(&Dictionary(single_info()));
        let mut data = b"d7:comment10:d4:info1:x5:extrad4:infoi42ee4:info".to_vec();
        let start = data.len();
        data.extend_from_slice(&info_bytes);
        let end = data.len();
        data.extend_from_slice(b"1:z4:infoe");

        crate::torrent::bencode::decode(&data).expect("range helper requires valid bencode");
        let range = find_info_range(&data).expect("root info dictionary");

        assert_eq!(range, start..end);
        assert_eq!(&data[range], info_bytes.as_slice());
        assert_eq!(parse(&data).unwrap().info_hash, SINGLE_INFO_HASH);
    }

    #[test]
    fn find_info_range_reports_missing_info() {
        let error = expect_error(find_info_range(b"d7:comment4:infoe"));
        assert!(
            matches!(error, MetainfoError::Parse(ParseError::MissingField { ref field }) if field == "info"),
            "{error:?}"
        );
    }

    #[test]
    fn parse_rejects_non_dictionary_root() {
        for root in [Integer(1), bytes(b"info"), List(Vec::new())] {
            let error = expect_error(parse(&encode(&root)));
            assert!(
                matches!(error, MetainfoError::RootNotDictionary { .. }),
                "{error:?}"
            );
        }
    }

    #[test]
    fn parse_propagates_empty_and_truncated_input_errors() {
        for data in [
            b"".as_slice(),
            b"d4:info".as_slice(),
            b"d4:infod".as_slice(),
        ] {
            let error = expect_error(parse(data));
            assert!(
                matches!(
                    error,
                    MetainfoError::Decode(DecodeError::UnexpectedEof { .. })
                ),
                "{error:?}"
            );
        }
    }

    #[test]
    fn parse_rejects_trailing_data() {
        let mut data = torrent(single_info());
        data.push(b'\n');
        let error = expect_error(parse(&data));

        assert!(
            matches!(
                error,
                MetainfoError::Decode(DecodeError::TrailingData { .. })
            ),
            "{error:?}"
        );
    }

    #[test]
    fn parse_rejects_missing_info() {
        let error = expect_error(parse(b"de"));
        assert!(
            matches!(error, MetainfoError::Parse(ParseError::MissingField { ref field }) if field == "info"),
            "{error:?}"
        );
    }

    #[test]
    fn parse_rejects_non_dictionary_info() {
        let data = encode(&dictionary(vec![("info", Integer(1))]));
        let error = expect_error(parse(&data));
        assert!(
            matches!(
                error,
                MetainfoError::Parse(ParseError::UnexpectedFieldType { .. })
            ),
            "{error:?}"
        );
    }

    #[test]
    fn parse_rejects_missing_name() {
        let mut info = single_info();
        info.remove(b"name".as_slice());
        let error = expect_error(parse(&torrent(info)));
        assert!(
            matches!(error, MetainfoError::Parse(ParseError::MissingField { ref field }) if field == "info.name"),
            "{error:?}"
        );
    }

    #[test]
    fn parse_rejects_missing_piece_length() {
        let mut info = single_info();
        info.remove(b"piece length".as_slice());
        let error = expect_error(parse(&torrent(info)));
        assert!(
            matches!(error, MetainfoError::Parse(ParseError::MissingField { ref field }) if field == "info.piece length"),
            "{error:?}"
        );
    }

    #[test]
    fn parse_rejects_missing_pieces() {
        let mut info = single_info();
        info.remove(b"pieces".as_slice());
        let error = expect_error(parse(&torrent(info)));
        assert!(
            matches!(error, MetainfoError::Parse(ParseError::MissingField { ref field }) if field == "info.pieces"),
            "{error:?}"
        );
    }

    #[test]
    fn parse_rejects_wrong_info_field_types() {
        for key in ["name", "piece length", "pieces", "length", "private"] {
            let mut info = single_info();
            info.insert(key.as_bytes().to_vec(), List(Vec::new()));
            let error = expect_error(parse(&torrent(info)));
            assert!(
                matches!(
                    error,
                    MetainfoError::Parse(ParseError::UnexpectedFieldType { .. })
                ),
                "field {key}: {error:?}"
            );
        }
    }

    #[test]
    fn parse_rejects_invalid_utf8_name() {
        let mut info = single_info();
        info.insert(b"name".to_vec(), bytes(b"\xff"));
        let error = expect_error(parse(&torrent(info)));
        assert!(
            matches!(
                error,
                MetainfoError::Parse(ParseError::InvalidFieldUtf8 { .. })
            ),
            "{error:?}"
        );
    }

    #[test]
    fn parse_rejects_invalid_utf8_optional_text() {
        for key in ["announce", "comment", "created by", "encoding"] {
            let data = encode(&dictionary(vec![
                ("info", Dictionary(single_info())),
                (key, bytes(b"\xff")),
            ]));
            let error = expect_error(parse(&data));
            assert!(
                matches!(
                    error,
                    MetainfoError::Parse(ParseError::InvalidFieldUtf8 { .. })
                ),
                "field {key}: {error:?}"
            );
        }
    }

    #[test]
    fn parse_rejects_non_positive_piece_length() {
        for length in [-1, 0] {
            let mut info = single_info();
            info.insert(b"piece length".to_vec(), Integer(length));
            let error = expect_error(parse(&torrent(info)));
            assert!(
                matches!(error, MetainfoError::InvalidPieceLength { .. }),
                "{error:?}"
            );
        }
    }

    #[test]
    fn parse_rejects_negative_file_length() {
        let mut info = single_info();
        info.insert(b"length".to_vec(), Integer(-1));
        let error = expect_error(parse(&torrent(info)));
        assert!(
            matches!(error, MetainfoError::InvalidFileLength { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn parse_rejects_too_few_or_too_many_piece_hashes() {
        for hash_count in [0, 1, 3] {
            let mut info = single_info();
            info.insert(b"pieces".to_vec(), Bytes(vec![0; hash_count * 20]));
            let error = expect_error(parse(&torrent(info)));
            assert!(
                matches!(error, MetainfoError::PieceCountMismatch { .. }),
                "{hash_count} hashes: {error:?}"
            );
        }
    }

    #[test]
    fn parse_rejects_both_length_and_files() {
        let mut info = single_info();
        info.insert(b"files".to_vec(), List(vec![file_entry(5, &["test.bin"])]));
        let error = expect_error(parse(&torrent(info)));
        assert!(
            matches!(
                error,
                MetainfoError::InvalidFileLayout {
                    reason: "both fields are present"
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn parse_rejects_missing_file_layout() {
        let mut info = single_info();
        info.remove(b"length".as_slice());
        let error = expect_error(parse(&torrent(info)));
        assert!(
            matches!(
                error,
                MetainfoError::InvalidFileLayout {
                    reason: "neither field is present",
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn parse_rejects_wrong_files_and_file_entry_types() {
        for files in [Integer(1), List(vec![Integer(1)])] {
            let mut info = multi_info(Vec::new());
            info.insert(b"files".to_vec(), files);
            let error = expect_error(parse(&torrent(info)));
            assert!(
                matches!(
                    error,
                    MetainfoError::Parse(ParseError::UnexpectedFieldType { .. })
                ),
                "{error:?}"
            );
        }
    }

    #[test]
    fn parse_reports_missing_file_length() {
        for file in [
            dictionary(Vec::new()),
            dictionary(vec![("path", List(vec![bytes(b"test.bin")]))]),
        ] {
            let error = expect_error(parse(&torrent(multi_info(vec![file]))));
            assert!(
                matches!(error, MetainfoError::Parse(ParseError::MissingField { ref field }) if field == "info.files[0].length"),
                "{error:?}"
            );
        }
    }

    #[test]
    fn parse_reports_missing_file_path() {
        let file = dictionary(vec![("length", Integer(5))]);
        let error = expect_error(parse(&torrent(multi_info(vec![file]))));
        assert!(
            matches!(error, MetainfoError::Parse(ParseError::MissingField { ref field }) if field == "info.files[0].path"),
            "{error:?}"
        );
    }

    #[test]
    fn parse_reports_empty_file_path() {
        let error = expect_error(parse(&torrent(multi_info(vec![file_entry(5, &[])]))));
        assert!(
            matches!(error, MetainfoError::EmptyFilePath { ref field } if field == "info.files[0].path"),
            "{error:?}"
        );
    }

    #[test]
    fn parse_rejects_wrong_path_and_path_component_types() {
        for path in [bytes(b"test.bin"), List(vec![Integer(1)])] {
            let file = dictionary(vec![("length", Integer(5)), ("path", path)]);
            let error = expect_error(parse(&torrent(multi_info(vec![file]))));
            assert!(
                matches!(
                    error,
                    MetainfoError::Parse(ParseError::UnexpectedFieldType { .. })
                ),
                "{error:?}"
            );
        }
    }

    #[test]
    fn parse_preserves_invalid_utf8_path_error() {
        let file = dictionary(vec![
            ("length", Integer(5)),
            ("path", List(vec![bytes(b"\xff")])),
        ]);
        let error = expect_error(parse(&torrent(multi_info(vec![file]))));
        assert!(
            matches!(
                error,
                MetainfoError::Parse(ParseError::InvalidFieldUtf8 { .. })
            ),
            "{error:?}"
        );
    }

    #[test]
    fn parse_ignores_unknown_file_fields() {
        let file = dictionary(vec![
            ("length", Integer(5)),
            ("path", List(vec![bytes(b"test.bin")])),
            ("extra", Integer(42)),
        ]);
        let meta = parse(&torrent(multi_info(vec![file]))).expect("unknown file field");
        assert_eq!(meta.total_size, 5);
        assert_eq!(meta.info.files[0].path, vec!["test.bin".to_owned()]);
    }

    #[test]
    fn parse_rejects_total_size_overflow() {
        let info = multi_info(vec![
            file_entry(i64::MAX, &["first.bin"]),
            file_entry(i64::MAX, &["second.bin"]),
            file_entry(2, &["third.bin"]),
        ]);
        let error = expect_error(parse(&torrent(info)));
        assert!(
            matches!(error, MetainfoError::TotalSizeOverflow { file_index: 2 }),
            "{error:?}"
        );
    }

    #[test]
    fn type_error_reports_field_expected_type_and_actual_type() {
        let mut info = single_info();
        info.insert(b"piece length".to_vec(), bytes(b"4"));
        let error = expect_error(parse(&torrent(info)));

        assert!(
            matches!(
                error,
                MetainfoError::Parse(ParseError::UnexpectedFieldType {
                    ref field,
                    expected: "integer",
                    actual: "byte string",
                }) if field == "info.piece length"
            ),
            "{error:?}"
        );
        assert_eq!(
            error.to_string(),
            "Field 'info.piece length': expected integer, found byte string"
        );
    }

    #[test]
    fn nested_type_error_reports_file_and_path_component_indices() {
        let file = dictionary(vec![
            ("length", Integer(2)),
            ("path", List(vec![bytes(b"dir"), Integer(1)])),
        ]);
        let info = multi_info(vec![file_entry(3, &["first.bin"]), file]);
        let error = expect_error(parse(&torrent(info)));

        assert!(
            matches!(
                error,
                MetainfoError::Parse(ParseError::UnexpectedFieldType {
                    ref field,
                    expected: "byte string",
                    actual: "integer",
                }) if field == "info.files[1].path[1]"
            ),
            "{error:?}"
        );
        assert_eq!(
            error.to_string(),
            "Field 'info.files[1].path[1]': expected byte string, found integer"
        );
    }

    #[test]
    fn piece_count_error_reports_expected_and_actual_counts() {
        let mut info = single_info();
        info.insert(b"pieces".to_vec(), Bytes(vec![0; 20]));
        let error = expect_error(parse(&torrent(info)));

        assert!(
            matches!(
                error,
                MetainfoError::PieceCountMismatch {
                    expected: 2,
                    actual: 1
                }
            ),
            "{error:?}"
        );
        assert_eq!(
            error.to_string(),
            "Piece hash count does not match total size: expected 2, found 1"
        );
    }

    #[test]
    fn invalid_file_length_reports_file_index_and_value() {
        let info = multi_info(vec![
            file_entry(3, &["first.bin"]),
            file_entry(-2, &["second.bin"]),
        ]);
        let error = expect_error(parse(&torrent(info)));

        assert!(
            matches!(
                error,
                MetainfoError::InvalidFileLength { ref field, value: -2 }
                    if field == "info.files[1].length"
            ),
            "{error:?}"
        );
        assert_eq!(
            error.to_string(),
            "Field 'info.files[1].length' must be a non-negative file size, found -2"
        );
    }
}
