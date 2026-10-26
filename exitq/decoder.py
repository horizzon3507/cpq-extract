from __future__ import annotations

import argparse
import base64
import getpass
import hashlib
import hmac
import json
import mimetypes
import os
import re
import shutil
import struct
import sys
import tempfile
import unicodedata
import zlib
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterator

from cryptography.hazmat.primitives import padding
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
from PyQt5.QtCore import QByteArray, QBuffer, QDataStream, QFile, QIODevice


MAX_ITEMS = 100_000
MAX_DATA_BLOCK = 2 * 1024 * 1024 * 1024
MAX_STRING_BYTES = 1024 * 1024
ENCRYPTION_MAGIC = b"CPQ1"
MIME_PREFIXES = {
    1: "application/x-copyq-window-title",
    2: "application/x-copyq-item-notes",
    3: "application/x-copyq-",
    4: "text/plain",
    5: "text/html",
    6: "text/uri-list",
    7: "image/",
    8: "text/",
    9: "application/",
    10: "audio/",
    11: "video/",
}
SPECIAL_EXTENSIONS = {
    "application/x-copyq-item-notes": "notes.txt",
    "application/x-copyq-window-title": "window-title.txt",
    "text/plain": "txt",
    "text/markdown": "md",
    "text/html": "html",
    "text/uri-list": "uri-list.txt",
    "image/jpeg": "jpg",
    "image/svg+xml": "svg",
    "image/png": "png",
    "image/gif": "gif",
    "image/webp": "webp",
    "image/bmp": "bmp",
    "application/pdf": "pdf",
    "application/json": "json",
}


class DecodeError(Exception):
    """Raised when a CopyQ export cannot be decoded safely."""


@dataclass
class Tab:
    name: str
    items: Iterator[dict[str, bytes]]


class QtReader:
    def __init__(self, data: bytes | QByteArray | QFile, version: int = QDataStream.Qt_4_7):
        if isinstance(data, QFile):
            self.device = data
            self.buffer = None
        else:
            self.buffer = QByteArray(data)
            self.device = QBuffer(self.buffer)
            self.device.open(QIODevice.ReadOnly)
        self.stream = QDataStream(self.device)
        self.stream.setVersion(version)

    def set_version(self, version: int) -> None:
        self.stream.setVersion(version)

    def _check(self, label: str) -> None:
        if self.stream.status() != QDataStream.Ok:
            raise DecodeError(f"arquivo truncado ou inválido ao ler {label}")

    def read_i32(self, label: str) -> int:
        value = self.stream.readInt32()
        self._check(label)
        return value

    def read_bool(self, label: str) -> bool:
        value = self.stream.readBool()
        self._check(label)
        return value

    def read_string(self, label: str) -> str:
        length = self.stream.readUInt32()
        self._check(label)
        if length == 0xFFFFFFFF:
            return ""
        if length > MAX_STRING_BYTES or length % 2:
            raise DecodeError(f"tamanho inválido de texto ao ler {label}")
        payload = bytes(self.device.read(length))
        if len(payload) != length:
            raise DecodeError(f"arquivo truncado ao ler {label}")
        try:
            return payload.decode("utf-16-be")
        except UnicodeDecodeError as error:
            raise DecodeError(f"texto inválido ao ler {label}") from error

    def read_byte_array(self, label: str) -> bytes:
        length = self.stream.readUInt32()
        self._check(label)
        if length == 0xFFFFFFFF:
            return b""
        if length == 0xFFFFFFFE:
            raise DecodeError(f"formato de bloco grande demais para esta versão ao ler {label}")
        if length > MAX_DATA_BLOCK:
            raise DecodeError(f"bloco muito grande ao ler {label}")
        result = bytes(self.device.read(length))
        if len(result) != length:
            raise DecodeError(f"arquivo truncado ao ler {label}")
        return result

    def read_variant_map(self, label: str) -> dict[str, Any]:
        value = self.stream.readQVariantMap()
        self._check(label)
        if not isinstance(value, dict):
            raise DecodeError(f"mapa inválido ao ler {label}")
        return value

    def read_variant(self, label: str) -> Any:
        value = self.stream.readQVariant()
        self._check(label)
        return value

    def at_end(self) -> bool:
        return self.stream.atEnd()


