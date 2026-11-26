# Third-party notices

CVS Desk is licensed under `AGPL-3.0-only`. It depends on the following Rust crates. Each
crate remains under its own license; the license text is available from the crate's
repository and from the crate package published on crates.io.

## Direct dependencies

| Crate | Version | License |
|---|---|---|
| `chrono` | 0.4 | MIT OR Apache-2.0 |
| `eframe` | 0.29 | MIT OR Apache-2.0 |
| `egui` | 0.29 | MIT OR Apache-2.0 |
| `egui-remixicon` | 0.29 | MIT OR Apache-2.0 |
| `encoding_rs` | 0.8 | (Apache-2.0 OR MIT) AND BSD-3-Clause |
| `flate2` | 1 | MIT OR Apache-2.0 |
| `image` | 0.25 | MIT OR Apache-2.0 |
| `rfd` | 0.18 | MIT |
| `toml` | 0.8 | MIT OR Apache-2.0 |
| `ttf-parser` | 0.25 | MIT OR Apache-2.0 |
| `unicode-width` | 0.1 | MIT OR Apache-2.0 |
| `winreg` | 0.56 | MIT |

The build also pulls in the transitive dependencies listed in `Cargo.lock`. Before
distributing a binary, include the license texts for every dependency in the resolved build.

## Fonts and icons

The file-type icons come from Remix Icon, distributed under the Apache License 2.0.

## CVS protocol compatibility

CVS Desk implements the CVS client protocol as an independent implementation.
