use crate::torrent::{
    bencode::BencodeValue,
    metainfo::{MetainfoError, TorrentFile, TorrentInfoV1, missing_field, unexpected_type},
};

pub fn parse_info(info: &BencodeValue) -> Result<TorrentInfoV1, MetainfoError> {
    let BencodeValue::Dictionary(info_dict) = info else {
        return Err(unexpected_type(info, "info", "dictionary"));
    };

    let mut name = None;
    let mut piece_length = None;
    let mut piece_hashes = None;
    let mut private = false;
    let mut length = None;
    let mut files = None;

    for (key, value) in info_dict {
        match key.as_slice() {
            b"name" => name = Some(parse_string(value, "info.name")?),
            b"piece length" => piece_length = Some(parse_integer(value, "info.piece length")?),
            b"pieces" => piece_hashes = Some(parse_hashes(value)?),
            b"private" => private = parse_integer(value, "info.private")? == 1,
            b"length" => length = Some(parse_integer(value, "info.length")?),
            b"files" => files = Some(value),
            _ => {}
        }
    }

    let raw_piece_length = piece_length.ok_or_else(|| missing_field("info.piece length"))?;
    let piece_length =
        u64::try_from(raw_piece_length).map_err(|_| MetainfoError::InvalidPieceLength {
            value: raw_piece_length,
        })?;

    if piece_length == 0 {
        return Err(MetainfoError::InvalidPieceLength {
            value: raw_piece_length,
        });
    }

    let piece_hashes = piece_hashes.ok_or_else(|| missing_field("info.pieces"))?;

    if length.is_some() == files.is_some() {
        return Err(MetainfoError::InvalidFileLayout {
            reason: if length.is_some() {
                "both fields are present"
            } else {
                "neither field is present"
            },
        });
    }

    let file_name = name.ok_or_else(|| missing_field("info.name"))?;
    let mut torrent_files: Vec<TorrentFile> = Vec::new();

    if let Some(raw_file_length) = length {
        let file_length =
            u64::try_from(raw_file_length).map_err(|_| MetainfoError::InvalidFileLength {
                field: "info.length".to_owned(),
                value: raw_file_length,
            })?;

        torrent_files.push(TorrentFile {
            path: vec![file_name.clone()],
            length: file_length,
            offset: 0,
        });
    } else {
        let files_value = files.ok_or_else(|| missing_field("info.files"))?;
        let BencodeValue::List(file_list) = files_value else {
            return Err(unexpected_type(files_value, "info.files", "list"));
        };

        let mut offset = 0;

        for (file_index, file) in file_list.iter().enumerate() {
            let file_field = format!("info.files[{file_index}]");
            let BencodeValue::Dictionary(file_value) = file else {
                return Err(unexpected_type(file, &file_field, "dictionary"));
            };

            let length_field = format!("{file_field}.length");
            let path_field = format!("{file_field}.path");
            let mut file_length = None;
            let mut file_path = None;

            for (key, value) in file_value {
                match key.as_slice() {
                    b"length" => {
                        let raw_length = parse_integer(value, &length_field)?;
                        file_length = Some(u64::try_from(raw_length).map_err(|_| {
                            MetainfoError::InvalidFileLength {
                                field: length_field.clone(),
                                value: raw_length,
                            }
                        })?);
                    }
                    b"path" => {
                        let BencodeValue::List(path_values) = value else {
                            return Err(unexpected_type(value, &path_field, "list"));
                        };

                        let path: Result<Vec<String>, MetainfoError> = path_values
                            .iter()
                            .enumerate()
                            .map(|(index, part)| {
                                parse_string(part, &format!("{path_field}[{index}]"))
                            })
                            .collect();
                        file_path = Some(path?);
                    }
                    _ => {}
                }
            }

            let length = file_length.ok_or_else(|| missing_field(&length_field))?;
            let path = file_path.ok_or_else(|| missing_field(&path_field))?;

            if path.is_empty() {
                return Err(MetainfoError::EmptyFilePath { field: path_field });
            }

            torrent_files.push(TorrentFile {
                path,
                length,
                offset,
            });

            offset = offset
                .checked_add(length)
                .ok_or(MetainfoError::TotalSizeOverflow { file_index })?;
        }
    }

    Ok(TorrentInfoV1 {
        name: file_name,
        piece_length,
        piece_hashes,
        files: torrent_files,
        private,
    })
}

