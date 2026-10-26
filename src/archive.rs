use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Cursor, Read};
use std::path::Path;

use flate2::read::ZlibDecoder;

use crate::qt::{QtReader, QtValue, StreamVersion};
use crate::Error;

const MAX_ITEMS: i32 = 100_000;
const MAX_FORMATS: i32 = 10_000;
const MAX_DATA_BLOCK: u64 = 2 * 1024 * 1024 * 1024;

pub type ClipboardItem = BTreeMap<String, Vec<u8>>;

pub struct ArchiveReader {
    reader: QtReader<BufReader<File>>,
    version: u8,
    metadata: BTreeMap<String, QtValue>,
    legacy_tab_name: Option<String>,
    v3_tabs: Vec<Option<(String, Vec<u8>)>>,
    direct_tab_read: bool,
}

pub struct ItemReader<'a> {
    reader: ItemReaderInner<'a>,
    remaining: u32,
}

enum ItemReaderInner<'a> {
    Direct(&'a mut QtReader<BufReader<File>>),
    Buffered(QtReader<Cursor<Vec<u8>>>),
}

impl ArchiveReader {
    pub fn open(path: &Path) -> Result<Self, Error> {
        if !path.is_file() {
            return Err(Error::InvalidData(format!(
                "input file does not exist: {}",
                path.display()
            )));
        }
        let file = File::open(path)?;
        let mut reader = QtReader::new(BufReader::new(file), StreamVersion::Qt4_7);
        let header = reader.read_qbytearray()?;
        let version = match header.as_slice() {
            b"CopyQ v1" => 1,
            b"CopyQ v2" => 2,
            b"CopyQ v3" => 3,
            b"CopyQ v4" => 4,
            b"CopyQ v5" => 5,
            _ => {
                return Err(Error::InvalidData(
                    "header is not a supported CopyQ v1-v5 archive".into(),
                ));
            }
        };

        let mut metadata = BTreeMap::new();
        let mut legacy_tab_name = None;
        let mut v3_tabs = Vec::new();
        match version {
            1 => legacy_tab_name = Some("clipboard".into()),
            2 => legacy_tab_name = Some(reader.read_qstring()?),
            3 => {
                reader.set_version(StreamVersion::Qt5_15);
                metadata = reader.read_qvariant_map()?;
                let raw_tabs = match metadata.remove("tabs") {
                    Some(QtValue::List(tabs)) => tabs,
                    None => Vec::new(),
                    _ => return Err(Error::InvalidData("invalid v3 tab list".into())),
                };
                if raw_tabs.len() > 10_000 {
                    return Err(Error::InvalidData(
                        "v3 archive contains more than 10,000 tabs".into(),
                    ));
                }
                for raw_tab in raw_tabs {
                    let QtValue::Map(mut fields) = raw_tab else {
                        return Err(Error::InvalidData("invalid v3 tab record".into()));
                    };
                    let name = take_string(fields.remove("name"), "v3 tab name")?;
                    let data = take_bytes(fields.remove("data"), "v3 tab data")?;
                    v3_tabs.push(Some((name, data)));
                }
            }
            4 => metadata = reader.read_qvariant_map()?,
            5 => {
                return Err(Error::UnsupportedVersion(5));
            }
            _ => unreachable!(),
        }

        Ok(Self {
            reader,
            version,
            metadata,
            legacy_tab_name,
            v3_tabs,
            direct_tab_read: false,
        })
    }

    pub fn version(&self) -> u8 {
        self.version
    }

    pub fn metadata(&self) -> &BTreeMap<String, QtValue> {
        &self.metadata
    }

    pub fn for_each_tab<F>(&mut self, mut visit: F) -> Result<usize, Error>
    where
        F: FnMut(usize, &str, &mut ItemReader<'_>) -> Result<(), Error>,
    {
        match self.version {
            1 | 2 => {
                if self.direct_tab_read {
                    return Ok(1);
                }
                self.direct_tab_read = true;
                let name = self
                    .legacy_tab_name
                    .clone()
                    .unwrap_or_else(|| "clipboard".into());
                let mut items = ItemReader::from_direct(&mut self.reader)?;
                visit(1, &name, &mut items)?;
                items.ensure_finished()?;
                Ok(1)
            }
            3 => {
                let tab_count = self.v3_tabs.len();
                for index in 0..tab_count {
                    let (name, data) = self.v3_tabs[index]
                        .take()
                        .ok_or_else(|| Error::InvalidData("v3 tab data already consumed".into()))?;
                    let mut items = ItemReader::from_buffered(data)?;
                    visit(index + 1, &name, &mut items)?;
                    items.ensure_finished()?;
                }
                Ok(tab_count)
            }
            4 => {
                let mut tab_count = 0;
                while !self.reader.at_end()? {
                    let mut fields = self.reader.read_qvariant_map()?;
                    let name = take_string(fields.remove("name"), "v4 tab name")?;
                    let data = take_bytes(fields.remove("data"), "v4 tab data")?;
                    let mut items = ItemReader::from_buffered(data)?;
                    tab_count += 1;
                    visit(tab_count, &name, &mut items)?;
                    items.ensure_finished()?;
                }
                Ok(tab_count)
            }
            5 => Err(Error::UnsupportedVersion(5)),
            _ => unreachable!(),
        }
    }
}

impl<'a> ItemReader<'a> {
    fn from_direct(reader: &'a mut QtReader<BufReader<File>>) -> Result<Self, Error> {
        let remaining = read_item_count(reader)?;
        Ok(Self {
            reader: ItemReaderInner::Direct(reader),
            remaining,
        })
    }

