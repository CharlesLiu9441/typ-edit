//! Package loading for `@preview/...` imports.
//!
//! Fonts and project files are local; packages are the one thing typst may need to
//! pull from the network. `typst-kit` supplies the standard data/cache/registry
//! layout, but its bundled downloader drags in a TLS stack we do not want on
//! Windows. [`CurlDownloader`] instead shells out to `curl.exe` (shipped with
//! Windows 10 1803+) with arguments passed as an array — never a shell.

use std::any::Any;
use std::io::{self, Read};
use std::process::Command;

use typst::syntax::package::PackageSpec;
use typst_kit::downloader::Downloader;
use typst_kit::packages::SystemPackages;

/// A [`Downloader`] backed by the `curl` executable.
///
/// Package archives are small, so the whole body is buffered into memory.
pub(crate) struct CurlDownloader;

impl Downloader for CurlDownloader {
    fn stream(&self, key: &dyn Any, url: &str) -> io::Result<(Option<usize>, Box<dyn Read>)> {
        let label = key
            .downcast_ref::<PackageSpec>()
            .map(ToString::to_string)
            .unwrap_or_else(|| "package index".to_owned());
        log::debug!("curl download {label} from {url}");

        let mut command = Command::new("curl");
        command.args(["-L", "--fail", "--silent", "--show-error", url]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let output = command
            .output()
            .map_err(|err| io::Error::other(format!("curl unavailable: {err}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(map_curl_failure(output.status.code(), stderr.trim()));
        }

        let bytes = output.stdout;
        Ok((Some(bytes.len()), Box::new(io::Cursor::new(bytes))))
    }
}

/// Maps a failed `curl` run to an [`io::Error`].
///
/// `curl --fail` exits with code 22 on every HTTP error response (including a
/// `404`), which the package loader must see as [`io::ErrorKind::NotFound`] to
/// distinguish a missing package from a network failure.
fn map_curl_failure(exit_code: Option<i32>, stderr: &str) -> io::Error {
    let kind = match exit_code {
        Some(22) => io::ErrorKind::NotFound,
        _ => io::ErrorKind::Other,
    };
    let message = if stderr.is_empty() {
        format!("curl exited with {exit_code:?}")
    } else {
        stderr.to_owned()
    };
    io::Error::new(kind, message)
}

/// The shared curl-backed [`Downloader`], for callers that need to fetch
/// registry resources directly (for example the package index).
pub(crate) fn curl_downloader() -> CurlDownloader {
    CurlDownloader
}

/// The standard system package loader: data dir, then cache dir, then the
/// official Typst Universe registry — same sources as the typst CLI.
pub(crate) fn system_packages() -> SystemPackages {
    SystemPackages::new(curl_downloader())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_errors_map_to_not_found() {
        let err = map_curl_failure(Some(22), "curl: (22) The requested URL returned error: 404");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn other_failures_keep_the_stderr_message() {
        let err = map_curl_failure(Some(7), "curl: (7) Failed to connect to host");
        assert_eq!(err.kind(), io::ErrorKind::Other);
        assert!(err.to_string().contains("Failed to connect"));
    }

    #[test]
    fn stream_reports_connection_failures() {
        // Port 1 is never listening; no network traffic leaves the machine.
        match CurlDownloader.stream(&"test", "http://127.0.0.1:1/x") {
            Err(err) => assert_eq!(err.kind(), io::ErrorKind::Other),
            Ok(_) => panic!("connection to port 1 must fail"),
        }
    }
}
