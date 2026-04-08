use std::fs::OpenOptions;
use std::path::PathBuf;

use crate::config::TestConfigBuilder;
use crate::harness::TestHarness;
use crate::mock_llm::MockLlmServer;

/// Holds the persistent state of a crashed daemon so it can be rebooted.
///
/// Obtained by calling [`TestHarness::crash`]. The daemon tasks have been
/// aborted and the socket file removed; the on-disk data in `tmp_dir` is
/// intact for recovery testing.
pub struct CrashedHarness {
    pub tmp_dir: tempfile::TempDir,
    pub mock_llm: MockLlmServer,
    pub data_dir: PathBuf,
    pub socket_path: PathBuf,
}

impl CrashedHarness {
    /// Boot a new daemon from the existing on-disk state, reusing the same
    /// temp directory, mock LLM server, data dir, and socket path.
    pub async fn reboot(self) -> TestHarness {
        let config = TestConfigBuilder::default().build(
            self.tmp_dir.path(),
            &self.mock_llm.base_url(),
        );

        TestHarness::wire_daemon(
            config,
            self.mock_llm,
            self.tmp_dir,
            self.data_dir,
            self.socket_path,
        )
        .await
    }

    /// Overwrite a file inside `data_dir` with garbage bytes, simulating
    /// storage corruption.
    ///
    /// `relative_path` is resolved against `data_dir`.
    pub fn corrupt_file(&self, relative_path: &str) {
        let path = self.data_dir.join(relative_path);
        let garbage: &[u8] = b"\x00\xff\xfe\xfd\xfc\xfb\xfa\xf9\xf8\xf7CORRUPT";
        std::fs::write(&path, garbage)
            .unwrap_or_else(|e| panic!("corrupt_file failed for {}: {}", path.display(), e));
    }

    /// Truncate a file inside `data_dir` to `bytes_to_keep` bytes.
    ///
    /// `relative_path` is resolved against `data_dir`.
    pub fn truncate_file(&self, relative_path: &str, bytes_to_keep: u64) {
        let path = self.data_dir.join(relative_path);
        let file = OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap_or_else(|e| panic!("truncate_file: could not open {}: {}", path.display(), e));
        file.set_len(bytes_to_keep).unwrap_or_else(|e| {
            panic!("truncate_file: set_len failed for {}: {}", path.display(), e)
        });
        // Flush to ensure the truncation is visible immediately.
        drop(file);
    }
}
