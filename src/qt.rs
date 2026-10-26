use std::collections::BTreeMap;
use std::io::Read;

use base64::Engine;
use serde_json::{Map, Number, Value};

use crate::Error;

const MAX_BYTE_ARRAY: u64 = 2 * 1024 * 1024 * 1024;
const MAX_STRING_BYTES: u32 = 64 * 1024 * 1024;
const MAX_CONTAINER_ITEMS: u32 = 1_000_000;
const MAX_VARIANT_DEPTH: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamVersion {
    Qt4_7,
    Qt5_15,
}

#[derive(Clone, Debug, PartialEq)]
pub enum QtValue {
    Null,
    Bool(bool),
    I64(i64),
    U64(u64),
    F64(f64),
    String(String),
    Bytes(Vec<u8>),
    List(Vec<QtValue>),
    Map(BTreeMap<String, QtValue>),
}

impl QtValue {
    pub fn to_json(&self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Bool(value) => Value::Bool(*value),
            Self::I64(value) => Value::Number((*value).into()),
            Self::U64(value) => Value::Number((*value).into()),
            Self::F64(value) => Number::from_f64(*value)
                .map(Value::Number)
                .unwrap_or(Value::Null),
            Self::String(value) => Value::String(value.clone()),
            Self::Bytes(value) => {
                let mut object = Map::new();
                object.insert("qt_type".into(), Value::String("QByteArray".into()));
                object.insert(
                    "base64".into(),
                    Value::String(base64::engine::general_purpose::STANDARD.encode(value)),
                );
                Value::Object(object)
            }
            Self::List(values) => Value::Array(values.iter().map(Self::to_json).collect()),
            Self::Map(values) => Value::Object(
                values
                    .iter()
                    .map(|(key, value)| (key.clone(), value.to_json()))
                    .collect(),
            ),
        }
    }
}

pub struct QtReader<R> {
    inner: R,
    version: StreamVersion,
    offset: u64,
}