def _as_bytes(value: Any, label: str) -> bytes:
    if isinstance(value, QByteArray):
        result = bytes(value)
    elif isinstance(value, bytes):
        result = value
    elif isinstance(value, bytearray):
        result = bytes(value)
    else:
        raise DecodeError(f"campo {label} não contém dados binários")
    if len(result) > MAX_DATA_BLOCK:
        raise DecodeError(f"campo {label} é grande demais")
    return result


def _as_text(value: Any, label: str) -> str:
    if isinstance(value, str):
        return value
    if isinstance(value, QByteArray):
        try:
            return bytes(value).decode("utf-8")
        except UnicodeDecodeError as error:
            raise DecodeError(f"campo {label} não é texto UTF-8 válido") from error
    raise DecodeError(f"campo {label} não contém texto")


def _decompress_qt(payload: bytes) -> bytes:
    if len(payload) < 4:
        raise DecodeError("bloco compactado incompleto")
    expected = struct.unpack(">I", payload[:4])[0]
    try:
        data = zlib.decompress(payload[4:])
    except zlib.error as error:
        raise DecodeError("não foi possível descompactar um item") from error
    if len(data) != expected:
        raise DecodeError("tamanho incorreto após descompactar um item")
    return data


def _decode_mime(encoded: str) -> str:
    if not encoded:
        raise DecodeError("tipo MIME vazio no arquivo")
    try:
        prefix_id = int(encoded[0], 16)
    except ValueError as error:
        raise DecodeError("tipo MIME compactado inválido") from error
    if prefix_id == 0:
        return encoded[1:]
    prefix = MIME_PREFIXES.get(prefix_id)
    if prefix is None:
        raise DecodeError(f"prefixo MIME desconhecido: {prefix_id}")
    return prefix + encoded[1:]


def _read_item_data(reader: QtReader) -> dict[str, bytes]:
    count = reader.read_i32("quantidade de formatos do item")
    if count == -2:
        count = reader.read_i32("quantidade de formatos do item")
        if count < 0 or count > 10_000:
            raise DecodeError("quantidade inválida de formatos no item")
        result: dict[str, bytes] = {}
        for _ in range(count):
            mime = _decode_mime(reader.read_string("tipo MIME"))
            compressed = reader.read_bool("indicador de compactação")
            payload = reader.read_byte_array("conteúdo do item")
            if compressed:
                payload = _decompress_qt(payload)
            if mime.startswith("FILE:"):
                mime = mime[5:]
                # A path embedded in an archive is data, never a path to open.
                result[f"application/x-copyq-file-reference; mime={mime}"] = payload
            else:
                result[mime] = payload
        return result

    if count == -3:
        raise DecodeError("bloco criptografado inesperado dentro de uma aba")
    if count < 0 or count > 10_000:
        raise DecodeError("quantidade inválida de formatos no item")

    # CopyQ v1 stored an uncompressed format count and qCompress() data.
    result = {}
    for _ in range(count):
        mime = reader.read_string("tipo MIME legado")
        payload = reader.read_byte_array("conteúdo legado")
        if payload:
            payload = _decompress_qt(payload)
        result[mime] = payload
    return result


def _iter_model(reader: QtReader) -> Iterator[dict[str, bytes]]:
    count = reader.read_i32("quantidade de itens da aba")
    if count < 0 or count > MAX_ITEMS:
        raise DecodeError(f"quantidade inválida de itens na aba: {count}")
    for _ in range(count):
        yield _read_item_data(reader)


def _expand_hkdf(ikm: bytes, salt: bytes, info: bytes, length: int) -> bytes:
    prk = hmac.new(salt, ikm, hashlib.sha256).digest()
    return _hkdf_expand(prk, info, length)


def _hkdf_expand(prk: bytes, info: bytes, length: int) -> bytes:
    output = bytearray()
    previous = b""
    counter = 1
    while len(output) < length:
        previous = hmac.new(prk, previous + info + bytes([counter]), hashlib.sha256).digest()
        output.extend(previous)
        counter += 1
    return bytes(output[:length])


