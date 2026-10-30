# mib2-shared-crates

Shared Rust libraries for Android Auto and CarPlay extenders on MIB2 head units.
The repository also supplies a read-only file checksum command.
It contains no firmware or native library implementation.

See [Terms](docs/TERMS.md) for the technical names and
[Settings](docs/SETTINGS.md) for configuration files and values.

## Fetch and build

Install Git and Podman or Docker. Fetch the source:

```sh
git clone https://github.com/korhojoa/mib2-shared-crates.git
```

From the new source folder, run one build command:

```sh
./build.sh
```

The command uses the public compiler image pinned in `compiler-image.txt`.
The build treats enabled Clippy warnings as errors. It uses the default Clippy groups and crate-level lint attributes.
It fetches locked dependencies, checks formatting and Clippy, runs host tests, and builds the ARM QNX workspace.
Set `CONTAINER_ENGINE=docker` to use Docker. The default engine is Podman.
Set `CARGO_TARGET_DIR` to reuse a separate target cache.
The build uses the normal Cargo cache and `target` folder.
The command binaries are `target/armv7-unknown-nto-qnx650/release/android-auto-settings`,
`target/armv7-unknown-nto-qnx650/release/carplay-settings` and
`target/armv7-unknown-nto-qnx650/release/file-checksum`.
If you set `CARGO_TARGET_DIR`, replace `target` with that folder.
The released Android Auto settings helper is `build/android-auto-settings`.
Its dedicated producer uses the retained version 0.3.4+1 source and compiler
options. It requires the complete released file hash to match.

## Libraries

| Crate | Function |
| --- | --- |
| `h264-ts-mux` | Parse H.264 fields and make MPEG-TS blocks. |
| `display-manager-control` | Declare contexts and select displays. |
| `dm-gles-window` | Create windows and draw with EGL and GLES2. |
| `mpe-encoder-state` | Read encoder events and publish PPS state. |
| `extender-settings` | Read Android Auto and CarPlay loader settings. |
| `file-checksum` | Print SHA256 checksums for files. |
| `aawg-creds` | Read wireless Android Auto access-point credentials. |
| `btsvc-core` | Parse Bluetooth reports and manage wireless connection state. |

The root workspace uses one lock file.
Applications select libraries through Cargo path dependencies.
Installation of the extenders is described in the [combined guide](https://github.com/korhojoa/mib2-extenders/blob/main/INSTALL.md).

## Configuration

The loaders read their adjacent configuration files. See
[Settings](docs/SETTINGS.md) for the reader rules and optional command tools.
These commands do not start automatically. The wireless Android Auto release
includes the Android Auto settings helper.

## File checksums

Run `file-checksum FILE...` to read files and print standard SHA256 checksum lines.
Use `--` before a filename such as `--help`.
The command escapes filenames when required.
A file, read or output error returns a nonzero status.
It does not verify checksum lists or install files.

## Status

The component test used a MIB2 High unit with firmware MHI2_ER_VWG13_P4512_2.

GAL is the Android Auto receiver process on the head unit. Its map test
exercised H.264 transport muxing, display-manager control,
encoder event observation and the settings reader. The map appeared on the
first connection. There were no stream write errors. Native navigation
returned after disconnect. This does not test every API in these libraries.

| Feature | Status |
| --- | --- |
| H.264 muxing, display-manager control, encoder observation and settings reader | Exercised by the GAL test above. |
| GLES window drawing | No recorded unit result for this library. |
| Android Auto settings helper 0.3.4+1 | Used by the owned settings page on the tested unit. |
| Current CarPlay settings command | No recorded unit result for this version. |
| Wireless credential reader | The PPS parser was used by wireless Android Auto on the tested unit. The access-point command fallback needs a separate test. |
| Bluetooth state modules | Parked. Some modules need a Bluetooth service that is not released. |
| File checksum command | Host tests and ARM link checks passed. No recorded unit result. |

Host tests check data handling. Target builds check ARM QNX compilation and
linking. These checks do not qualify a complete unit installation.

## License

All crates use `MIT OR Apache-2.0`. You can choose either license.
See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).
