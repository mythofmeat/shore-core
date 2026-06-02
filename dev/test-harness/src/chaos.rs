use std::fs::OpenOptions;
use std::path::PathBuf;

use crate::config::TestConfigBuilder;
use crate::harness::TestHarness;
use crate::mock_llm::MockLlmSidecar;

/// Holds the persistent state of a crashed daemon so it can be rebooted.
///
/// Obtained by calling [`TestHarness::crash`]. The daemon tasks have been
/// aborted; the on-disk data in `tmp_dir` is intact for recovery testing.
#[derive(Debug)]
pub struct CrashedHarness {
    pub tmp_dir: tempfile::TempDir,
    pub mock_llm: MockLlmSidecar,
    pub data_dir: PathBuf,
    pub addr: String,
}

impl CrashedHarness {
    /// Boot a new daemon from the existing on-disk state, reusing the same
    /// temp directory, mock LLM server, data dir, and TCP address.
    pub async fn reboot(self) -> TestHarness {
        self.reboot_with(TestConfigBuilder::default()).await
    }

    /// Reboot using a specific `TestConfigBuilder`. Required when the
    /// original boot used non-default model aliases or extra characters
    /// — the persistent daemon state on disk is meaningful only against
    /// the same catalog/character set.
    pub async fn reboot_with(self, builder: TestConfigBuilder) -> TestHarness {
        let config = builder.build(self.tmp_dir.path(), &self.mock_llm.base_url());

        TestHarness::wire_daemon(
            config,
            self.mock_llm,
            self.tmp_dir,
            self.data_dir,
            self.addr,
        )
        .await
    }

    /// Overwrite a file inside `data_dir` with garbage bytes, simulating
    /// storage corruption.
    ///
    /// `relative_path` is resolved against `data_dir`.
    pub fn corrupt_file(&self, relative_path: &str) -> std::io::Result<()> {
        let path = self.data_dir.join(relative_path);
        let garbage: &[u8] = b"\x00\xff\xfe\xfd\xfc\xfb\xfa\xf9\xf8\xf7CORRUPT";
        std::fs::write(path, garbage)
    }

    /// Truncate a file inside `data_dir` to `bytes_to_keep` bytes.
    ///
    /// `relative_path` is resolved against `data_dir`.
    pub fn truncate_file(&self, relative_path: &str, bytes_to_keep: u64) -> std::io::Result<()> {
        let path = self.data_dir.join(relative_path);
        let file = OpenOptions::new().write(true).open(path)?;
        file.set_len(bytes_to_keep)?;
        // Flush to ensure the truncation is visible immediately.
        drop(file);
        Ok(())
    }
}
