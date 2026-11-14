//! Runs LibRaw in a separate process, so a crash or hang inside it (a corrupt or malicious
//! RAW) only kills the helper: the app reports a decode error instead of dying, and does not
//! crash again on every launch while pre-decoding the last folder.
//!
//! The app re-runs its own binary as `photobrowser --libraw-helper preview <edge> <path>` or
//! `... full <path>`. The helper writes `MAGIC ‖ width ‖ height ‖ orientation ‖ RGBA` to stdout
//! (integers little-endian), or a message to stderr with exit status 1.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::{DecodeError, Rgba, jpeg::MAX_DECODE_PIXELS, raw};

/// First argument that switches the binary into helper mode.
pub const HELPER_FLAG: &str = "--libraw-helper";

const MAGIC: &[u8; 4] = b"PBRW";
const HEADER_LEN: usize = 4 + 4 + 4 + 1;

/// How long a helper may run before it is killed.
pub const PREVIEW_TIMEOUT: Duration = Duration::from_secs(30);
pub const FULL_TIMEOUT: Duration = Duration::from_secs(120);

/// Helper-mode entry point; `args` follow `HELPER_FLAG`. Returns the process exit status.
pub fn helper_main(args: Vec<OsString>) -> i32 {
    let result = match args.as_slice() {
        [mode, edge, path] if mode == "preview" => match edge.to_str().and_then(|e| e.parse().ok())
        {
            Some(edge) => raw::libraw_preview(Path::new(path), edge),
            None => return usage(),
        },
        [mode, path] if mode == "full" => raw::libraw_full(Path::new(path)),
        _ => return usage(),
    };
    match result {
        Ok((img, orientation)) => {
            let mut out = io::stdout().lock();
            match out
                .write_all(&encode(&img, orientation))
                .and_then(|()| out.flush())
            {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("couldn't write the decoded image: {e}");
                    1
                }
            }
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

fn usage() -> i32 {
    eprintln!("usage: {HELPER_FLAG} preview <edge> <path> | full <path>");
    2
}

/// Arguments for a preview decode of `path`, long edge ≥ `target_edge` where possible.
pub fn preview_args(path: &Path, target_edge: u32) -> Vec<OsString> {
    vec![
        HELPER_FLAG.into(),
        "preview".into(),
        target_edge.to_string().into(),
        path.into(),
    ]
}

/// Arguments for a full-resolution decode of `path`.
pub fn full_args(path: &Path) -> Vec<OsString> {
    vec![HELPER_FLAG.into(), "full".into(), path.into()]
}

pub fn encode(img: &Rgba, orientation: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + img.data.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&img.width.to_le_bytes());
    out.extend_from_slice(&img.height.to_le_bytes());
    out.push(orientation);
    out.extend_from_slice(&img.data);
    out
}

/// Parses the helper's stdout, rejecting anything malformed or over the pixel budget.
pub fn decode(mut bytes: Vec<u8>) -> Result<(Rgba, u8), String> {
    if bytes.len() < HEADER_LEN || &bytes[..4] != MAGIC {
        return Err("RAW decoder returned malformed output".into());
    }
    let width = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    let height = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    let orientation = bytes[12];
    let pixels = width as usize * height as usize;
    if pixels > MAX_DECODE_PIXELS || bytes.len() - HEADER_LEN != pixels * 4 {
        return Err("RAW decoder returned malformed output".into());
    }
    bytes.drain(..HEADER_LEN);
    Ok((Rgba::new(width, height, bytes), orientation))
}

/// Runs `program` with `args` as a helper and returns its image. A crash (signal), a timeout
/// (the helper is killed), a failure exit or malformed output all become `DecodeError::Raw`.
pub fn run_helper(
    program: &Path,
    args: &[OsString],
    timeout: Duration,
) -> Result<(Rgba, u8), DecodeError> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Read both pipes on their own threads, so a full pipe can never stall the helper.
    let stdout = read_all(child.stdout.take());
    let stderr = read_all(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(DecodeError::Raw(format!(
                "RAW decoder timed out after {} s",
                timeout.as_secs()
            )));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    if let Some(signal) = status.signal() {
        return Err(DecodeError::Raw(format!(
            "RAW decoder crashed on this file (signal {signal})"
        )));
    }
    if !status.success() {
        let message = String::from_utf8_lossy(&stderr).trim().to_owned();
        return Err(DecodeError::Raw(if message.is_empty() {
            "RAW decoder failed".into()
        } else {
            message
        }));
    }
    decode(stdout).map_err(DecodeError::Raw)
}

fn read_all(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    })
}

/// This binary, for re-running as a helper.
pub fn current_program() -> io::Result<PathBuf> {
    std::env::current_exe()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str, timeout: Duration) -> Result<(Rgba, u8), DecodeError> {
        run_helper(Path::new("/bin/sh"), &["-c".into(), script.into()], timeout)
    }

    fn message(r: Result<(Rgba, u8), DecodeError>) -> String {
        match r {
            Err(DecodeError::Raw(m)) => m,
            other => panic!(
                "expected a RAW error, got {:?}",
                other.map(|(i, o)| (i.width, o))
            ),
        }
    }

    #[test]
    fn encode_then_decode_roundtrips() {
        let img = Rgba::new(3, 2, (0..24).collect());
        let (back, orientation) = decode(encode(&img, 6)).unwrap();
        assert_eq!((back.width, back.height, orientation), (3, 2, 6));
        assert_eq!(back.data, (0..24).collect::<Vec<u8>>());
    }

    #[test]
    fn malformed_output_is_rejected() {
        let mut short = encode(&Rgba::new(2, 2, vec![0; 16]), 1);
        short.pop();
        assert!(decode(short).is_err());
        assert!(decode(b"nope".to_vec()).is_err());
        let mut huge = MAGIC.to_vec();
        huge.extend_from_slice(&u32::MAX.to_le_bytes());
        huge.extend_from_slice(&u32::MAX.to_le_bytes());
        huge.push(1);
        assert!(decode(huge).is_err());
    }

    #[test]
    fn crashing_helper_is_an_error_not_a_crash() {
        let m = message(sh("kill -SEGV $$", Duration::from_secs(10)));
        assert!(m.contains("crashed"), "{m}");
    }

    #[test]
    fn hanging_helper_is_killed() {
        let start = Instant::now();
        let m = message(sh("exec sleep 30", Duration::from_millis(200)));
        assert!(m.contains("timed out"), "{m}");
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn failing_helper_reports_its_message() {
        let m = message(sh(
            "echo 'not a RAW file' >&2; exit 1",
            Duration::from_secs(10),
        ));
        assert_eq!(m, "not a RAW file");
    }

    #[test]
    fn unknown_helper_arguments_are_a_usage_error() {
        assert_eq!(helper_main(vec!["bogus".into()]), 2);
        assert_eq!(
            helper_main(vec!["preview".into(), "x".into(), "/p".into()]),
            2
        );
    }
}
