use std::collections::BTreeMap;

use thiserror::Error;

use super::{BencodeValue, value_type};

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("Required field '{field}' is missing")]
    MissingField { field: String },

    #[error("Field '{field}': expected {expected}, found {actual}")]
    UnexpectedFieldType {
        field: String,
        expected: &'static str,
        actual: &'static str,
    },

    #[error("Field '{field}' is not valid UTF-8: {source}")]
    InvalidFieldUtf8 {
        field: String,
        #[source]
        source: std::str::Utf8Error,
    },
}

pub fn missing_field(field: &str) -> ParseError {
    ParseError::MissingField {
        field: field.to_owned(),
    }
}

pub fn unexpected_type(value: &BencodeValue, field: &str, expected: &'static str) -> ParseError {
    ParseError::UnexpectedFieldType {
        field: field.to_owned(),
        expected,
        actual: value_type(value),
    }
}

pub fn parse_bytes<'a>(value: &'a BencodeValue, field: &str) -> Result<&'a [u8], ParseError> {
    let BencodeValue::Bytes(bytes) = value else {
        return Err(unexpected_type(value, field, "byte string"));
    };

    Ok(bytes)
}

pub fn parse_string(value: &BencodeValue, field: &str) -> Result<String, ParseError> {
    let bytes = parse_bytes(value, field)?;
    let text = str::from_utf8(bytes).map_err(|source| ParseError::InvalidFieldUtf8 {
        field: field.to_owned(),
        source,
    })?;

    Ok(text.to_owned())
}

pub fn parse_integer(value: &BencodeValue, field: &str) -> Result<i64, ParseError> {
    let BencodeValue::Integer(number) = value else {
        return Err(unexpected_type(value, field, "integer"));
    };

    Ok(*number)
}

pub fn parse_list<'a>(
    value: &'a BencodeValue,
    field: &str,
) -> Result<&'a [BencodeValue], ParseError> {
    let BencodeValue::List(items) = value else {
        return Err(unexpected_type(value, field, "list"));
    };

    Ok(items)
}

pub fn parse_dictionary<'a>(
    value: &'a BencodeValue,
    field: &str,
) -> Result<&'a BTreeMap<Vec<u8>, BencodeValue>, ParseError> {
    let BencodeValue::Dictionary(fields) = value else {
        return Err(unexpected_type(value, field, "dictionary"));
    };

    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    use BencodeValue::{Bytes, Dictionary, Integer, List};

    #[test]
    fn bytes_accept_binary_data_without_utf8_validation() {
        let value = Bytes(vec![0, 0xff, 0x80]);
        assert_eq!(parse_bytes(&value, "pieces").unwrap(), &[0, 0xff, 0x80]);
        assert!(
            parse_bytes(&Bytes(Vec::new()), "pieces")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn strings_accept_utf8_and_empty_values() {
        for text in ["", "test.bin", "файл.bin"] {
            let value = Bytes(text.as_bytes().to_vec());
            assert_eq!(parse_string(&value, "name").unwrap(), text);
        }
    }

    #[test]
    fn integers_preserve_sign_and_full_range() {
        for number in [i64::MIN, -1, 0, 1, i64::MAX] {
            assert_eq!(parse_integer(&Integer(number), "value").unwrap(), number);
        }
    }

    #[test]
    fn collections_preserve_nested_and_binary_values() {
        let value = Dictionary(BTreeMap::from([(
            vec![0xff],
            List(vec![Integer(-1), Bytes(vec![0xff])]),
        )]));
        let fields = parse_dictionary(&value, "root").unwrap();
        let items = parse_list(&fields[&vec![0xff]], "root.items").unwrap();
        assert_eq!(items, &[Integer(-1), Bytes(vec![0xff])]);
        assert!(parse_list(&List(Vec::new()), "items").unwrap().is_empty());
        assert!(
            parse_dictionary(&Dictionary(BTreeMap::new()), "root")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn type_errors_preserve_field_expected_and_actual_types() {
        let value = Integer(1);
        let errors = [
            (
                parse_bytes(&value, "nested.field").unwrap_err(),
                "byte string",
                "integer",
            ),
            (
                parse_string(&value, "nested.field").unwrap_err(),
                "byte string",
                "integer",
            ),
            (
                parse_list(&value, "nested.field").unwrap_err(),
                "list",
                "integer",
            ),
            (
                parse_dictionary(&value, "nested.field").unwrap_err(),
                "dictionary",
                "integer",
            ),
            (
                parse_integer(&Bytes(Vec::new()), "nested.field").unwrap_err(),
                "integer",
                "byte string",
            ),
        ];

        for (error, expected_type, actual_type) in errors {
            assert!(matches!(
                error,
                ParseError::UnexpectedFieldType { ref field, expected, actual }
                    if field == "nested.field" && expected == expected_type && actual == actual_type
            ));
            assert_eq!(
                error.to_string(),
                format!("Field 'nested.field': expected {expected_type}, found {actual_type}")
            );
        }
    }

    #[test]
    fn invalid_utf8_preserves_field_and_original_source() {
        let error = parse_string(&Bytes(vec![b'a', 0xff]), "peers[2].ip").unwrap_err();
        assert!(matches!(
            error,
            ParseError::InvalidFieldUtf8 { ref field, source }
                if field == "peers[2].ip" && source.valid_up_to() == 1
        ));
        let source = std::error::Error::source(&error).expect("original UTF-8 error");
        assert!(source.downcast_ref::<std::str::Utf8Error>().is_some());
    }
}