impl<R: Read> QtReader<R> {
    pub fn new(inner: R, version: StreamVersion) -> Self {
        Self {
            inner,
            version,
            offset: 0,
        }
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn version(&self) -> StreamVersion {
        self.version
    }

    pub fn read_optional_u8(&mut self) -> Result<Option<u8>, Error> {
        let mut byte = [0_u8; 1];
        match self.inner.read(&mut byte)? {
            0 => Ok(None),
            1 => {
                self.offset += 1;
                Ok(Some(byte[0]))
            }
            _ => unreachable!(),
        }
    }

    pub fn read_u8(&mut self) -> Result<u8, Error> {
        let mut value = [0_u8; 1];
        self.read_exact(&mut value, "byte")?;
        Ok(value[0])
    }

    pub fn read_bool(&mut self) -> Result<bool, Error> {
        Ok(self.read_u8()? != 0)
    }

    pub fn read_u16(&mut self) -> Result<u16, Error> {
        let mut value = [0_u8; 2];
        self.read_exact(&mut value, "uint16")?;
        Ok(u16::from_be_bytes(value))
    }

    pub fn read_u32(&mut self) -> Result<u32, Error> {
        let mut value = [0_u8; 4];
        self.read_exact(&mut value, "uint32")?;
        Ok(u32::from_be_bytes(value))
    }

    pub fn read_i32(&mut self) -> Result<i32, Error> {
        Ok(self.read_u32()? as i32)
    }

    pub fn read_u64(&mut self) -> Result<u64, Error> {
        let mut value = [0_u8; 8];
        self.read_exact(&mut value, "uint64")?;
        Ok(u64::from_be_bytes(value))
    }

    pub fn read_i64(&mut self) -> Result<i64, Error> {
        Ok(self.read_u64()? as i64)
    }

    pub fn read_f64(&mut self) -> Result<f64, Error> {
        Ok(f64::from_bits(self.read_u64()?))
    }

    pub fn read_qbytearray(&mut self) -> Result<Vec<u8>, Error> {
        let length = self.read_u32()?;
        let length = match length {
            u32::MAX => return Ok(Vec::new()),
            0xffff_fffe => self.read_u64()?,
            value => u64::from(value),
        };
        if length > MAX_BYTE_ARRAY {
            return Err(Error::InvalidData(format!(
                "QByteArray length exceeds the 2 GiB limit: {length}"
            )));
        }
        let length = usize::try_from(length)
            .map_err(|_| Error::InvalidData("QByteArray does not fit in memory".into()))?;
        let mut bytes = vec![0; length];
        self.read_exact(&mut bytes, "QByteArray payload")?;
        Ok(bytes)
    }

    pub fn read_qstring(&mut self) -> Result<String, Error> {
        let length = self.read_u32()?;
        if length == u32::MAX {
            return Ok(String::new());
        }
        let length = if length == u32::MAX - 1 {
            self.read_u64()?
        } else {
            u64::from(length)
        };
        if length > u64::from(MAX_STRING_BYTES) || length % 2 != 0 {
            return Err(Error::InvalidData(format!(
                "invalid QString byte length: {length}"
            )));
        }
        let length = usize::try_from(length)
            .map_err(|_| Error::InvalidData("QString does not fit in memory".into()))?;
        let mut bytes = vec![0; length];
        self.read_exact(&mut bytes, "QString payload")?;
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16(&units)
            .map_err(|_| Error::InvalidData("QString contains invalid UTF-16".into()))
    }

    pub fn read_qstring_list(&mut self) -> Result<Vec<String>, Error> {
        let count = self.read_count("QStringList")?;
        (0..count).map(|_| self.read_qstring()).collect()
    }

    pub fn read_qvariant_map(&mut self) -> Result<BTreeMap<String, QtValue>, Error> {
        let count = self.read_count("QVariantMap")?;
        let mut values = BTreeMap::new();
        for _ in 0..count {
            let key = self.read_qstring()?;
            let value = self.read_variant_at_depth(0)?;
            if values.insert(key.clone(), value).is_some() {
                return Err(Error::InvalidData(format!(
                    "duplicate QVariantMap key: {key}"
                )));
            }
        }
        Ok(values)
    }

    pub fn read_variant(&mut self) -> Result<QtValue, Error> {
        self.read_variant_at_depth(0)
    }

    fn read_variant_at_depth(&mut self, depth: usize) -> Result<QtValue, Error> {
        if depth >= MAX_VARIANT_DEPTH {
            return Err(Error::InvalidData(
                "QVariant nesting exceeds the 64-level limit".into(),
            ));
        }
        let type_id = self.read_u32()?;
        let is_null = self.read_bool()?;
        let value = match type_id {
            0 => {
                // Qt serializes an invalid QVariant as a null QByteArray payload.
                let _ = self.read_qbytearray()?;
                QtValue::Null
            }
            1 => QtValue::Bool(self.read_bool()?),
            2 => QtValue::I64(i64::from(self.read_i32()?)),
            3 => QtValue::U64(u64::from(self.read_u32()?)),
            4 => QtValue::I64(self.read_i64()?),
            5 => QtValue::U64(self.read_u64()?),
            6 => QtValue::F64(self.read_f64()?),
            7 => {
                let unit = self.read_u16()?;
                let decoded: String = char::decode_utf16([unit])
                    .map(|result| result.unwrap_or(char::REPLACEMENT_CHARACTER))
                    .collect();
                QtValue::String(decoded)
            }
            8 => QtValue::Map(self.read_qvariant_map_at_depth(depth + 1)?),
            9 => {
                let count = self.read_count("QVariantList")?;
                let values = (0..count)
                    .map(|_| self.read_variant_at_depth(depth + 1))
                    .collect::<Result<Vec<_>, _>>()?;
                QtValue::List(values)
            }
            10 => QtValue::String(self.read_qstring()?),
            11 => QtValue::List(
                self.read_qstring_list()?
                    .into_iter()
                    .map(QtValue::String)
                    .collect(),
            ),
            12 => QtValue::Bytes(self.read_qbytearray()?),
            _ => return Err(Error::UnsupportedVariant(type_id)),
        };
        if is_null {
            Ok(QtValue::Null)
        } else {
            Ok(value)
        }
    }

    fn read_qvariant_map_at_depth(
        &mut self,
        depth: usize,
    ) -> Result<BTreeMap<String, QtValue>, Error> {
        let count = self.read_count("QVariantMap")?;
        let mut values = BTreeMap::new();
        for _ in 0..count {
            let key = self.read_qstring()?;
            let value = self.read_variant_at_depth(depth)?;
            if values.insert(key.clone(), value).is_some() {
                return Err(Error::InvalidData(format!(
                    "duplicate QVariantMap key: {key}"
                )));
            }
        }
        Ok(values)
    }

    fn read_count(&mut self, label: &str) -> Result<u32, Error> {
        let count = self.read_u32()?;
        if count > MAX_CONTAINER_ITEMS {
            return Err(Error::InvalidData(format!(
                "{label} count exceeds the {MAX_CONTAINER_ITEMS} item limit: {count}"
            )));
        }
        Ok(count)
    }

    fn read_exact(&mut self, bytes: &mut [u8], label: &str) -> Result<(), Error> {
        self.inner.read_exact(bytes).map_err(|error| {
            Error::InvalidData(format!(
                "truncated or invalid Qt stream while reading {label} at byte {}: {error}",
                self.offset
            ))
        })?;
        self.offset += bytes.len() as u64;
        Ok(())
    }

    pub fn into_inner(self) -> R {
        self.inner
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{QtReader, QtValue, StreamVersion};

    #[test]
    fn reads_qstring_map_from_qdatastream_bytes() {
        let encoded = [
            0x00, 0x00, 0x00, 0x01, // one map entry
            0x00, 0x00, 0x00, 0x02, 0x00, 0x76, // key: v
            0x00, 0x00, 0x00, 0x0a, 0x00, // QVariant QString, not null
            0x00, 0x00, 0x00, 0x0a, // five UTF-16 code units
            0x00, 0x68, 0x00, 0x65, 0x00, 0x6c, 0x00, 0x6c, 0x00, 0x6f,
        ];
        let mut reader = QtReader::new(Cursor::new(encoded), StreamVersion::Qt4_7);
        let values = reader.read_qvariant_map().unwrap();
        assert_eq!(values.get("v"), Some(&QtValue::String("hello".into())));
    }

    #[test]
    fn reads_invalid_variant_payload() {
        let encoded = [
            0x00, 0x00, 0x00, 0x01, // one map entry
            0x00, 0x00, 0x00, 0x02, 0x00, 0x76, // key: v
            0x00, 0x00, 0x00, 0x00, 0x01, // invalid QVariant, null
            0xff, 0xff, 0xff, 0xff, // null QByteArray payload
        ];
        let mut reader = QtReader::new(Cursor::new(encoded), StreamVersion::Qt4_7);
        let values = reader.read_qvariant_map().unwrap();
        assert_eq!(values.get("v"), Some(&QtValue::Null));
    }
}
