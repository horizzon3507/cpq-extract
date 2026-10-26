# CPQ Extractor

Utilitário local para extrair backups `.cpq` do CopyQ. Ele mantém a ordem dos itens, separa as abas em diretórios e grava cada formato de clipboard como um arquivo, com um `manifest.json` relacionando item, MIME type e nome do arquivo.

## Uso

Requer Python 3.10 ou mais recente, PyQt5 e `cryptography`. A partir da pasta raiz deste repositório, rode diretamente:

```sh
python3 -m exitq.decoder exitq/cop.cpq cpq-extraido
```

Não é necessário instalar o projeto para usar esse comando. Para criar o comando `cpq-extract`, instale em um ambiente virtual:

```sh
python3 -m venv .venv
source .venv/bin/activate
python -m pip install .
cpq-extract exitq/cop.cpq cpq-extraido
```

Backups criptografados pedem a senha no terminal sem mostrá-la na tela. A senha não é gravada. A pasta de destino precisa ainda não existir; isso evita misturar a extração com arquivos preexistentes.

## Estrutura de saída

```text
cpq-extraido/
├── manifest.json
├── settings.json             # se o backup contiver configurações
├── commands.json             # se o backup contiver comandos
├── 001_clipboard/
│   ├── 000001.txt
│   ├── 000001.html
│   └── 000002.png
└── 002_notes/
    └── 000001.md
```

Os nomes numéricos seguem a ordem dos itens dentro da aba no backup. As extensões vêm do MIME type: `text/plain` vira `.txt`, `text/markdown` vira `.md`, e imagens mantêm extensões como `.png`, `.jpg` ou `.svg`. Formatos desconhecidos são preservados como `.bin`; o MIME original aparece no manifesto. Um item pode gerar vários arquivos, um para cada formato que o CopyQ guardou.

O decodificador entende exports CopyQ v1 a v5. A estrutura binária usa `QDataStream` do Qt; exports v5 com senha usam a criptografia do CopyQ. Caminhos de arquivo encontrados dentro do backup são preservados como referências e nunca são abertos pelo extrator.