def _decrypt_cpq_envelope(payload: bytes, key: bytes) -> bytes:
    min_size = 4 + 16 + 16 + 16 + 32
    if len(payload) < min_size or not payload.startswith(ENCRYPTION_MAGIC):
        raise DecodeError("bloco criptografado não tem um envelope CopyQ reconhecido")
    signed = payload[:-32]
    salt = payload[4:20]
    message_key = _expand_hkdf(key, salt, b"copyq-msg-key-v1", 32)
    hmac_prk = hmac.new(bytes(32), key, hashlib.sha256).digest()
    hmac_key = _hkdf_expand(hmac_prk, b"copyq-hmac-v1", 32)
    expected_tag = hmac.new(hmac_key, signed, hashlib.sha256).digest()
    if not hmac.compare_digest(expected_tag, payload[-32:]):
        raise DecodeError("senha incorreta ou bloco criptografado corrompido")

    encrypted = signed[20:]
    iv, ciphertext = encrypted[:16], encrypted[16:]
    if not ciphertext or len(ciphertext) % 16:
        raise DecodeError("conteúdo AES inválido")
    decryptor = Cipher(algorithms.AES(message_key), modes.CBC(iv)).decryptor()
    padded = decryptor.update(ciphertext) + decryptor.finalize()
    unpadder = padding.PKCS7(128).unpadder()
    try:
        return unpadder.update(padded) + unpadder.finalize()
    except ValueError as error:
        raise DecodeError("senha incorreta ou bloco criptografado corrompido") from error


def _read_serialized_data_map(reader: QtReader, key: bytes | None = None) -> dict[str, Any]:
    marker = reader.read_i32("marcador do bloco de dados")
    if marker == -3:
        if key is None:
            raise DecodeError("este arquivo está criptografado; informe a senha do backup")
        encrypted_size = reader.read_i32("tamanho do bloco criptografado")
        if encrypted_size < 0 or encrypted_size > MAX_DATA_BLOCK:
            raise DecodeError("tamanho inválido do bloco criptografado")
        encrypted = reader.read_byte_array("bloco criptografado")
        if encrypted_size != len(encrypted):
            raise DecodeError("tamanho inconsistente no bloco criptografado")
        clear = _decrypt_cpq_envelope(encrypted, key)
        inner = QtReader(clear, QDataStream.Qt_5_15)
        inner_marker = inner.read_i32("marcador dos dados descriptografados")
        if inner_marker != -2:
            raise DecodeError("formato desconhecido nos dados descriptografados")
        return _read_serialized_items(inner)
    if marker == -2:
        return _read_serialized_items(reader)
    raise DecodeError(f"versão desconhecida de dados serializados: {marker}")


def _read_serialized_items(reader: QtReader) -> dict[str, Any]:
    count = reader.read_i32("quantidade de formatos serializados")
    if count < 0 or count > 10_000:
        raise DecodeError("quantidade inválida de formatos serializados")
    result: dict[str, Any] = {}
    for _ in range(count):
        encoded_mime = reader.read_string("tipo MIME serializado")
        mime = _decode_mime(encoded_mime)
        compressed = reader.read_bool("indicador de compactação")
        payload = reader.read_byte_array("valor serializado")
        if compressed:
            payload = _decompress_qt(payload)
        if mime.startswith("FILE:"):
            mime = f"application/x-copyq-file-reference; mime={mime[5:]}"
        result[mime] = payload
    return result


def _derive_v5_key(password: str) -> bytes:
    return hashlib.pbkdf2_hmac(
        "sha1", password.encode("utf-8"), b"copyq_salt_v1", 100_000, dklen=32
    )


def _read_v5_map(reader: QtReader, key: bytes | None) -> dict[str, Any]:
    raw = _read_serialized_data_map(reader, key)
    result: dict[str, Any] = {}
    for name, serialized in raw.items():
        value_reader = QtReader(_as_bytes(serialized, name), QDataStream.Qt_5_15)
        result[name] = value_reader.read_variant(f"metadado {name}")
    return result


def _read_v5_tab_map(reader: QtReader, key: bytes | None) -> dict[str, Any]:
    raw = _read_serialized_data_map(reader, key)
    return raw


