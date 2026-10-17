use std::collections::BTreeMap;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("Unexpected end of bencode data at byte offset {offset}")]
    UnexpectedEof { offset: usize },

    #[error("Invalid bencode prefix 0x{byte:02x} at byte offset {offset}")]
    InvalidPrefix { byte: u8, offset: usize },

    #[error("Integer starting at byte offset {offset} is missing its terminating 'e'")]
    UnterminatedInteger { offset: usize },

    #[error(
        "Invalid integer at byte offset {offset}: expected a canonical decimal integer fitting in i64"
    )]
    InvalidInteger { offset: usize },

    #[error("Dictionary key at byte offset {offset} must be a byte string")]
    InvalidDictionaryKey { offset: usize },

    #[error("Duplicate dictionary key at byte offset {offset}")]
    DuplicateDictionaryKey { offset: usize },

    #[error(
        "Dictionary key at byte offset {offset} is out of order; keys must be sorted by raw bytes"
    )]
    InvalidDictionaryKeyOrder { offset: usize },

    #[error("Byte string at byte offset {offset} is missing ':' after its length")]
    MissingByteStringSeparator { offset: usize },

    #[error("Invalid or overflowing byte-string length at byte offset {offset}")]
    InvalidByteStringLength { offset: usize },

    #[error(
        "Byte string at byte offset {offset} declares {expected} bytes, but only {available} remain"
    )]
    TruncatedByteString {
        offset: usize,
        expected: usize,
        available: usize,
    },

    #[error("Trailing data after the bencoded value at byte offset {offset}")]
    TrailingData { offset: usize },
}

#[derive(Debug, PartialEq, Eq)]
pub enum BencodeValue {
    Integer(i64),
    Bytes(Vec<u8>),
    List(Vec<BencodeValue>),
    Dictionary(BTreeMap<Vec<u8>, BencodeValue>),
}

/// Decodes one bencoded value and rejects trailing data.
pub fn decode(data: &[u8]) -> Result<BencodeValue, DecodeError> {
    let (result, index) = bdecode(data, 0)?;

    if index != data.len() {
        return Err(DecodeError::TrailingData { offset: index });
    }

    Ok(result)
}