pub fn parse_hashes(value: &BencodeValue) -> Result<Vec<[u8; 20]>, MetainfoError> {
    let BencodeValue::Bytes(value_bytes) = value else {
        return Err(unexpected_type(value, "info.pieces", "byte string"));
    };

    let (chunks, remainder) = value_bytes.as_chunks::<20>();
    if !remainder.is_empty() {
        return Err(MetainfoError::InvalidPiecesLength {
            actual: value_bytes.len(),
        });
    }

    Ok(chunks.to_vec())
}

pub fn parse_http_seeds(value: &BencodeValue) -> Result<Vec<String>, MetainfoError> {
    let BencodeValue::List(seed_values) = value else {
        return Err(unexpected_type(value, "httpseeds", "list"));
    };

    seed_values
        .iter()
        .enumerate()
        .map(|(index, item)| parse_string(item, &format!("httpseeds[{index}]")))
        .collect()
}

pub fn parse_url_list(value: &BencodeValue) -> Result<Vec<String>, MetainfoError> {
    match value {
        BencodeValue::Bytes(_) => Ok(vec![parse_string(value, "url-list")?]),
        BencodeValue::List(items) => items
            .iter()
            .enumerate()
            .map(|(index, item)| parse_string(item, &format!("url-list[{index}]")))
            .collect(),
        _ => Err(unexpected_type(value, "url-list", "byte string or list")),
    }
}

pub fn parse_nodes(value: &BencodeValue) -> Result<Vec<(String, u16)>, MetainfoError> {
    let BencodeValue::List(node_values) = value else {
        return Err(unexpected_type(value, "nodes", "list"));
    };

    let mut nodes = Vec::new();

    for (index, node_value) in node_values.iter().enumerate() {
        let BencodeValue::List(node) = node_value else {
            return Err(unexpected_type(
                node_value,
                &format!("nodes[{index}]"),
                "list",
            ));
        };

        if node.len() != 2 {
            return Err(MetainfoError::InvalidNodeLength {
                index,
                actual: node.len(),
            });
        }

        let host = parse_string(&node[0], &format!("nodes[{index}][0]"))?;
        if host.is_empty() {
            return Err(MetainfoError::EmptyNodeHost { index });
        }

        let raw_port = parse_integer(&node[1], &format!("nodes[{index}][1]"))?;
        let port = u16::try_from(raw_port).map_err(|_| MetainfoError::InvalidNodePort {
            index,
            value: raw_port,
        })?;
        if port == 0 {
            return Err(MetainfoError::InvalidNodePort {
                index,
                value: raw_port,
            });
        }

        nodes.push((host, port));
    }

    Ok(nodes)
}

pub fn parse_announce_list(value: &BencodeValue) -> Result<Vec<Vec<String>>, MetainfoError> {
    let BencodeValue::List(tier_values) = value else {
        return Err(unexpected_type(value, "announce-list", "list"));
    };

    let mut tiers = Vec::new();

    for (tier_index, tier_value) in tier_values.iter().enumerate() {
        let tier_field = format!("announce-list[{tier_index}]");
        let BencodeValue::List(tracker_values) = tier_value else {
            return Err(unexpected_type(tier_value, &tier_field, "list"));
        };

        let tier: Result<Vec<String>, MetainfoError> = tracker_values
            .iter()
            .enumerate()
            .map(|(index, tracker)| parse_string(tracker, &format!("{tier_field}[{index}]")))
            .collect();
        tiers.push(tier?);
    }

    Ok(tiers)
}

pub fn parse_string(value: &BencodeValue, field: &str) -> Result<String, MetainfoError> {
    let BencodeValue::Bytes(value_bytes) = value else {
        return Err(unexpected_type(value, field, "byte string"));
    };

    let text = str::from_utf8(value_bytes).map_err(|source| MetainfoError::InvalidFieldUtf8 {
        field: field.to_owned(),
        source,
    })?;

    Ok(text.to_owned())
}