class ArchiveReader:
    def __init__(self, path: Path, password: str | None = None):
        if not path.is_file():
            raise DecodeError(f"arquivo de entrada não encontrado: {path}")
        self.file = QFile(os.fspath(path))
        if not self.file.open(QIODevice.ReadOnly):
            raise DecodeError(f"não foi possível abrir o arquivo: {path}")
        self.reader = QtReader(self.file)
        self.key: bytes | None = None
        try:
            header = self.reader.read_byte_array("cabeçalho")
            match = re.fullmatch(rb"CopyQ v([1-5])", header)
            if match is None:
                raise DecodeError("cabeçalho não reconhecido; esperado um backup CopyQ v1–v5")
            self.version = int(match.group(1))
            self.metadata: dict[str, Any] = {}
            self.legacy_name: str | None = None
            self.tab_names: list[str] = []

            if self.version == 5:
                self.reader.set_version(QDataStream.Qt_5_15)
                if password is None:
                    password = getpass.getpass("Senha do backup CopyQ: ")
                self.key = _derive_v5_key(password)
                self.metadata = _read_v5_map(self.reader, self.key)
                tab_names = self.metadata.get("tabs", [])
                if not isinstance(tab_names, (list, tuple)) or len(tab_names) > 10_000:
                    raise DecodeError("lista de abas inválida no backup v5")
                self.tab_names = [_as_text(name, "nome da aba") for name in tab_names]
            elif self.version in (1, 2):
                self.legacy_name = self.reader.read_string("nome da aba")
            elif self.version == 3:
                self.metadata = self.reader.read_variant_map("metadados do backup")
                raw_tabs = self.metadata.get("tabs", [])
                if not isinstance(raw_tabs, (list, tuple)):
                    raise DecodeError("lista de abas inválida no backup v3")
            else:
                self.metadata = self.reader.read_variant_map("metadados do backup")
        except Exception:
            self.file.close()
            raise

    def close(self) -> None:
        self.file.close()

    def __enter__(self) -> ArchiveReader:
        return self

    def __exit__(self, exc_type: Any, exc_value: Any, traceback: Any) -> None:
        self.close()

    def iter_tabs(self) -> Iterator[Tab]:
        if self.version in (1, 2):
            yield Tab(self.legacy_name or "clipboard", _iter_model(self.reader))
            return

        if self.version == 3:
            raw_tabs = self.metadata.get("tabs", [])
            for index in range(len(raw_tabs)):
                raw_tab = raw_tabs[index]
                if not isinstance(raw_tab, dict):
                    raise DecodeError("registro de aba inválido no backup v3")
                name = _as_text(raw_tab.get("name"), "nome da aba")
                model = raw_tab.get("data")
                if not isinstance(model, (QByteArray, bytes, bytearray)):
                    raise DecodeError(f"dados da aba {name} não são binários")
                yield Tab(name, _iter_model(QtReader(model)))
                raw_tabs[index] = None
            return

        if self.version == 4:
            while not self.reader.at_end():
                raw_tab = self.reader.read_variant_map("dados da aba")
                name = _as_text(raw_tab.get("name"), "nome da aba")
                model = raw_tab.get("data")
                if not isinstance(model, (QByteArray, bytes, bytearray)):
                    raise DecodeError(f"dados da aba {name} não são binários")
                yield Tab(name, _iter_model(QtReader(model)))
            return

        for name in self.tab_names:
            tab_map = _read_v5_tab_map(self.reader, self.key)
            model = _as_bytes(tab_map.get("data"), f"dados da aba {name}")
            yield Tab(name, _iter_model(QtReader(model, QDataStream.Qt_4_7)))


def _safe_component(value: str) -> str:
    normalized = unicodedata.normalize("NFKC", value).strip()
    chars = [char if char.isalnum() or char in "-_" else "_" for char in normalized]
    result = "".join(chars).strip("._-")[:72]
    return result or "tab"


def _file_extension(mime: str) -> str:
    special = SPECIAL_EXTENSIONS.get(mime.lower())
    if special:
        return special
    guessed = mimetypes.guess_extension(mime.split(";", 1)[0].strip().lower())
    if guessed:
        extension = guessed.lstrip(".").lower()
        if extension and all(char.isalnum() or char in "-_" for char in extension):
            return extension
    mime_slug = _safe_component(mime.lower().replace("/", "-").replace("+", "-")).lower()
    return f"format-{mime_slug or 'unknown'}.bin"


def _unique_filename(stem: str, extension: str, used: set[str]) -> str:
    filename = f"{stem}.{extension}"
    suffix = 2
    while filename in used:
        filename = f"{stem}_{suffix:02d}.{extension}"
        suffix += 1
    used.add(filename)
    return filename