/// Decodes a bencoded value starting at index and returns the next byte offset.
pub fn bdecode(data: &[u8], index: usize) -> Result<(BencodeValue, usize), DecodeError> {
    let mut idx = index;

    if idx >= data.len() {
        return Err(DecodeError::UnexpectedEof { offset: idx });
    }

    match data[idx] {
        b'i' => {
            let end_idx = data[idx + 1..]
                .iter()
                .position(|&byte| byte == b'e')
                .map(|pos| idx + 1 + pos)
                .ok_or(DecodeError::UnterminatedInteger { offset: idx })?;

            let num_bytes = &data[idx + 1..end_idx];

            if num_bytes.starts_with(b"0") && num_bytes.len() > 1
                || num_bytes.starts_with(b"-0")
                || num_bytes.starts_with(b"+")
            {
                return Err(DecodeError::InvalidInteger { offset: idx });
            }

            let num_str = str::from_utf8(num_bytes)
                .map_err(|_| DecodeError::InvalidInteger { offset: idx })?;
            let number = num_str
                .parse::<i64>()
                .map_err(|_| DecodeError::InvalidInteger { offset: idx })?;

            Ok((BencodeValue::Integer(number), end_idx + 1))
        }
        b'l' => {
            idx += 1;
            let mut items = Vec::new();
            loop {
                match data.get(idx) {
                    Some(b'e') => break,
                    Some(_) => {
                        let (item, next_idx) = bdecode(data, idx)?;
                        items.push(item);
                        idx = next_idx;
                    }
                    None => return Err(DecodeError::UnexpectedEof { offset: idx }),
                }
            }
            Ok((BencodeValue::List(items), idx + 1))
        }
        b'd' => {
            idx += 1;
            let mut dict: BTreeMap<Vec<u8>, BencodeValue> = BTreeMap::new();
            loop {
                match data.get(idx) {
                    Some(b'e') => break,
                    Some(_) => {
                        let (key, next_idx) = bdecode(data, idx)?;
                        let BencodeValue::Bytes(key_bytes) = key else {
                            return Err(DecodeError::InvalidDictionaryKey { offset: idx });
                        };

                        if let Some((last_key, _)) = dict.last_key_value() {
                            if key_bytes == *last_key {
                                return Err(DecodeError::DuplicateDictionaryKey { offset: idx });
                            }
                            if key_bytes < *last_key {
                                return Err(DecodeError::InvalidDictionaryKeyOrder { offset: idx });
                            }
                        }

                        idx = next_idx;
                        let (value, next_idx) = bdecode(data, idx)?;
                        dict.insert(key_bytes, value);
                        idx = next_idx;
                    }
                    None => return Err(DecodeError::UnexpectedEof { offset: idx }),
                }
            }
            Ok((BencodeValue::Dictionary(dict), idx + 1))
        }
        b'0'..=b'9' => {
            let sep_idx = data[idx..]
                .iter()
                .position(|&byte| byte == b':')
                .map(|pos| idx + pos)
                .ok_or(DecodeError::MissingByteStringSeparator { offset: idx })?;

            let len_bytes = &data[idx..sep_idx];
            if len_bytes.starts_with(b"0") && len_bytes.len() > 1 {
                return Err(DecodeError::InvalidByteStringLength { offset: idx });
            }

            let len_str = str::from_utf8(len_bytes)
                .map_err(|_| DecodeError::InvalidByteStringLength { offset: idx })?;
            let length = len_str
                .parse::<usize>()
                .map_err(|_| DecodeError::InvalidByteStringLength { offset: idx })?;

            let start_idx = sep_idx + 1;
            let end_idx = start_idx
                .checked_add(length)
                .ok_or(DecodeError::InvalidByteStringLength { offset: idx })?;

            let bytes = data
                .get(start_idx..end_idx)
                .ok_or(DecodeError::TruncatedByteString {
                    offset: idx,
                    expected: length,
                    available: data.len() - start_idx,
                })?
                .to_vec();

            Ok((BencodeValue::Bytes(bytes), end_idx))
        }
        byte => Err(DecodeError::InvalidPrefix { byte, offset: idx }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    // === INTEGER ===

    #[test]
    fn decode_positive_integer() {
        let (value, index) = bdecode(b"i42e", 0).unwrap();

        assert_eq!(value, BencodeValue::Integer(42));
        assert_eq!(index, 4);
    }

    #[test]
    fn decode_negative_integer() {
        let (value, index) = bdecode(b"i-123e", 0).unwrap();

        assert_eq!(value, BencodeValue::Integer(-123));
        assert_eq!(index, 6);
    }

    #[test]
    fn decode_zero() {
        let (value, index) = bdecode(b"i0e", 0).unwrap();

        assert_eq!(value, BencodeValue::Integer(0));
        assert_eq!(index, 3);
    }

    // === BYTE STRINGS ===

    #[test]
    fn decode_string() {
        let (value, index) = bdecode(b"5:hello", 0).unwrap();

        assert_eq!(value, BencodeValue::Bytes(b"hello".to_vec()));
        assert_eq!(index, 7);
    }

    #[test]
    fn decode_empty_string() {
        let (value, index) = bdecode(b"0:", 0).unwrap();

        assert_eq!(value, BencodeValue::Bytes(vec![]));
        assert_eq!(index, 2);
    }

    #[test]
    fn decode_binary_bytes() {
        let data = b"3:\x00\xff\x80";
        let (value, index) = bdecode(data, 0).unwrap();

        assert_eq!(value, BencodeValue::Bytes(vec![0, 255, 128]));
        assert_eq!(index, 5);
    }

    // === LISTS ===

    #[test]
    fn decode_empty_list() {
        let (value, index) = bdecode(b"le", 0).unwrap();

        assert_eq!(value, BencodeValue::List(vec![]));
        assert_eq!(index, 2);
    }

    #[test]
    fn decode_list() {
        let data = b"li42e3:fooe";
        let (value, index) = bdecode(data, 0).unwrap();

        assert_eq!(
            value,
            BencodeValue::List(vec![
                BencodeValue::Integer(42),
                BencodeValue::Bytes(b"foo".to_vec()),
            ])
        );
        assert_eq!(index, data.len());
    }

    #[test]
    fn decode_nested_list() {
        let data = b"lli1ei2eei3ee";
        let (value, index) = bdecode(data, 0).unwrap();

        assert_eq!(
            value,
            BencodeValue::List(vec![
                BencodeValue::List(vec![BencodeValue::Integer(1), BencodeValue::Integer(2),]),
                BencodeValue::Integer(3),
            ])
        );
        assert_eq!(index, data.len());
    }

    // === DICTIONARIES ===

    #[test]
    fn decode_empty_dictionary() {
        let (value, index) = bdecode(b"de", 0).unwrap();

        assert_eq!(value, BencodeValue::Dictionary(BTreeMap::new()));
        assert_eq!(index, 2);
    }

    #[test]
    fn decode_dictionary() {
        let data = b"d3:foo3:bar5:helloi42ee";
        let (value, index) = bdecode(data, 0).unwrap();

        let mut expected = BTreeMap::new();
        expected.insert(b"foo".to_vec(), BencodeValue::Bytes(b"bar".to_vec()));
        expected.insert(b"hello".to_vec(), BencodeValue::Integer(42));

        assert_eq!(value, BencodeValue::Dictionary(expected));
        assert_eq!(index, data.len());
    }

    #[test]
    fn decode_nested_dictionary() {
        let data = b"d4:infod4:name4:testee";
        let (value, index) = bdecode(data, 0).unwrap();

        let mut inner = BTreeMap::new();
        inner.insert(b"name".to_vec(), BencodeValue::Bytes(b"test".to_vec()));

        let mut outer = BTreeMap::new();
        outer.insert(b"info".to_vec(), BencodeValue::Dictionary(inner));

        assert_eq!(value, BencodeValue::Dictionary(outer));
        assert_eq!(index, data.len());
    }

    // === INDEX HANDLING ===

    #[test]
    fn decode_from_nonzero_index() {
        let data = b"5:helloi42e";

        let (value, index) = bdecode(data, 7).unwrap();

        assert_eq!(value, BencodeValue::Integer(42));
        assert_eq!(index, data.len());
    }

    #[test]
    fn decode_string_from_nonzero_index() {
        let data = b"i42e5:hello";

        let (value, index) = bdecode(data, 4).unwrap();

        assert_eq!(value, BencodeValue::Bytes(b"hello".to_vec()));
        assert_eq!(index, data.len());
    }

    #[test]
    fn decode_multiple_values_sequentially() {
        let data = b"i10ei20ei30e";
        let mut index = 0;

        for expected in [10, 20, 30] {
            let (value, next_index) = bdecode(data, index).unwrap();

            assert_eq!(value, BencodeValue::Integer(expected));
            assert!(next_index > index);

            index = next_index;
        }

        assert_eq!(index, data.len());
    }

    // === INVALID INPUT ===

    #[test]
    fn reject_empty_input() {
        assert!(bdecode(b"", 0).is_err());
    }

    #[test]
    fn reject_unknown_prefix() {
        assert!(bdecode(b"x123", 0).is_err());
    }

    #[test]
    fn reject_unterminated_integer() {
        assert!(bdecode(b"i123", 0).is_err());
    }

    #[test]
    fn reject_invalid_integer() {
        assert!(bdecode(b"i12xe", 0).is_err());
    }

    #[test]
    fn reject_incomplete_string() {
        assert!(bdecode(b"10:hello", 0).is_err());
    }

    #[test]
    fn reject_unterminated_list() {
        assert!(bdecode(b"li42e", 0).is_err());
    }

    #[test]
    fn reject_unterminated_dictionary() {
        assert!(bdecode(b"d3:foo3:bar", 0).is_err());
    }

    #[test]
    fn reject_invalid_dictionary_key() {
        assert!(bdecode(b"di42e3:fooe", 0).is_err());
    }

    // === STRICT BENCODE VALIDATION ===

    #[test]
    fn reject_integer_leading_zero() {
        assert!(bdecode(b"i042e", 0).is_err());
    }

    #[test]
    fn reject_negative_zero() {
        assert!(bdecode(b"i-0e", 0).is_err());
    }

    #[test]
    fn reject_explicitly_pozitive_number() {
        assert!(bdecode(b"i+42e", 0).is_err());
    }

    #[test]
    fn reject_string_length_leading_zero() {
        assert!(bdecode(b"03:abc", 0).is_err());
    }

    #[test]
    fn reject_duplicate_dictionary_keys() {
        assert!(bdecode(b"d3:fooi1e3:fooi2ee", 0).is_err());
    }

    #[test]
    fn reject_unsorted_dictionary_keys() {
        assert!(bdecode(b"d1:b1:x1:a1:ye", 0).is_err());
    }

    #[test]
    fn invalid_prefix_reports_byte_and_offset() {
        let error = decode(b"li1exe").unwrap_err();
        assert!(
            matches!(
                error,
                DecodeError::InvalidPrefix {
                    byte: b'x',
                    offset: 4
                }
            ),
            "{error:?}"
        );
        assert_eq!(
            error.to_string(),
            "Invalid bencode prefix 0x78 at byte offset 4"
        );
    }

    #[test]
    fn duplicate_and_unsorted_dictionary_keys_have_distinct_errors() {
        let duplicate = decode(b"d1:ai1e1:ai2ee").unwrap_err();
        assert!(
            matches!(duplicate, DecodeError::DuplicateDictionaryKey { offset: 7 }),
            "{duplicate:?}"
        );

        let unsorted = decode(b"d1:b1:x1:a1:ye").unwrap_err();
        assert!(
            matches!(
                unsorted,
                DecodeError::InvalidDictionaryKeyOrder { offset: 7 }
            ),
            "{unsorted:?}"
        );
    }

    #[test]
    fn truncated_byte_string_reports_declared_and_available_lengths() {
        let error = decode(b"5:abc").unwrap_err();
        assert!(
            matches!(
                error,
                DecodeError::TruncatedByteString {
                    offset: 0,
                    expected: 5,
                    available: 3,
                }
            ),
            "{error:?}"
        );
        assert_eq!(
            error.to_string(),
            "Byte string at byte offset 0 declares 5 bytes, but only 3 remain"
        );
    }

    #[test]
    fn trailing_data_reports_first_unconsumed_offset() {
        let error = decode(b"i1ex").unwrap_err();
        assert!(
            matches!(error, DecodeError::TrailingData { offset: 3 }),
            "{error:?}"
        );
        assert_eq!(
            error.to_string(),
            "Trailing data after the bencoded value at byte offset 3"
        );
    }

    #[test]
    fn invalid_byte_string_length_is_not_an_integer_error() {
        let error = decode(b"03:abc").unwrap_err();
        assert!(
            matches!(error, DecodeError::InvalidByteStringLength { offset: 0 }),
            "{error:?}"
        );
    }

    #[test]
    fn byte_string_without_separator_reports_its_start() {
        let error = decode(b"123").unwrap_err();
        assert!(
            matches!(error, DecodeError::MissingByteStringSeparator { offset: 0 }),
            "{error:?}"
        );
    }

    #[test]
    fn non_utf8_integer_reports_invalid_integer() {
        let error = decode(b"i\xffe").unwrap_err();
        assert!(
            matches!(error, DecodeError::InvalidInteger { offset: 0 }),
            "{error:?}"
        );
    }
}