    fn from_buffered(data: Vec<u8>) -> Result<Self, Error> {
        let mut reader = QtReader::new(Cursor::new(data), StreamVersion::Qt4_7);
        let remaining = read_item_count(&mut reader)?;
        Ok(Self {
            reader: ItemReaderInner::Buffered(reader),
            remaining,
        })
    }

    pub fn remaining(&self) -> u32 {
        self.remaining
    }

    pub fn next_item(&mut self) -> Result<Option<ClipboardItem>, Error> {
        if self.remaining == 0 {
            return Ok(None);
        }
        let item = match &mut self.reader {
            ItemReaderInner::Direct(reader) => read_item_data(reader)?,
            ItemReaderInner::Buffered(reader) => read_item_data(reader)?,
        };
        self.remaining -= 1;
        Ok(Some(item))
    }

    fn ensure_finished(&mut self) -> Result<(), Error> {
        if self.remaining != 0 {
            return Err(Error::InvalidData(format!(
                "tab reader left {} items unread",
                self.remaining
            )));
        }
        let has_trailing_data = match &mut self.reader {
            ItemReaderInner::Direct(reader) => !reader.at_end()?,
            ItemReaderInner::Buffered(reader) => !reader.at_end()?,
        };
        if has_trailing_data {
            return Err(Error::InvalidData(
                "unexpected trailing bytes after tab items".into(),
            ));
        }
        Ok(())
    }
}

fn read_item_count<R: Read>(reader: &mut QtReader<R>) -> Result<u32, Error> {
    let count = reader.read_i32()?;
    if count < 0 || count > MAX_ITEMS {
        return Err(Error::InvalidData(format!(
            "invalid item count in tab: {count}"
        )));
    }
    Ok(count as u32)
}

fn read_item_data<R: Read>(reader: &mut QtReader<R>) -> Result<ClipboardItem, Error> {
    let mut count = reader.read_i32()?;
    if count == -2 {
        count = reader.read_i32()?;
        if count < 0 || count > MAX_FORMATS {
            return Err(Error::InvalidData(format!(
                "invalid format count in item: {count}"
            )));
        }
        let mut item = ClipboardItem::new();
        for _ in 0..count {
            let mut mime = decode_mime(&reader.read_qstring()?)?;
            let compressed = reader.read_bool()?;
            let mut payload = read_limited_byte_array(reader, "item data")?;
            if compressed {
                payload = decompress_qt(&payload)?;
            }
            if let Some(file_mime) = mime.strip_prefix("FILE:") {
                mime = format!("application/x-copyq-file-reference; mime={file_mime}");
            }
            item.insert(mime, payload);
        }
        return Ok(item);
    }
    if count == -3 {
        return Err(Error::InvalidData(
            "unexpected encrypted data marker inside tab items".into(),
        ));
    }
    if count < 0 || count > MAX_FORMATS {
        return Err(Error::InvalidData(format!(
            "invalid legacy format count in item: {count}"
        )));
    }

    let mut item = ClipboardItem::new();
    for _ in 0..count {
        let mime = reader.read_qstring()?;
        let mut payload = read_limited_byte_array(reader, "legacy item data")?;
        if !payload.is_empty() {
            payload = decompress_qt(&payload)?;
        }
        item.insert(mime, payload);
    }
    Ok(item)
}

fn read_limited_byte_array<R: Read>(
    reader: &mut QtReader<R>,
    label: &str,
) -> Result<Vec<u8>, Error> {
    let bytes = reader.read_qbytearray()?;
    if bytes.len() as u64 > MAX_DATA_BLOCK {
        return Err(Error::InvalidData(format!(
            "{label} exceeds the 2 GiB limit"
        )));
    }
    Ok(bytes)
}

fn decompress_qt(payload: &[u8]) -> Result<Vec<u8>, Error> {
    if payload.len() < 4 {
        return Err(Error::InvalidData(
            "compressed item data is incomplete".into(),
        ));
    }
    let expected = u32::from_be_bytes(payload[..4].try_into().unwrap()) as u64;
    if expected > MAX_DATA_BLOCK {
        return Err(Error::InvalidData(format!(
            "decompressed item exceeds the 2 GiB limit: {expected}"
        )));
    }
    let mut decoder = ZlibDecoder::new(&payload[4..]);
    let mut output = Vec::with_capacity(expected as usize);
    decoder
        .by_ref()
        .take(expected + 1)
        .read_to_end(&mut output)
        .map_err(|error| Error::InvalidData(format!("failed to decompress item: {error}")))?;
    if output.len() as u64 != expected {
        return Err(Error::InvalidData(format!(
            "decompressed item length mismatch: expected {expected}, got {}",
            output.len()
        )));
    }
    Ok(output)
}

fn decode_mime(encoded: &str) -> Result<String, Error> {
    let Some(first) = encoded.chars().next() else {
        return Err(Error::InvalidData("empty encoded MIME type".into()));
    };
    let Some(prefix_id) = first.to_digit(16) else {
        return Err(Error::InvalidData("invalid encoded MIME prefix".into()));
    };
    let suffix = &encoded[first.len_utf8()..];
    let prefix = match prefix_id {
        0 => return Ok(suffix.to_owned()),
        1 => "application/x-copyq-window-title",
        2 => "application/x-copyq-item-notes",
        3 => "application/x-copyq-",
        4 => "text/plain",
        5 => "text/html",
        6 => "text/uri-list",
        7 => "image/",
        8 => "text/",
        9 => "application/",
        10 => "audio/",
        11 => "video/",
        _ => {
            return Err(Error::InvalidData(format!(
                "unknown compressed MIME prefix: {prefix_id}"
            )));
        }
    };
    Ok(format!("{prefix}{suffix}"))
}

fn take_string(value: Option<QtValue>, label: &str) -> Result<String, Error> {
    match value {
        Some(QtValue::String(value)) => Ok(value),
        _ => Err(Error::InvalidData(format!("missing or invalid {label}"))),
    }
}

fn take_bytes(value: Option<QtValue>, label: &str) -> Result<Vec<u8>, Error> {
    match value {
        Some(QtValue::Bytes(value)) => Ok(value),
        _ => Err(Error::InvalidData(format!("missing or invalid {label}"))),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{decode_mime, decompress_qt, ItemReader};

    #[test]
    fn reads_v2_item_with_two_clipboard_formats() {
        let mut encoded = Vec::new();
        push_i32(&mut encoded, 1); // one row
        push_i32(&mut encoded, -2); // v2 item data
        push_i32(&mut encoded, 2); // two MIME types
        push_qstring(&mut encoded, "4"); // compressed text/plain MIME
        encoded.push(0); // uncompressed
        push_qbytearray(&mut encoded, b"hello");
        push_qstring(&mut encoded, "5"); // compressed text/html MIME
        encoded.push(0);
        push_qbytearray(&mut encoded, b"<b>hello</b>");

        let mut items = ItemReader::from_buffered(encoded).unwrap();
        let item = items.next_item().unwrap().unwrap();
        assert_eq!(item.get("text/plain").unwrap(), b"hello");
        assert_eq!(item.get("text/html").unwrap(), b"<b>hello</b>");
        assert!(items.next_item().unwrap().is_none());
        items.ensure_finished().unwrap();
    }

    #[test]
    fn reads_v1_compressed_item() {
        let source = b"legacy text";
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        use std::io::Write;
        encoder.write_all(source).unwrap();
        let compressed = encoder.finish().unwrap();
        let mut qcompressed = (source.len() as u32).to_be_bytes().to_vec();
        qcompressed.extend(compressed);

        let mut encoded = Vec::new();
        push_i32(&mut encoded, 1); // one row
        push_i32(&mut encoded, 1); // one legacy format
        push_qstring(&mut encoded, "text/plain");
        push_qbytearray(&mut encoded, &qcompressed);

        let mut items = ItemReader::from_buffered(encoded).unwrap();
        let item = items.next_item().unwrap().unwrap();
        assert_eq!(item.get("text/plain").unwrap(), source);
        items.ensure_finished().unwrap();
    }

    #[test]
    fn decodes_copyq_mime_prefixes_and_file_references() {
        assert_eq!(decode_mime("4").unwrap(), "text/plain");
        assert_eq!(decode_mime("7png").unwrap(), "image/png");
        assert_eq!(
            decode_mime("0application/x-test").unwrap(),
            "application/x-test"
        );
        assert_eq!(
            decompress_qt(&[]).unwrap_err().to_string(),
            "invalid CopyQ data: compressed item data is incomplete"
        );
    }

    fn push_i32(target: &mut Vec<u8>, value: i32) {
        target.extend(value.to_be_bytes());
    }

    fn push_qstring(target: &mut Vec<u8>, value: &str) {
        let utf16: Vec<u16> = value.encode_utf16().collect();
        target.extend(((utf16.len() * 2) as u32).to_be_bytes());
        for unit in utf16 {
            target.extend(unit.to_be_bytes());
        }
    }

    fn push_qbytearray(target: &mut Vec<u8>, value: &[u8]) {
        target.extend((value.len() as u32).to_be_bytes());
        target.extend(value);
    }

    #[test]
    fn decompression_checks_declared_size() {
        let mut encoded = Vec::new();
        encoded.extend(2_u32.to_be_bytes());
        encoded.extend([0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01]);
        assert!(decompress_qt(&encoded).is_err());

        let mut reader = Cursor::new(encoded);
        let mut bytes = Vec::new();
        use std::io::Read;
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes.len(), 12);
    }
}
