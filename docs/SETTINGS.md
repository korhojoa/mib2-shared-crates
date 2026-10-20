<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->
# Settings

## Loader configuration

The application supplies the path of `gal_loader.conf` or `dio_loader.conf`
beside its loaded library. The reader does not write these files. Missing
keys use the default supplied by the calling application.

Both readers use signed decimal integer prefixes. Leading zeroes are decimal.
The first matching key wins. A value with no numeric prefix uses the caller
fallback. Overflow selects the nearest signed 32-bit bound.

| Key | Type | Minimum | Maximum | Default | Meaning |
| --- | --- | --- | --- | --- | --- |
| `extender_settings_dir` | Path text | Not applicable | Not applicable | Base configuration folder | Select the Android Auto override folder. A relative path starts at the base configuration folder. |

The feature names and numeric defaults belong to the consuming extender.
See the [GAL settings](https://github.com/korhojoa/gal-extender/blob/main/docs/SETTINGS.md)
and [DIO settings](https://github.com/korhojoa/dio-extender/blob/main/docs/SETTINGS.md).
The shared libraries do not select a cluster output mode.

## Optional settings commands

`android-auto-settings` and `carplay-settings` are parked command tools.
They do not start automatically. Put each executable beside its loader
configuration, or give an explicit configuration path.
The commands find files from their executable location, not the working folder.
Run a command without arguments to print its syntax.

A read prints a snapshot token. A save needs that token and refuses a changed
file. The Android Auto command saves overrides. The CarPlay command preserves
the base text and writes a leading managed section in `dio_loader.conf`.
The section delimiters are `# carplay-settings overrides v1` and
`# end carplay-settings overrides`. Keep the complete section when updating
a configuration. Do not replace existing settings with an example file.

The helper validation descriptors are in
[`schema.rs`](../extender-settings/src/schema.rs) and
[`carplay.rs`](../extender-settings/src/carplay.rs). Those helper limits are
not additional runtime clamps in the consuming loaders.

## Wireless credential reader

These environment values are read by `aawg-creds`. This library does not
start an access point or a Bluetooth service. The service that uses some
`btsvc-core` modules is not released.

| Variable | Type | Minimum | Maximum | Default | Meaning |
| --- | --- | --- | --- | --- | --- |
| `AAWG_IP` | Text | Not applicable | Not applicable | Empty | Access-point address override. |
| `AAWG_PORT` | Unsigned 32-bit integer | 0 | 4294967295 | 5288 | Phone connection port. Invalid text uses the default. |
| `AAWG_SSID` | Text | Not applicable | Not applicable | Empty | Wireless network name override. |
| `AAWG_KEY` | Text | Not applicable | Not applicable | Empty | Wireless passphrase override. |
| `AAWG_KEY_FILE` | Path text | Not applicable | Not applicable | Unset | Read the passphrase from this file if no key override is supplied. |
| `AAWG_BSSID` | Text | Not applicable | Not applicable | Empty | Access-point hardware address override. |
| `AAWG_UAPUTL` | Executable name or path | Not applicable | Not applicable | `uaputl` | Native access-point query command. |
| `AAWG_UAP_IF` | Text | Not applicable | Not applicable | `uap0` | Interface queried by the native command. |

The native command fallback has no recorded unit qualification. Do not assume
that it supplies valid credentials on every firmware.

The other libraries accept arguments through their Rust APIs. They have no
loader configuration file. The `file-checksum FILE...` command reads only
its file arguments.