def _json_value(value: Any) -> Any:
    if isinstance(value, QByteArray):
        data = bytes(value)
        return {"qt_type": "QByteArray", "base64": base64.b64encode(data).decode("ascii")}
    if isinstance(value, dict):
        return {str(key): _json_value(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [_json_value(item) for item in value]
    if value is None or isinstance(value, (str, bool, int, float)):
        return value
    return {"qt_type": type(value).__name__, "value": str(value)}


def _write_json(path: Path, value: Any) -> None:
    with path.open("w", encoding="utf-8", newline="\n") as output:
        json.dump(value, output, ensure_ascii=False, indent=2)
        output.write("\n")


def extract_archive(archive_path: Path, output: Path) -> tuple[int, int, int]:
    output = output.expanduser().absolute()
    if output.exists():
        raise DecodeError(f"a pasta de destino já existe; escolha outra: {output}")
    with ArchiveReader(archive_path) as archive:
        parent = output.parent
        parent.mkdir(parents=True, exist_ok=True)
        staging = Path(tempfile.mkdtemp(prefix=f".{output.name}.tmp-", dir=parent))
        total_items = 0
        total_tabs = 0
        commands = archive.metadata.get("commands", [])
        command_count = len(commands) if isinstance(commands, (list, tuple)) else 0

        try:
            manifest_path = staging / "manifest.json"
            with manifest_path.open("x", encoding="utf-8", newline="\n") as manifest:
                manifest.write("{\n  \"format\": \"CopyQ CPQ\",\n")
                manifest.write(f"  \"archive_version\": {archive.version},\n  \"tabs\": [")

                for tab_number, tab in enumerate(archive.iter_tabs(), start=1):
                    total_tabs = tab_number
                    tab_dir_name = f"{tab_number:03d}_{_safe_component(tab.name)}"
                    tab_dir = staging / tab_dir_name
                    tab_dir.mkdir()
                    if tab_number > 1:
                        manifest.write(",")
                    manifest.write("\n    {")
                    manifest.write(f"\"index\": {tab_number}, ")
                    manifest.write("\"name\": ")
                    manifest.write(json.dumps(tab.name, ensure_ascii=False))
                    manifest.write(", \"directory\": ")
                    manifest.write(json.dumps(tab_dir_name))
                    manifest.write(", \"items\": [")

                    item_count = 0
                    for item_number, formats in enumerate(tab.items, start=1):
                        total_items += 1
                        item_count = item_number
                        stem = f"{item_number:06d}"
                        used_names: set[str] = set()
                        item_files = []
                        for mime, payload in formats.items():
                            extension = _file_extension(mime)
                            filename = _unique_filename(stem, extension, used_names)
                            item_path = tab_dir / filename
                            with item_path.open("xb") as file:
                                file.write(payload)
                            item_files.append({
                                "mime": mime,
                                "file": f"{tab_dir_name}/{filename}",
                                "bytes": len(payload),
                            })
                        if item_number > 1:
                            manifest.write(",")
                        manifest.write("\n      ")
                        json.dump(
                            {"index": item_number, "files": item_files},
                            manifest,
                            ensure_ascii=False,
                        )

                    manifest.write(f"], \"item_count\": {item_count}}}")

                manifest.write("\n  ],\n")

                if archive.metadata.get("settings"):
                    _write_json(staging / "settings.json", _json_value(archive.metadata["settings"]))
                if commands:
                    _write_json(staging / "commands.json", _json_value(commands))

                manifest.write(f"  \"total_items\": {total_items},\n")
                manifest.write(f"  \"has_settings\": {bool(archive.metadata.get('settings'))},\n")
                manifest.write(f"  \"command_count\": {command_count}\n}}\n")

            staging.rename(output)
        except Exception:
            shutil.rmtree(staging, ignore_errors=True)
            raise

        return total_items, total_tabs, archive.version


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="cpq-extract",
        description="Extrai um backup .cpq em arquivos ordenados por aba e item.",
    )
    parser.add_argument("arquivo", type=Path, help="backup .cpq do CopyQ")
    parser.add_argument("pasta", type=Path, help="nova pasta onde os itens serão extraídos")
    return parser


def main() -> int:
    args = build_parser().parse_args()
    try:
        total_items, total_tabs, _version = extract_archive(args.arquivo, args.pasta)
    except (DecodeError, OSError) as error:
        print(f"Erro: {error}", file=sys.stderr)
        return 1
    except Exception as error:
        print(f"Erro inesperado ao decodificar o backup: {error}", file=sys.stderr)
        return 1

    print(f"Extraídos {total_items} itens de {total_tabs} abas para {args.pasta.expanduser().absolute()}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
