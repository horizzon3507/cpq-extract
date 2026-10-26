use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use tempfile::Builder;
use unicode_normalization::UnicodeNormalization;

use crate::archive::ArchiveReader;
use crate::qt::QtValue;
use crate::Error;

#[derive(Debug, Eq, PartialEq)]
pub struct ExtractionSummary {
    pub archive_version: u8,
    pub tab_count: usize,
    pub item_count: u64,
    pub output: PathBuf,
}

#[derive(Serialize)]
struct ManifestFile<'a> {
    mime: &'a str,
    file: String,
    bytes: usize,
}

#[derive(Serialize)]
struct ManifestItem<'a> {
    index: u32,
    files: Vec<ManifestFile<'a>>,
}

pub fn extract(
    input: &Path,
    output: &Path,
    password: Option<&str>,
) -> Result<ExtractionSummary, Error> {
    let output = absolute_path(output)?;
    match fs::symlink_metadata(&output) {
        Ok(_) => return Err(Error::OutputExists(output)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let mut archive = ArchiveReader::open_with_password(input, password)?;
    let parent = output
        .parent()
        .ok_or_else(|| Error::InvalidData("output directory has no parent".into()))?;
    fs::create_dir_all(parent)?;
    let name = output
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| Error::InvalidData("output directory has an invalid name".into()))?;
    let staging = Builder::new()
        .prefix(&format!(".{name}.tmp-"))
        .tempdir_in(parent)?;
    let staging_path = staging.path().to_owned();
    let mut manifest = BufWriter::new(create_new(&staging_path.join("manifest.json"))?);
    write!(
        manifest,
        "{{\n  \"format\": \"CopyQ CPQ\",\n  \"archive_version\": {},\n  \"tabs\": [",
        archive.version()
    )?;

    let mut total_items = 0_u64;
    let tab_count = archive.for_each_tab(|tab_index, tab_name, items| {
        let directory = format!("{tab_index:03}_{}", safe_component(tab_name));
        let tab_path = staging_path.join(&directory);
        fs::create_dir(&tab_path)?;
        if tab_index > 1 {
            write!(manifest, ",")?;
        }
        write!(manifest, "\n    {{\"index\": {tab_index}, \"name\":")?;
        serde_json::to_writer(&mut manifest, tab_name)?;
        write!(manifest, ", \"directory\":")?;
        serde_json::to_writer(&mut manifest, &directory)?;
        write!(manifest, ", \"items\": [")?;

        let mut item_count = 0_u32;
        while let Some(formats) = items.next_item()? {
            item_count += 1;
            total_items += 1;
            let mut used_filenames = HashSet::new();
            let mut files = Vec::with_capacity(formats.len());
            for (mime, payload) in &formats {
                let extension = file_extension(mime);
                let filename = unique_filename(item_count, &extension, &mut used_filenames);
                let mut output_file = create_new(&tab_path.join(&filename))?;
                output_file.write_all(payload)?;
                files.push(ManifestFile {
                    mime,
                    file: format!("{directory}/{filename}"),
                    bytes: payload.len(),
                });
            }
            if item_count > 1 {
                write!(manifest, ",")?;
            }
            write!(manifest, "\n      ")?;
            serde_json::to_writer(
                &mut manifest,
                &ManifestItem {
                    index: item_count,
                    files,
                },
            )?;
        }

        write!(manifest, "], \"item_count\": {item_count}}}")?;
        Ok(())
    })?;

    write!(manifest, "\n  ],\n")?;
    let settings = archive.metadata().get("settings");
    let commands = archive.metadata().get("commands");
    let has_settings = settings.is_some_and(QtValue::is_truthy);
    let command_count = match commands {
        Some(QtValue::List(values)) => values.len(),
        _ => 0,
    };
    if let Some(value) = settings.filter(|value| value.is_truthy()) {
        write_json(&staging_path.join("settings.json"), &value.to_json())?;
    }
    if let Some(value) = commands.filter(|value| value.is_truthy()) {
        write_json(&staging_path.join("commands.json"), &value.to_json())?;
    }
    write!(
        manifest,
        "  \"total_items\": {total_items},\n  \"has_settings\": {has_settings},\n  \"command_count\": {command_count}\n}}\n"
    )?;
    manifest.flush()?;
    drop(manifest);

    let staging_path = staging.keep();
    if let Err(error) = fs::rename(&staging_path, &output) {
        let _ = fs::remove_dir_all(&staging_path);
        return Err(error.into());
    }

    Ok(ExtractionSummary {
        archive_version: archive.version(),
        tab_count,
        item_count: total_items,
        output,
    })
}

fn absolute_path(path: &Path) -> Result<PathBuf, Error> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn create_new(path: &Path) -> Result<File, Error> {
    Ok(OpenOptions::new().write(true).create_new(true).open(path)?)
}

fn write_json(path: &Path, value: &serde_json::Value) -> Result<(), Error> {
    let mut output = BufWriter::new(create_new(path)?);
    serde_json::to_writer_pretty(&mut output, value)?;
    writeln!(output)?;
    output.flush()?;
    Ok(())
}

fn safe_component(value: &str) -> String {
    let normalized: String = value.nfkc().collect();
    let normalized = normalized.trim();
    let component: String = normalized
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    let component: String = component
        .trim_matches(|character| matches!(character, '.' | '_' | '-'))
        .chars()
        .take(72)
        .collect();
    if component.is_empty() {
        "tab".into()
    } else {
        component
    }
}

fn file_extension(mime: &str) -> String {
    let mime_lower = mime.to_ascii_lowercase();
    let mime_without_parameters = mime.split(';').next().unwrap_or_default().trim();
    let normalized_mime = mime_without_parameters.to_ascii_lowercase();
    let special = match normalized_mime.as_str() {
        "application/x-copyq-item-notes" => Some("notes.txt"),
        "application/x-copyq-window-title" => Some("window-title.txt"),
        "text/plain" => Some("txt"),
        "text/markdown" => Some("md"),
        "text/html" => Some("html"),
        "text/uri-list" => Some("uri-list.txt"),
        "image/jpeg" => Some("jpg"),
        "image/svg+xml" => Some("svg"),
        "image/png" => Some("png"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "image/bmp" => Some("bmp"),
        "application/pdf" => Some("pdf"),
        "application/json" => Some("json"),
        _ => None,
    };
    if let Some(extension) = special {
        return extension.into();
    }

    if let Some(extension) = mime_guess::get_mime_extensions_str(&normalized_mime)
        .and_then(|extensions| extensions.first())
    {
        let extension = extension.to_ascii_lowercase();
        if extension
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        {
            return extension;
        }
    }

    let slug = safe_component(&mime_lower.replace(['/', '+'], "-")).to_ascii_lowercase();
    format!(
        "format-{}.bin",
        if slug.is_empty() { "unknown" } else { &slug }
    )
}

fn unique_filename(item_number: u32, extension: &str, used: &mut HashSet<String>) -> String {
    let base = format!("{item_number:06}");
    let mut filename = format!("{base}.{extension}");
    let mut suffix = 2_u32;
    while !used.insert(filename.clone()) {
        filename = format!("{base}_{suffix:02}.{extension}");
        suffix += 1;
    }
    filename
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::Value;

    use super::{extract, file_extension, safe_component};

    #[test]
    fn extracts_a_small_qt_v4_backup() {
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("sample.cpq");
        let output_path = temp.path().join("out");
        fs::write(&archive_path, make_v4_archive()).unwrap();

        let summary = extract(&archive_path, &output_path, None).unwrap();
        assert_eq!(summary.archive_version, 4);
        assert_eq!(summary.tab_count, 1);
        assert_eq!(summary.item_count, 1);
        assert_eq!(
            fs::read(output_path.join("001_clipboard/000001.txt")).unwrap(),
            b"hello"
        );
        let manifest: Value =
            serde_json::from_slice(&fs::read(output_path.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["total_items"], 1);
        assert_eq!(manifest["tabs"][0]["name"], "clipboard");
        assert_eq!(
            manifest["tabs"][0]["items"][0]["files"][0]["mime"],
            "text/plain"
        );
    }

    #[test]
    fn extracts_legacy_v1_and_v2_archives() {
        let temp = tempfile::tempdir().unwrap();
        for version in [1, 2] {
            let archive_path = temp.path().join(format!("v{version}.cpq"));
            let output_path = temp.path().join(format!("out-v{version}"));
            fs::write(&archive_path, make_legacy_archive(version)).unwrap();

            let summary = extract(&archive_path, &output_path, None).unwrap();
            assert_eq!(summary.archive_version, version);
            assert_eq!(summary.tab_count, 1);
            assert_eq!(summary.item_count, 1);
            let tab = if version == 1 { "clipboard" } else { "Legacy" };
            assert_eq!(
                fs::read(output_path.join(format!("001_{tab}/000001.txt"))).unwrap(),
                b"legacy item"
            );
        }
    }

    #[test]
    fn extracts_a_qt_v3_archive_with_embedded_tabs() {
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("v3.cpq");
        let output_path = temp.path().join("out-v3");
        fs::write(&archive_path, make_v3_archive()).unwrap();

        let summary = extract(&archive_path, &output_path, None).unwrap();
        assert_eq!(summary.archive_version, 3);
        assert_eq!(summary.tab_count, 1);
        assert_eq!(summary.item_count, 1);
        assert_eq!(
            fs::read(output_path.join("001_ArchiveV3/000001.txt")).unwrap(),
            b"v3 item"
        );
    }

    #[test]
    fn refuses_an_existing_output_directory() {
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("sample.cpq");
        let output_path = temp.path().join("out");
        fs::write(&archive_path, make_v4_archive()).unwrap();
        fs::create_dir(&output_path).unwrap();
        assert!(extract(&archive_path, &output_path, None).is_err());
    }

    #[test]
    fn extracts_an_encrypted_qt_v5_backup() {
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("sample.cpq");
        let output_path = temp.path().join("out");
        fs::write(
            &archive_path,
            include_bytes!("../tests/fixtures/v5-small.cpq"),
        )
        .unwrap();

        let summary = extract(&archive_path, &output_path, Some("fixture pass")).unwrap();
        assert_eq!(summary.archive_version, 5);
        assert_eq!(summary.tab_count, 1);
        assert_eq!(summary.item_count, 1);
        assert_eq!(
            fs::read(output_path.join("001_Fixture/000001.txt")).unwrap(),
            b"CopyQ v5 fixture text"
        );
        let settings: Value =
            serde_json::from_slice(&fs::read(output_path.join("settings.json")).unwrap()).unwrap();
        assert_eq!(settings["Fixture/flag"], "v5 settings");
        let manifest: Value =
            serde_json::from_slice(&fs::read(output_path.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["has_settings"], true);
    }

    #[test]
    fn sanitizes_tab_directories_and_maps_mime_extensions() {
        assert_eq!(safe_component(" ../A/B "), "A_B");
        assert_eq!(file_extension("image/png"), "png");
        assert_eq!(file_extension("text/markdown"), "md");
        assert_eq!(file_extension("text/plain;charset=utf-8"), "txt");
        assert_eq!(
            file_extension("application/x-unknown"),
            "format-application-x-unknown.bin"
        );
    }

    fn make_v4_archive() -> Vec<u8> {
        let mut archive = Vec::new();
        push_qbytearray(&mut archive, b"CopyQ v4");

        push_u32(&mut archive, 1); // one root metadata entry
        push_qstring(&mut archive, "tabs");
        push_variant_header(&mut archive, 11); // QStringList
        push_u32(&mut archive, 1);
        push_qstring(&mut archive, "clipboard");

        let mut model = Vec::new();
        push_i32(&mut model, 1); // one row
        push_i32(&mut model, -2); // v2 item serialization
        push_i32(&mut model, 1); // one format
        push_qstring(&mut model, "4");
        model.push(0);
        push_qbytearray(&mut model, b"hello");

        push_u32(&mut archive, 2); // tab map: data and name
        push_qstring(&mut archive, "data");
        push_variant_header(&mut archive, 12); // QByteArray
        push_qbytearray(&mut archive, &model);
        push_qstring(&mut archive, "name");
        push_variant_header(&mut archive, 10); // QString
        push_qstring(&mut archive, "clipboard");
        archive
    }

    fn make_legacy_archive(version: u8) -> Vec<u8> {
        let mut archive = Vec::new();
        push_qbytearray(&mut archive, format!("CopyQ v{version}").as_bytes());
        if version == 2 {
            push_qstring(&mut archive, "Legacy");
        }
        push_i32(&mut archive, 1); // one row
        if version == 1 {
            let mut encoder =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            use std::io::Write;
            encoder.write_all(b"legacy item").unwrap();
            let compressed = encoder.finish().unwrap();
            let mut qcompressed = 11_u32.to_be_bytes().to_vec();
            qcompressed.extend(compressed);
            push_i32(&mut archive, 1);
            push_qstring(&mut archive, "text/plain");
            push_qbytearray(&mut archive, &qcompressed);
        } else {
            push_i32(&mut archive, -2);
            push_i32(&mut archive, 1);
            push_qstring(&mut archive, "4");
            archive.push(0);
            push_qbytearray(&mut archive, b"legacy item");
        }
        archive
    }

    fn make_v3_archive() -> Vec<u8> {
        let mut archive = Vec::new();
        push_qbytearray(&mut archive, b"CopyQ v3");

        let mut model = Vec::new();
        push_i32(&mut model, 1);
        push_i32(&mut model, -2);
        push_i32(&mut model, 1);
        push_qstring(&mut model, "4");
        model.push(0);
        push_qbytearray(&mut model, b"v3 item");

        push_u32(&mut archive, 1); // root metadata: tabs
        push_qstring(&mut archive, "tabs");
        push_variant_header(&mut archive, 9); // QVariantList
        push_u32(&mut archive, 1);
        push_variant_header(&mut archive, 8); // QVariantMap
        push_u32(&mut archive, 2); // data and name
        push_qstring(&mut archive, "data");
        push_variant_header(&mut archive, 12); // QByteArray
        push_qbytearray(&mut archive, &model);
        push_qstring(&mut archive, "name");
        push_variant_header(&mut archive, 10); // QString
        push_qstring(&mut archive, "ArchiveV3");
        archive
    }

    fn push_variant_header(target: &mut Vec<u8>, type_id: u32) {
        push_u32(target, type_id);
        target.push(0); // QVariant is not null
    }

    fn push_u32(target: &mut Vec<u8>, value: u32) {
        target.extend(value.to_be_bytes());
    }

    fn push_i32(target: &mut Vec<u8>, value: i32) {
        target.extend(value.to_be_bytes());
    }

    fn push_qstring(target: &mut Vec<u8>, value: &str) {
        let utf16: Vec<u16> = value.encode_utf16().collect();
        push_u32(target, (utf16.len() * 2) as u32);
        for unit in utf16 {
            target.extend(unit.to_be_bytes());
        }
    }

    fn push_qbytearray(target: &mut Vec<u8>, value: &[u8]) {
        push_u32(target, value.len() as u32);
        target.extend(value);
    }
}
