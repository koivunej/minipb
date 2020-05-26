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
    use std::io::{Cursor, Seek, SeekFrom};
    use crate::{FieldValue, Status};
    use super::FieldReader;

    #[test]
    fn read_basic_fields() {
        let expected = &[
            (FieldValue::Varint(42), 2 /* bytes consumed */, 0 /* field len */),
            (FieldValue::Varint(242), 3, 0),
            (FieldValue::Varint(22242), 4, 0),
            (FieldValue::Varint(2097153), 5, 0),
            (FieldValue::Varint(268435457), 6, 0),
            (FieldValue::Varint(34359738369), 7, 0),
            (FieldValue::Varint(4398046511105), 8, 0),
            (FieldValue::Varint(562949953421313), 9, 0),
            (FieldValue::Varint(u64::max_value()), 11, 0),
            (FieldValue::Fixed64(1), 9, 0),
            (FieldValue::Fixed32(1), 5, 0),
            (FieldValue::DataLength(1), 2, 1),
            (FieldValue::DataLength(242), 3, 242),
            (FieldValue::DataLength(22242), 4, 22242),
            (FieldValue::DataLength(u32::max_value()), 6, u32::max_value() as usize),
            // longer fields are not supported
        ];

        let mut buffer = Vec::new();

        for (field, expected_consumed, expected_field_len) in expected {

            let mut cursor = std::io::Cursor::new(&mut buffer);
            field.output_with_field_id(1, &mut cursor).unwrap();

            cursor.seek(SeekFrom::Start(0)).unwrap();

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
}