pub fn parse_integer(value: &BencodeValue, field: &str) -> Result<i64, MetainfoError> {
    let BencodeValue::Integer(number) = value else {
        return Err(unexpected_type(value, field, "integer"));
    };

    Ok(*number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::torrent::bencode::BencodeValue::{Bytes, Integer, List};
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

    fn bytes(value: &[u8]) -> BencodeValue {
        Bytes(value.to_vec())
    }

    // Result::unwrap_err would require Debug on the successful parser values.
    fn expect_error<T>(result: Result<T, MetainfoError>) -> MetainfoError {
        match result {
            Err(error) => error,
            Ok(_) => panic!("expected an error, but parsing succeeded"),
        }
    }
    #[test]
    fn parse_hashes_preserves_binary_values_and_order() {
        let hashes = parse_hashes(&Bytes(PIECE_HASHES.concat())).expect("binary hashes");
        assert_eq!(hashes, PIECE_HASHES.to_vec());
    }

    #[test]
    fn parse_hashes_accepts_empty_bytes() {
        assert!(parse_hashes(&Bytes(Vec::new())).unwrap().is_empty());
    }

    #[test]
    fn parse_hashes_rejects_incomplete_hashes() {
        for length in [1, 19, 21, 39, 41] {
            let error = expect_error(parse_hashes(&Bytes(vec![0; length])));
            assert!(
                matches!(error, MetainfoError::InvalidPiecesLength { .. }),
                "{length} bytes: {error:?}"
            );
        }
    }

    #[test]
    fn parse_hashes_rejects_non_bytes() {
        let error = expect_error(parse_hashes(&List(Vec::new())));
        assert!(
            matches!(error, MetainfoError::UnexpectedFieldType { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn parse_announce_list_requires_two_list_levels() {
        for value in [
            bytes(b"http://tracker"),
            List(vec![bytes(b"http://tracker")]),
            List(vec![List(vec![Integer(1)])]),
        ] {
            let error = expect_error(parse_announce_list(&value));
            assert!(
                matches!(error, MetainfoError::UnexpectedFieldType { .. }),
                "{error:?}"
            );
        }
    }

    #[test]
    fn parse_announce_list_preserves_utf8_error() {
        let value = List(vec![List(vec![bytes(b"\xff")])]);
        let error = expect_error(parse_announce_list(&value));
        assert!(
            matches!(error, MetainfoError::InvalidFieldUtf8 { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn parse_nodes_rejects_wrong_shapes() {
        for value in [Integer(1), List(vec![Integer(1)])] {
            let error = expect_error(parse_nodes(&value));
            assert!(
                matches!(error, MetainfoError::UnexpectedFieldType { .. }),
                "{error:?}"
            );
        }

        for node in [
            List(Vec::new()),
            List(vec![bytes(b"host")]),
            List(vec![bytes(b"host"), Integer(1), Integer(2)]),
        ] {
            let error = expect_error(parse_nodes(&List(vec![node])));
            assert!(
                matches!(error, MetainfoError::InvalidNodeLength { .. }),
                "{error:?}"
            );
        }
    }

    #[test]
    fn parse_nodes_rejects_empty_host() {
        let value = List(vec![List(vec![bytes(b""), Integer(6881)])]);
        let error = expect_error(parse_nodes(&value));
        assert!(
            matches!(error, MetainfoError::EmptyNodeHost { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn parse_nodes_rejects_invalid_port_values() {
        for port in [-1, 0, 65536, i64::MAX] {
            let value = List(vec![List(vec![bytes(b"host"), Integer(port)])]);
            let error = expect_error(parse_nodes(&value));
            assert!(
                matches!(error, MetainfoError::InvalidNodePort { .. }),
                "port {port}: {error:?}"
            );
        }
    }

    #[test]
    fn parse_nodes_preserves_type_and_utf8_errors() {
        let wrong_port = List(vec![List(vec![bytes(b"host"), bytes(b"6881")])]);
        let error = expect_error(parse_nodes(&wrong_port));
        assert!(
            matches!(error, MetainfoError::UnexpectedFieldType { .. }),
            "{error:?}"
        );

        let wrong_host = List(vec![List(vec![Integer(1), Integer(6881)])]);
        let error = expect_error(parse_nodes(&wrong_host));
        assert!(
            matches!(error, MetainfoError::UnexpectedFieldType { .. }),
            "{error:?}"
        );

        let invalid_utf8 = List(vec![List(vec![bytes(b"\xff"), Integer(6881)])]);
        let error = expect_error(parse_nodes(&invalid_utf8));
        assert!(
            matches!(error, MetainfoError::InvalidFieldUtf8 { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn parse_url_list_accepts_one_url_or_a_flat_list() {
        assert_eq!(
            parse_url_list(&bytes(b"https://mirror/")).unwrap(),
            vec!["https://mirror/".to_owned()]
        );
        assert_eq!(
            parse_url_list(&List(vec![bytes(b"https://a/"), bytes(b"https://b/")])).unwrap(),
            vec!["https://a/".to_owned(), "https://b/".to_owned()]
        );
        assert!(parse_url_list(&List(Vec::new())).unwrap().is_empty());
    }

    #[test]
    fn parse_url_list_rejects_wrong_types() {
        for value in [
            Integer(1),
            List(vec![Integer(1)]),
            List(vec![List(Vec::new())]),
        ] {
            let error = expect_error(parse_url_list(&value));
            assert!(
                matches!(error, MetainfoError::UnexpectedFieldType { .. }),
                "{error:?}"
            );
        }
    }

    #[test]
    fn parse_http_seeds_requires_a_flat_list() {
        assert_eq!(
            parse_http_seeds(&List(vec![bytes(b"https://seed/seed.php")])).unwrap(),
            vec!["https://seed/seed.php".to_owned()]
        );

        for value in [bytes(b"https://seed/seed.php"), List(vec![Integer(1)])] {
            let error = expect_error(parse_http_seeds(&value));
            assert!(
                matches!(error, MetainfoError::UnexpectedFieldType { .. }),
                "{error:?}"
            );
        }
    }

    #[test]
    fn seed_parsers_preserve_utf8_errors() {
        for value in [bytes(b"\xff"), List(vec![bytes(b"\xff")])] {
            let error = expect_error(parse_url_list(&value));
            assert!(
                matches!(error, MetainfoError::InvalidFieldUtf8 { .. }),
                "{error:?}"
            );
        }

        let error = expect_error(parse_http_seeds(&List(vec![bytes(b"\xff")])));
        assert!(
            matches!(error, MetainfoError::InvalidFieldUtf8 { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn invalid_pieces_length_reports_byte_count() {
        let error = expect_error(parse_hashes(&Bytes(vec![0; 21])));

        assert!(
            matches!(error, MetainfoError::InvalidPiecesLength { actual: 21 }),
            "{error:?}"
        );
        assert_eq!(
            error.to_string(),
            "Field 'info.pieces' must contain a multiple of 20 bytes, found 21 bytes"
        );
    }

    #[test]
    fn invalid_node_port_reports_node_index_value_and_valid_range() {
        let value = List(vec![
            List(vec![bytes(b"first.example"), Integer(6881)]),
            List(vec![bytes(b"second.example"), Integer(65536)]),
        ]);
        let error = expect_error(parse_nodes(&value));

        assert!(
            matches!(
                error,
                MetainfoError::InvalidNodePort {
                    index: 1,
                    value: 65536
                }
            ),
            "{error:?}"
        );
        assert_eq!(
            error.to_string(),
            "Field 'nodes[1][1]' must be a port in 1..=65535, found 65536"
        );
    }

    #[test]
    fn utf8_error_reports_tracker_indices_and_preserves_source() {
        let value = List(vec![
            List(vec![bytes(b"http://tracker")]),
            List(vec![bytes(b"\xff")]),
        ]);
        let error = expect_error(parse_announce_list(&value));

        assert!(
            matches!(
                error,
                MetainfoError::InvalidFieldUtf8 { ref field, .. }
                    if field == "announce-list[1][0]"
            ),
            "{error:?}"
        );
        assert!(
            error
                .to_string()
                .starts_with("Field 'announce-list[1][0]' is not valid UTF-8:")
        );
        let source = std::error::Error::source(&error).expect("original UTF-8 error");
        assert!(source.downcast_ref::<std::str::Utf8Error>().is_some());
    }
}
