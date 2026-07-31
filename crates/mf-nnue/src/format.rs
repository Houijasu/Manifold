// Derived from Eonego source identified in `THIRD_PARTY_NOTICES/Eonego.txt`.
// Eonego's copyright and MIT license notice are reproduced there.
// This port is part of Manifold and is distributed under GPL-3.0-only.

use std::fmt;

pub(crate) const LEB128_MAGIC: &[u8; 17] = b"COMPRESSED_LEB128";

#[derive(Clone, Copy)]
pub(crate) enum ArrayEncoding {
    Raw,
    SignedLeb128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FormatError {
    UnexpectedEof,
    InvalidSignedLeb128,
    ValueOutOfRange { value: i64, target: &'static str },
}

impl fmt::Display for FormatError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEof => formatter.write_str("unexpected end of NNUE data"),
            Self::InvalidSignedLeb128 => formatter.write_str("invalid signed LEB128 value"),
            Self::ValueOutOfRange { value, target } => {
                write!(formatter, "value {value} is out of range for {target}")
            }
        }
    }
}

impl std::error::Error for FormatError {}

pub(crate) struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    pub(crate) const fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }

    pub(crate) fn read_bytes(&mut self, length: usize) -> Result<&'a [u8], FormatError> {
        let end = self
            .position
            .checked_add(length)
            .filter(|&end| end <= self.bytes.len())
            .ok_or(FormatError::UnexpectedEof)?;
        let result = &self.bytes[self.position..end];
        self.position = end;
        Ok(result)
    }

    pub(crate) fn read_u32(&mut self) -> Result<u32, FormatError> {
        let bytes: [u8; 4] = self.read_bytes(4)?.try_into().expect("four-byte slice");
        Ok(u32::from_le_bytes(bytes))
    }

    pub(crate) fn read_i32(&mut self) -> Result<i32, FormatError> {
        Ok(self.read_u32()? as i32)
    }

    pub(crate) fn read_signed_leb128(&mut self) -> Result<i64, FormatError> {
        let mut result = 0_u64;
        let mut shift = 0_u32;

        for index in 0..10 {
            let byte = *self.read_bytes(1)?.first().expect("one-byte slice");
            let payload = u64::from(byte & 0x7f);

            if shift == 63 && payload > 1 {
                return Err(FormatError::InvalidSignedLeb128);
            }
            result |= payload << shift;

            if byte & 0x80 == 0 {
                let encoded_bits = shift + 7;
                if encoded_bits < 64 && byte & 0x40 != 0 {
                    result |= u64::MAX << encoded_bits;
                }
                return Ok(result as i64);
            }

            if index == 9 {
                return Err(FormatError::InvalidSignedLeb128);
            }
            shift += 7;
        }

        unreachable!("the loop returns after at most ten bytes")
    }

    pub(crate) fn begin_array(&mut self) -> Result<ArrayEncoding, FormatError> {
        if self
            .bytes
            .get(self.position..)
            .is_some_and(|remaining| remaining.starts_with(LEB128_MAGIC))
        {
            self.read_bytes(LEB128_MAGIC.len())?;
            self.read_i32()?;
            Ok(ArrayEncoding::SignedLeb128)
        } else {
            Ok(ArrayEncoding::Raw)
        }
    }

    pub(crate) fn read_i8_into(&mut self, output: &mut [i8]) -> Result<(), FormatError> {
        let encoding = self.begin_array()?;
        self.read_i8_into_with_encoding(output, encoding)
    }

    pub(crate) fn read_i8_into_with_encoding(
        &mut self,
        output: &mut [i8],
        encoding: ArrayEncoding,
    ) -> Result<(), FormatError> {
        match encoding {
            ArrayEncoding::Raw => {
                let length = output.len();
                for (output, &value) in output.iter_mut().zip(self.read_bytes(length)?) {
                    *output = value as i8;
                }
            }
            ArrayEncoding::SignedLeb128 => {
                for output in output {
                    let value = self.read_signed_leb128()?;
                    *output = i8::try_from(value).map_err(|_| FormatError::ValueOutOfRange {
                        value,
                        target: "i8",
                    })?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn read_i16_into(&mut self, output: &mut [i16]) -> Result<(), FormatError> {
        let encoding = self.begin_array()?;
        self.read_i16_into_with_encoding(output, encoding)
    }

    pub(crate) fn read_i16_into_with_encoding(
        &mut self,
        output: &mut [i16],
        encoding: ArrayEncoding,
    ) -> Result<(), FormatError> {
        match encoding {
            ArrayEncoding::Raw => {
                for output in output {
                    let bytes: [u8; 2] = self.read_bytes(2)?.try_into().expect("two-byte slice");
                    *output = i16::from_le_bytes(bytes);
                }
            }
            ArrayEncoding::SignedLeb128 => {
                for output in output {
                    let value = self.read_signed_leb128()?;
                    *output = i16::try_from(value).map_err(|_| FormatError::ValueOutOfRange {
                        value,
                        target: "i16",
                    })?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn read_i32_into(&mut self, output: &mut [i32]) -> Result<(), FormatError> {
        let encoding = self.begin_array()?;
        match encoding {
            ArrayEncoding::Raw => {
                for output in output {
                    *output = self.read_i32()?;
                }
            }
            ArrayEncoding::SignedLeb128 => {
                for output in output {
                    let value = self.read_signed_leb128()?;
                    *output = i32::try_from(value).map_err(|_| FormatError::ValueOutOfRange {
                        value,
                        target: "i32",
                    })?;
                }
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn read_i8_array(&mut self, length: usize) -> Result<Vec<i8>, FormatError> {
        let mut output = vec![0; length];
        self.read_i8_into(&mut output)?;
        Ok(output)
    }

    #[cfg(test)]
    fn read_i16_array(&mut self, length: usize) -> Result<Vec<i16>, FormatError> {
        let mut output = vec![0; length];
        self.read_i16_into(&mut output)?;
        Ok(output)
    }

    #[cfg(test)]
    fn read_i32_array(&mut self, length: usize) -> Result<Vec<i32>, FormatError> {
        let mut output = vec![0; length];
        self.read_i32_into(&mut output)?;
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::{Cursor, LEB128_MAGIC};

    fn compressed(values: &[u8]) -> Vec<u8> {
        let mut bytes = LEB128_MAGIC.to_vec();
        bytes.extend_from_slice(&123_i32.to_le_bytes());
        bytes.extend_from_slice(values);
        bytes
    }

    #[test]
    fn signed_leb128_decodes_positive_values() {
        let mut cursor = Cursor::new(&[0xe5, 0x8e, 0x26]);
        assert_eq!(cursor.read_signed_leb128().unwrap(), 624_485);
    }

    #[test]
    fn signed_leb128_decodes_negative_values_with_sign_extension() {
        let mut cursor = Cursor::new(&[0x9b, 0xf1, 0x59]);
        assert_eq!(cursor.read_signed_leb128().unwrap(), -624_485);

        let mut cursor = Cursor::new(&[0x7f]);
        assert_eq!(cursor.read_signed_leb128().unwrap(), -1);
    }

    #[test]
    fn each_i16_array_detects_compressed_or_raw_encoding() {
        let bytes = compressed(&[0x01, 0x7e]);
        let mut compressed_cursor = Cursor::new(&bytes);
        assert_eq!(compressed_cursor.read_i16_array(2).unwrap(), vec![1, -2]);

        let mut raw_cursor = Cursor::new(&[0x34, 0x12, 0xfe, 0xff]);
        assert_eq!(raw_cursor.read_i16_array(2).unwrap(), vec![0x1234, -2]);
    }

    #[test]
    fn each_i8_array_detects_compressed_or_raw_encoding() {
        let bytes = compressed(&[0x40, 0x3f]);
        let mut compressed_cursor = Cursor::new(&bytes);
        assert_eq!(compressed_cursor.read_i8_array(2).unwrap(), vec![-64, 63]);

        let mut raw_cursor = Cursor::new(&[0x80, 0x7f]);
        assert_eq!(raw_cursor.read_i8_array(2).unwrap(), vec![-128, 127]);
    }

    #[test]
    fn each_i32_array_detects_compressed_or_raw_encoding() {
        let bytes = compressed(&[0x00, 0x7f]);
        let mut compressed_cursor = Cursor::new(&bytes);
        assert_eq!(compressed_cursor.read_i32_array(2).unwrap(), vec![0, -1]);

        let mut raw_cursor = Cursor::new(&[0x78, 0x56, 0x34, 0x12, 0xff, 0xff, 0xff, 0xff]);
        assert_eq!(raw_cursor.read_i32_array(2).unwrap(), vec![0x1234_5678, -1]);
    }

    #[test]
    fn compressed_byte_count_is_consumed_but_not_enforced() {
        let mut bytes = LEB128_MAGIC.to_vec();
        bytes.extend_from_slice(&(-1_i32).to_le_bytes());
        bytes.push(0x2a);

        let mut cursor = Cursor::new(&bytes);
        assert_eq!(cursor.read_i8_array(1).unwrap(), vec![42]);
        assert_eq!(cursor.remaining(), 0);
    }
}
