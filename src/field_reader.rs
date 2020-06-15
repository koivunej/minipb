use std::convert::TryFrom;

use crate::{
    pb::*, DecodingError, FieldInfo, FieldValue, NeedMoreBytes, ReadField, Status, WireType,
};

#[derive(Default)]
pub struct FieldReader {
    field: Option<FieldInfo>,
}

impl FieldReader {
    /// Reads the first bytes as any field. After returning a length delimited field, the data must
    /// be skipped for 'ReadField::bytes_to_skip` to avoid interpreting the field as a nested message.
    pub fn next<'a>(
        &'a mut self,
        data: &[u8],
    ) -> Result<Result<ReadField<'a>, Status>, DecodingError> {
        // TODO: this needs to work on Bytes, or two slices to support ring buffers
        macro_rules! launder {
            ($x:expr) => {
                match $x {
                    Ok(x) => x,
                    Err(NeedMoreBytes) => return Ok(Err(Status::NeedMoreBytes)),
                }
            };
        }

        if data.is_empty() {
            return Ok(Err(Status::IdleAtEndOfBuffer));
        }

        let (consumed, tag) = launder!(read_varint32(data)?);

        let data = &data[consumed..];

        let field = tag >> 3;
        let kind = WireType::try_from(tag)?;

        let (additional, value) = match &kind {
            WireType::Varint => {
                let (consumed, val) = launder!(read_varint64(data)?);
                (consumed, FieldValue::Varint(val))
            }
            WireType::Fixed32 => {
                let (consumed, val) = launder!(read_fixed32(data));
                (consumed, FieldValue::Fixed32(val))
            }
            WireType::Fixed64 => {
                let (consumed, val) = launder!(read_fixed64(data));
                (consumed, FieldValue::Fixed64(val))
            }
            WireType::LengthDelimited => {
                let (consumed, len) = launder!(read_varint32(data)?);
                (consumed, FieldValue::DataLength(len))
            }
        };

        let consumed = consumed + additional;

        self.field = Some(FieldInfo {
            id: field,
            kind,
            value,
        });

        let field = self.field.as_ref().unwrap();

        Ok(Ok(ReadField { consumed, field }))
    }
}

#[cfg(test)]
mod tests {
    use super::FieldReader;
    use crate::{FieldValue, Status};
    use hex_literal::hex;

    #[test]
    fn read_basic_fields() {
        use FieldValue::*;
        let expected = &[
            (Varint(42), 2, 0),
            (Varint(242), 3, 0),
            (Varint(22_242), 4, 0),
            (Varint(2_097_153), 5, 0),
            (Varint(268_435_457), 6, 0),
            (Varint(34_359_738_369), 7, 0),
            (Varint(4_398_046_511_105), 8, 0),
            (Varint(562_949_953_421_313), 9, 0),
            (Varint(u64::max_value()), 11, 0),
            (Fixed64(1), 9, 0),
            (Fixed32(1), 5, 0),
            (DataLength(1), 2, 1),
            (DataLength(242), 3, 242),
            (DataLength(22242), 4, 22242),
            (DataLength(u32::max_value()), 6, u32::max_value() as usize),
            // longer fields are not supported
        ];

        let mut buffer = Vec::new();

        for (field, expected_consumed, expected_field_len) in expected {
            buffer.extend(field.output_with_field_id(1));

            let mut fr = FieldReader::default();

            let read_field = fr.next(&buffer[..]).unwrap().unwrap();
            assert_eq!(read_field.consumed, *expected_consumed);
            assert_eq!(read_field.field_id(), 1);
            assert_eq!(read_field.field_len(), *expected_field_len);

            // select non-empty slices but incomplete starting from the beginning to make sure that
            // all those report back NeedMoreBytes
            for end_byte in 1..(buffer.len() - 1) {
                let need_more = fr.next(&buffer[..end_byte]).unwrap().unwrap_err();
                assert!(matches!(need_more, Status::NeedMoreBytes));
            }

            buffer.clear();
        }
    }

    #[test]
    fn read_good_pb() {
        let input = hex!("0a200804121c2e2e2f2e2e2f2e2e2f617263682f61726d36342f626f6f742f647473");

        let mut fr = FieldReader::default();

        let f = fr.next(&input[..]).unwrap().unwrap();
        assert_eq!(f.consumed, 2);
        assert_eq!(f.field_id(), 1);
        assert_eq!(f.field_len(), 32);
        assert!(
            matches!(f.value(), FieldValue::DataLength(32)),
            "{:?}",
            f.value()
        );

        let f = fr.next(&input[2..]).unwrap().unwrap();
        assert_eq!(f.consumed, 2);
        assert_eq!(f.field_id(), 1);
        assert_eq!(f.field_len(), 0); // well this is a bit interesting value..?
        assert!(
            matches!(f.value(), FieldValue::Varint(4)),
            "{:?}",
            f.value()
        );

        let f = fr.next(&input[4..]).unwrap().unwrap();
        assert_eq!(f.consumed, 2);
        assert_eq!(f.field_id(), 2);
        assert_eq!(f.field_len(), 28);
        assert!(
            matches!(f.value(), FieldValue::DataLength(28)),
            "{:?}",
            f.value()
        );

        assert_eq!(
            std::str::from_utf8(&input[6..]),
            Ok("../../../arch/arm64/boot/dts")
        );
    }
}
