# CPQ Extractor

`cpq-extract` é um utilitário de linha de comando em Rust que extrai backups `.cpq` do CopyQ para arquivos ordenados por aba e item.

## Compilar e executar

Requer Rust 1.85 ou mais recente.

```sh
cargo build --release
./target/release/cpq-extract backup.cpq cpq-extraido
```

Para instalar o executável no diretório de binários do Cargo:

```sh
cargo install --path .
cpq-extract backup.cpq cpq-extraido
```

Backups v5 criptografados pedem a senha sem exibi-la no terminal. Para uso em scripts, passe um arquivo com a senha:

```sh
cpq-extract backup.cpq cpq-extraido --password-file senha.txt
```

A pasta de destino ainda não pode existir. O extrator grava primeiro em uma pasta temporária e a move para o destino quando termina.

## Arquivos extraídos

```text
cpq-extraido/
├── manifest.json
├── settings.json             # quando o backup inclui configurações
├── commands.json             # quando o backup inclui comandos
├── 001_clipboard/
│   ├── 000001.txt
│   ├── 000001.html
│   └── 000002.png
└── 002_Anotações/
    └── 000001.md
```

Os itens mantêm a ordem dentro de cada aba. Cada formato MIME salvo vira um arquivo; tipos comuns usam extensões como `.txt`, `.md`, `.html`, `.png` e `.jpg`. Formatos desconhecidos recebem a extensão `.bin`. O `manifest.json` relaciona cada item ao MIME type, ao caminho e ao tamanho dos arquivos. Um item pode gerar vários arquivos.

O extrator entende backups CopyQ v1 a v5, incluindo exports v5 criptografados. Caminhos de arquivos guardados como referências são preservados como dados; o extrator nunca abre esses caminhos.

As versões v1 e v2 podem ser lidas item por item. As versões v3 a v5 armazenam os dados de cada aba em blocos Qt, então o uso de memória pode chegar ao tamanho da maior aba. A v3 armazena os blocos das abas juntos e pode exigir memória próxima ao tamanho do backup.
