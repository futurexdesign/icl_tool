# icl_tool (Rust)

`icl_tool` is a small Rust CLI to **encode** and **decode** DWIN/DGUS-style ICL image bundles (`DGUS_3`), and to **visualise DWIN flash sector allocation** in a `DWIN_SET` folder.

- **Encode**: build an `.icl` file from either already-JPEG inputs or from ordinary images (PNG/JPEG/etc).
- **Decode**: extract JPEGs from an existing `.icl` and write a small JSON summary report.
- **Sectors**: scan a `DWIN_SET` folder for overlapping flash assets and print a colour-coded sector map.

## Usage

Run help:

```bash
icl_tool --help
icl_tool -h
```

Pick exactly one mode flag per invocation. All “input” arguments for JPEG encoding are specified as `index=path` (e.g. `0=00.jpg`), where `index` selects the slot in the ICL offset table.

### `--encode-jpegs` / `-j`

Encode an ICL from **already-JPEG** files.

```bash
icl_tool --encode-jpegs -o <OUT_ICL> <INPUTS>...
icl_tool -j -o <OUT_ICL> <INPUTS>...
```

- **`-o`, `--output-file`**: output path (e.g. `out.icl`)
- **`INPUTS`**: one or more `index=path` pairs pointing to `.jpg` files
- **`--vendor-native-crc`**: write the CRC field using native-endian bytes (compatibility toggle)

Example:

```bash
icl_tool --encode-jpegs -o out.icl 0=00.jpg 1=01.jpg
icl_tool -j --output-file=out.icl 0=00.jpg 1=01.jpg
```

### `--encode` / `-e`

Encode an ICL from “ordinary” image files by converting them to JPEG first.

```bash
icl_tool --encode -o <OUT_ICL> -i <IN_DIR> [OPTIONS]
icl_tool -e -o <OUT_ICL> -i <IN_DIR> [OPTIONS]
```

- **`-o`, `--output-file`**: output path (e.g. `out.icl`)
- **`-i`, `--input-dir`**: directory containing input images (`.png`, `.jpg`, `.jpeg`), scanned non-recursively
- **`--jpeg-quality <0-100>`**: quality for the intermediate JPEG encoding (default: `75`)
- **`--vendor-native-crc`**: write the CRC field using native-endian bytes (compatibility toggle)

Example:

```bash
icl_tool --encode -o out.icl -i ./images --jpeg-quality 75
icl_tool -e --output-file=out.icl --input-dir=./images
```

### `--decode` / `-d`

Extract JPEGs from an ICL into a directory.

```bash
icl_tool --decode -i <ICL_PATH> -o <OUT_DIR>
icl_tool -d -i <ICL_PATH> -o <OUT_DIR>
```

- **`-i`, `--input-file`**: path to an `.icl` file
- **`-o`, `--output-dir`**: directory to write `NN.jpg` files and `extract_report.json`

Example:

```bash
icl_tool --decode -i 26.icl -o extracted_dir
icl_tool -d --input-file=26.icl --output-dir=extracted_dir
```

The command also prints a small JSON summary (CRC values, image count, and any warnings) to stdout.

### `--sectors` / `-s`

Visualise DWIN display flash sector allocation and detect overlapping files in a `DWIN_SET` folder.

```bash
icl_tool --sectors <FOLDER>
icl_tool -s <FOLDER>
```

- **`FOLDER`**: path to a project folder containing `DWIN_SET`, or to `DWIN_SET` itself
- Scans flashable assets (`.ICL`, `.HZK`, `.BIN`) whose names begin with a sector index (e.g. `32MAIN.ICL` → sector 32)
- Prints a colour-coded map of all 64 × 256 KiB sectors
- Exit code `0` when no overlaps are detected; exit code equals the number of overlapping files when conflicts exist

Example:

```bash
icl_tool --sectors /path/to/project
icl_tool -s /path/to/project/DWIN_SET
```

### Shell completions

Generate a ZSH completion script:

```bash
icl_tool --completions > _icl_tool
```

Place `_icl_tool` on your `$fpath`, then run `compinit` (or restart your shell).

## Build

Prerequisites:
- Rust toolchain (stable) with Cargo

Build:

```bash
cargo build
```

Run (development):

```bash
cargo run -- --help
cargo run -- -e -o out.icl -i ./images
cargo run -- -d -i 26.icl -o extracted_dir
```

Release build:

```bash
cargo build --release
```

The binary will be at `target/release/icl_tool`.

Install 
```bash
cargo install --path .
```
## Notes / behavior

- Supports sparse index tables by using `index=path` inputs, or numeric/`NN-name` filenames with `--encode`. Empty slots (missing indices) are stored as zero offsets — required for DGUS projects that skip picture IDs.
- With `--encode`, stems like `01.jpg` and `01-home.png` both map to index `1`. If any file lacks a leading numeric index, files are packed sequentially from `0` instead (which will misalign screens on-device).
- Uses marker-based JPEG parsing (rather than fixed offsets) to better match vendor formatting.
- Shows progress bars only when stderr is a TTY (they auto-disable in CI / redirected output).
