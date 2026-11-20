# Models

Sorta ships **no weights**. This directory is where they go, and `sorta models`
reports what is here.

```
sorta models                 # what is present, pinned, missing
sorta models --verify        # check pinned digests
```

## What is expected

| Id | File | Purpose | Input → output | Licence | Feature |
|---|---|---|---|---|---|
| `intent-deberta` | `intent-deberta.rten` | document intent (invoice, contract, statement, letter) | text ≤512 tokens → label + confidence | MIT | `model-intent-deberta` |
| `embed-minilm` | `embed-minilm.rten` | 384-dim embeddings for clustering and near-duplicates | text ≤256 tokens → 384 f32 | Apache-2.0 | `model-embed-minilm` |
| `ner-gliner` | `ner-gliner.rten` | entities: client, date, amount, reference | text + label set → spans | Apache-2.0 | `model-ner-gliner` |
| `ocr-det` | `ocr-det.rten` | find text boxes on a scanned page | image, 960 px long side → boxes + scores | Apache-2.0 | `model-ocr-rapid` |
| `ocr-rec` | `ocr-rec.rten` | read a text box | crop, 48 px height → text + score | Apache-2.0 | `model-ocr-rapid` |
| `extract-sanga` | `extract-sanga.rten` | field extraction for known document types | text → field map | to be confirmed | `model-extract-sanga` |

## How to produce one

1. Fetch the upstream weights named in the table (`sorta models` prints the source).
2. Export to ONNX — for Hugging Face encoders, Optimum does it:
   `optimum-cli export onnx --model <hf-id> out/`.
3. Convert to `.rten` with `rten-convert` from the
   [`rten`](https://github.com/robertknight/rten) project.
4. Put the file here under the exact name in the table.
5. Pin its digest so `--verify` means something:

   ```
   b3sum models/intent-deberta.rten
   ```

   and record it in the `digest` field of the matching spec in `src/models.rs`.
6. Rebuild with the feature: `cargo build --release --features model-intent-deberta`.

## What happens without them

Nothing breaks. Classification runs on format, keyword rules and the confidence gate;
extraction runs on the PDF/OOXML backends; near-duplicates need an embedding engine
(`[embedding] key_command`) or a model here. `sorta models` says "No weights" and the
tool keeps working.

## Why `.rten` and not `.onnx`

`rten` is a pure-Rust ONNX runtime, so a model adds no C or C++ toolchain to the
build — the same reason the rest of this project avoids native dependencies. An
`.onnx` file works too if `rten` can load it directly; the `.rten` format is smaller
and loads faster.

## Licences

The licence column is the upstream model's, not Sorta's. Check it before shipping a
build that bundles weights: MIT and Apache-2.0 both need their notice carried, and
`NOTICE` is where Sorta keeps those.
