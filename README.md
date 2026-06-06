# icl_tool (Rust)

`icl_tool` is a small Rust CLI to **encode** and **decode** DWIN/DGUS-style ICL image bundles (`DGUS_3`).

- **Encode**: build an `.icl` file from either already-JPEG inputs or from ordinary images (PNG/JPEG/etc).
- **Decode**: extract JPEGs from an existing `.icl` and write a small JSON summary report.

## Usage

Run help:

```bash
icl_tool --help
icl_tool <command> --help
```

All “input” arguments for encoding are specified as `index=path` (e.g. `0=00.jpg`), where `index` selects the slot in the ICL offset table.

### `encode-jpegs`

Encode an ICL from **already-JPEG** files.

```bash
icl_tool encode-jpegs [OPTIONS] <OUT_ICL> [INPUTS]...
```

- **`OUT_ICL`**: output path (e.g. `out.icl`)
- **`INPUTS`**: one or more `index=path` pairs pointing to `.jpg` files
- **`--vendor-native-crc`**: write the CRC field using native-endian bytes (compatibility toggle)

Example:

```bash
icl_tool encode-jpegs out.icl 0=00.jpg 1=01.jpg
```

### `encode`

Encode an ICL from “ordinary” image files by converting them to JPEG first.

```bash
icl_tool encode [OPTIONS] <OUT_ICL> <IN_DIR>
```

- **`OUT_ICL`**: output path (e.g. `out.icl`)
- **`IN_DIR`**: directory containing input images (`.png`, `.jpg`, `.jpeg`), scanned non-recursively
- **`--jpeg-quality <0-100>`**: quality for the intermediate JPEG encoding (default: `75`)
- **`--vendor-native-crc`**: write the CRC field using native-endian bytes (compatibility toggle)

Example:

```bash
icl_tool encode out.icl ./images --jpeg-quality 75
```

### `decode`

Extract JPEGs from an ICL into a directory.

```bash
icl_tool decode <ICL_PATH> <OUT_DIR>
```

- **`ICL_PATH`**: path to an `.icl` file
- **`OUT_DIR`**: directory to write `NN.jpg` files and `extract_report.json`

Example:

```bash
icl_tool decode 26.icl extracted_dir
```

The command also prints a small JSON summary (CRC values, image count, and any warnings) to stdout.

## Build

Prerequisites:
- Rust toolchain (stable) with Cargo

Build:

```bash
cargo build
```

Run (development):

```bash
cargo run -- <command> [args...]
```

Release build:

```bash
cargo build --release
```

The binary will be at `target/release/icl_tool`.

## Notes / behavior

- Supports sparse index tables by using `index=path` inputs.
- Uses marker-based JPEG parsing (rather than fixed offsets) to better match vendor formatting.
- Shows progress bars only when stderr is a TTY (they auto-disable in CI / redirected output).
