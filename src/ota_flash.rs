//! OTA firmware flashing.
//!
//! Streams the uploaded firmware into the *inactive* OTA app slot using
//! ESP-IDF's `esp_ota_*` API. This never touches the running image: the new
//! firmware is written to the other slot, validated by `esp_ota_end` (magic
//! byte, segment layout, checksum), and only then marked for boot. NVS (WiFi
//! credentials, settings, shots, crash log) lives on its own partition and
//! survives untouched.
//!
//! Requires the two-slot partition table (`partitions_two_ota.csv`).
//!
//! Combined with `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE` and
//! `health::confirm_running_slot_valid()`, a bad image can neither boot
//! half-written nor brick the machine: validation rejects it before the
//! switch, and if a flipped image misbehaves before confirming itself, the
//! bootloader rolls back to the previous slot.
//!
//! The raw `esp_ota_*` bindings are used instead of `esp_idf_svc::ota::EspOta`
//! because `EspOtaUpdate` borrows its parent `EspOta` instance, which makes an
//! owned, streaming writer awkward; the raw API has no such lifetime coupling.

/// Maximum accepted firmware size: the size of one OTA app slot
/// (0x1D0000 bytes in `partitions_two_ota.csv`).
pub const FIRMWARE_MAX: usize = 0x1D0000;

#[derive(Debug)]
pub enum WriteError {
    /// The image exceeded the OTA slot size; the update was aborted and the
    /// inactive slot left clean.
    TooLarge,
    /// Flash-level failure; the update was aborted.
    Flash(String),
}

// --- ESP32 implementation -------------------------------------------------------

#[cfg(target_arch = "xtensa")]
mod imp {
    use esp_idf_svc::sys::{self, esp_ota_handle_t, esp_partition_t};

    /// Streaming firmware writer. Feed it HTTP body chunks as they arrive —
    /// nothing larger than the chunk is ever held in RAM (the ESP32 has ~300 KB
    /// of heap, far less than a firmware image).
    pub struct OtaWriter {
        handle: esp_ota_handle_t,
        partition: *const esp_partition_t,
        written: usize,
    }

    impl OtaWriter {
        /// Begins an OTA update on the inactive slot, erasing only the sectors
        /// needed for `expected_size` bytes.
        pub fn begin(expected_size: usize) -> anyhow::Result<Self> {
            if expected_size > super::FIRMWARE_MAX {
                anyhow::bail!(
                    "Firmware ({} bytes) exceeds OTA slot size ({} bytes)",
                    expected_size,
                    super::FIRMWARE_MAX
                );
            }
            let partition = unsafe { sys::esp_ota_get_next_update_partition(core::ptr::null()) };
            if partition.is_null() {
                anyhow::bail!("No inactive OTA app partition found (check partition table)");
            }
            let mut handle: esp_ota_handle_t = 0;
            sys::esp!(unsafe { sys::esp_ota_begin(partition, expected_size, &mut handle) })
                .map_err(|e| anyhow::anyhow!("esp_ota_begin failed: {e}"))?;
            Ok(Self {
                handle,
                partition,
                written: 0,
            })
        }

        /// Writes the next chunk.
        pub fn write(&mut self, chunk: &[u8]) -> Result<(), super::WriteError> {
            if self.written + chunk.len() > super::FIRMWARE_MAX {
                self.abort();
                return Err(super::WriteError::TooLarge);
            }
            sys::esp!(unsafe { sys::esp_ota_write(self.handle, chunk.as_ptr() as _, chunk.len()) })
                .map_err(|e| {
                    self.abort();
                    super::WriteError::Flash(format!("esp_ota_write failed: {e}"))
                })?;
            self.written += chunk.len();
            Ok(())
        }

        /// Validates the image and switches the bootloader to the new slot.
        /// The caller should respond to the HTTP client, then reboot.
        pub fn finish(mut self) -> anyhow::Result<usize> {
            sys::esp!(unsafe { sys::esp_ota_end(self.handle) })
                .map_err(|e| anyhow::anyhow!("Firmware image validation failed: {e}"))?;
            sys::esp!(unsafe { sys::esp_ota_set_boot_partition(self.partition) })
                .map_err(|e| anyhow::anyhow!("Failed to select new slot for boot: {e}"))?;
            // The handle was consumed by esp_ota_end; skip abort on drop.
            self.handle = 0;
            Ok(self.written)
        }

        /// Aborts the update, leaving the inactive slot invalid/erased.
        pub fn abort(&mut self) {
            if self.handle != 0 {
                unsafe { sys::esp_ota_abort(self.handle) };
                self.handle = 0;
            }
        }
    }

    impl Drop for OtaWriter {
        fn drop(&mut self) {
            self.abort();
        }
    }

    /// Marks the running OTA slot as valid (cancels a pending rollback).
    /// With `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y` this must be called once
    /// the firmware has proven itself healthy, or the bootloader will roll
    /// back to the previous slot on the next reboot.
    pub fn mark_running_slot_valid() -> Result<(), sys::EspError> {
        sys::esp!(unsafe { sys::esp_ota_mark_app_valid_cancel_rollback() })
    }
}

// --- Host build (cargo test --lib): API-compatible stub -------------------------

#[cfg(not(target_arch = "xtensa"))]
mod imp {
    /// Host stub mirroring the device API for unit tests.
    pub struct OtaWriter {
        written: usize,
    }

    impl OtaWriter {
        #[allow(unused_variables)]
        pub fn begin(expected_size: usize) -> anyhow::Result<Self> {
            if expected_size > super::FIRMWARE_MAX {
                anyhow::bail!(
                    "Firmware ({} bytes) exceeds OTA slot size ({} bytes)",
                    expected_size,
                    super::FIRMWARE_MAX
                );
            }
            Ok(Self { written: 0 })
        }

        pub fn write(&mut self, chunk: &[u8]) -> Result<(), super::WriteError> {
            if self.written + chunk.len() > super::FIRMWARE_MAX {
                return Err(super::WriteError::TooLarge);
            }
            self.written += chunk.len();
            Ok(())
        }

        pub fn finish(self) -> anyhow::Result<usize> {
            Ok(self.written)
        }

        pub fn abort(&mut self) {}
    }

    pub fn mark_running_slot_valid() -> Result<(), ()> {
        Ok(())
    }
}

pub use imp::{mark_running_slot_valid, OtaWriter};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_oversized_begin() {
        let err = match OtaWriter::begin(FIRMWARE_MAX + 1) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("expected begin to reject an oversized image"),
        };
        assert!(err.contains("exceeds"));
    }

    #[test]
    fn rejects_oversized_writes() {
        let mut writer = OtaWriter::begin(FIRMWARE_MAX).unwrap();
        assert!(matches!(writer.write(&[0u8; 100]), Ok(())));
        assert!(matches!(
            writer.write(&[0u8; FIRMWARE_MAX]),
            Err(WriteError::TooLarge)
        ));
    }
}
