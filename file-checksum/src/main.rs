// SPDX-License-Identifier: MIT OR Apache-2.0
//! Print SHA256 file checksums in the standard sum-line format.

use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::ExitCode;

fn checksum(mut input: impl Read) -> io::Result<[u8; 32]> {
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        match input.read(&mut buffer) {
            Ok(0) => return Ok(digest.finalize().into()),
            Ok(count) => digest.update(&buffer[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

fn sum_line(output: &mut impl Write, digest: &[u8; 32], path: &Path) -> io::Result<()> {
    let name = path.as_os_str().as_encoded_bytes();
    let escaped = name
        .iter()
        .any(|byte| matches!(byte, b'\\' | b'\n' | b'\r'));
    if escaped {
        output.write_all(b"\\")?;
    }
    for byte in digest {
        write!(output, "{byte:02x}")?;
    }
    output.write_all(b"  ")?;
    for byte in name {
        match byte {
            b'\\' => output.write_all(b"\\\\")?,
            b'\n' => output.write_all(b"\\n")?,
            b'\r' => output.write_all(b"\\r")?,
            byte => output.write_all(&[*byte])?,
        }
    }
    output.write_all(b"\n")
}

fn run(
    arguments: impl IntoIterator<Item = OsString>,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> io::Result<bool> {
    let mut arguments = arguments.into_iter().peekable();
    if arguments.peek().is_none() {
        writeln!(errors, "Usage: file-checksum [--] FILE...")?;
        return Ok(false);
    }
    if arguments.peek().is_some_and(|arg| arg == "--help") {
        writeln!(output, "Usage: file-checksum [--] FILE...")?;
        writeln!(
            output,
            "Print each file's SHA256 checksum. Files are read only."
        )?;
        return Ok(true);
    }
    if arguments.peek().is_some_and(|arg| arg == "--") {
        arguments.next();
    }
    let mut success = true;
    let mut count = 0;
    for argument in arguments {
        count += 1;
        let path = Path::new(&argument);
        match File::open(path).and_then(checksum) {
            Ok(digest) => sum_line(output, &digest, path)?,
            Err(error) => {
                writeln!(errors, "file-checksum: {}: {error}", path.display())?;
                success = false;
            }
        }
    }
    if count == 0 {
        writeln!(errors, "file-checksum: no files specified")?;
        success = false;
    }
    output.flush()?;
    Ok(success)
}

fn main() -> ExitCode {
    let stdout = io::stdout();
    let stderr = io::stderr();
    match run(
        std::env::args_os().skip(1),
        &mut stdout.lock(),
        &mut stderr.lock(),
    ) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            let _ = writeln!(stderr.lock(), "file-checksum: output failed: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        for (input, expected) in [
            (
                b"".as_slice(),
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                b"abc".as_slice(),
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
        ] {
            let mut line = Vec::new();
            sum_line(&mut line, &checksum(input).unwrap(), Path::new("file")).unwrap();
            assert_eq!(
                String::from_utf8(line).unwrap(),
                format!("{expected}  file\n")
            );
        }
    }

    #[test]
    fn streams_across_buffer_boundaries() {
        let bytes = vec![b'a'; 1_000_000];
        let mut line = Vec::new();
        sum_line(
            &mut line,
            &checksum(bytes.as_slice()).unwrap(),
            Path::new("million"),
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(line).unwrap(),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0  million\n"
        );
    }

    #[test]
    fn escapes_sum_line_names() {
        let mut output = Vec::new();
        sum_line(&mut output, &[0; 32], Path::new("a\\b\nc\r")).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            format!("\\{}  a\\\\b\\nc\\r\n", "00".repeat(32))
        );
    }

    #[cfg(unix)]
    #[test]
    fn preserves_non_utf8_name_bytes() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let mut output = Vec::new();
        let name = Path::new(OsStr::from_bytes(b"name-\xff"));
        sum_line(&mut output, &[0; 32], name).unwrap();
        let mut expected = "00".repeat(32).into_bytes();
        expected.extend_from_slice(b"  name-\xff\n");
        assert_eq!(output, expected);
    }

    #[test]
    fn reader_failure_has_no_digest() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("read failure"))
            }
        }
        assert!(checksum(Broken).is_err());
    }

    #[test]
    fn interrupted_read_is_retried() {
        struct Interrupted(bool);
        impl Read for Interrupted {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                if std::mem::replace(&mut self.0, false) {
                    Err(io::Error::from(io::ErrorKind::Interrupted))
                } else {
                    Ok(0)
                }
            }
        }
        assert_eq!(
            checksum(Interrupted(true)).unwrap(),
            checksum(b"".as_slice()).unwrap()
        );
    }

    #[test]
    fn output_failure_is_reported() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::BrokenPipe))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        assert!(sum_line(&mut Broken, &[0; 32], Path::new("file")).is_err());
    }
}
