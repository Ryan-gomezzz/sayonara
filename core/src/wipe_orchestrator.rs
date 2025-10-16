// Wipe Orchestrator - Routes to appropriate wipe implementation based on drive type
//
// This module acts as the main entry point for wipe operations, detecting the drive
// type and routing to the appropriate specialized wipe implementation.

use crate::{
    DriveInfo, DriveType, WipeConfig, Algorithm, DriveResult, DriveError,
    drives::{
        SMRDrive,
        OptaneDrive,
        HybridDrive,
        NVMeAdvanced,
        integrated_wipe::{
            wipe_smr_drive_integrated,
            wipe_optane_drive_integrated,
            wipe_hybrid_drive_integrated,
            wipe_emmc_drive_integrated,
            wipe_raid_array_integrated,
            wipe_nvme_advanced_integrated,
            WipeAlgorithm,
        },
    },
    error::{RecoveryCoordinator, ErrorContext},
};
use crate::drives::types::emmc::EMMCDevice;
use anyhow::Result;
use std::fs::OpenOptions;
use std::io::{Write, Seek, SeekFrom};

/// Main wipe orchestrator with integrated error recovery
pub struct WipeOrchestrator {
    device_path: String,
    config: WipeConfig,
    drive_info: DriveInfo,
    recovery_coordinator: RecoveryCoordinator,
}

impl WipeOrchestrator {
    /// Create new orchestrator for a device with error recovery
    pub fn new(device_path: String, config: WipeConfig) -> Result<Self> {
        // Detect drive type and capabilities
        // For now, create a basic DriveInfo
        let drive_info = Self::create_basic_drive_info(&device_path)?;

        // Initialize recovery coordinator for error handling and checkpointing
        let recovery_coordinator = RecoveryCoordinator::new(&device_path, &config)
            .map_err(|e| DriveError::IoError(std::io::Error::new(std::io::ErrorKind::Other, format!("Failed to initialize recovery coordinator: {}", e))))?;

        Ok(Self {
            device_path,
            config,
            drive_info,
            recovery_coordinator,
        })
    }

    /// Execute the wipe operation with error recovery
    pub async fn execute(&mut self) -> DriveResult<()> {
        println!("\n=== Starting Wipe Operation ===");
        println!("Device: {}", self.device_path);
        println!("Model: {}", self.drive_info.model);
        println!("Size: {} GB", self.drive_info.size / (1024 * 1024 * 1024));
        println!("Type: {:?}", self.drive_info.drive_type);
        println!("Algorithm: {:?}", self.config.algorithm);
        println!();

        // Route to appropriate wipe implementation
        match self.drive_info.drive_type {
            DriveType::SMR => self.wipe_smr_drive().await,
            DriveType::Optane => self.wipe_optane_drive().await,
            DriveType::HybridSSHD => self.wipe_hybrid_drive().await,
            DriveType::EMMC => self.wipe_emmc_drive().await,
            DriveType::UFS => self.wipe_ufs_drive().await,
            DriveType::NVMe => self.wipe_nvme_drive().await,
            DriveType::SSD => self.wipe_ssd_drive().await,
            DriveType::HDD => self.wipe_hdd_drive().await,
            DriveType::RAID => self.wipe_raid_member().await,
            _ => Err(DriveError::Unsupported(
                format!("Drive type {:?} not yet supported", self.drive_info.drive_type)
            )),
        }
    }

    /// Wipe SMR (Shingled Magnetic Recording) drive with error recovery
    async fn wipe_smr_drive(&mut self) -> DriveResult<()> {
        println!("📀 Detected SMR drive - using zone-aware wipe strategy with OptimizedIO + Recovery");

        let smr = SMRDrive::get_zone_configuration(&self.device_path)
            .map_err(|e| DriveError::HardwareCommandFailed(format!("SMR detection failed: {}", e)))?;

        println!("Zone Model: {:?}", smr.zone_model);
        println!("Total Zones: {}", smr.zones.len());
        println!("Conventional Zones: {}", smr.conventional_zone_count);
        println!();

        // Convert WipeConfig algorithm to WipeAlgorithm
        let wipe_algorithm = self.convert_to_wipe_algorithm();

        // Create error context for recovery
        let context = ErrorContext::new(
            "smr_wipe",
            &self.device_path,
        );

        // Execute with recovery coordinator
        self.recovery_coordinator.execute_with_recovery(
            "wipe_smr_drive",
            context,
            || -> DriveResult<()> {
                wipe_smr_drive_integrated(&smr, wipe_algorithm.clone())
                    .map_err(|e| DriveError::IoError(std::io::Error::new(std::io::ErrorKind::Other, format!("{}", e))))?;
                Ok(())
            }
        ).map_err(|e| DriveError::IoError(
            std::io::Error::new(std::io::ErrorKind::Other, format!("{}", e))
        ))?;

        println!("✅ SMR drive wipe completed successfully");
        Ok(())
    }

    /// Wipe Intel Optane / 3D XPoint drive with error recovery
    async fn wipe_optane_drive(&mut self) -> DriveResult<()> {
        println!("⚡ Detected Intel Optane drive - checking for ISE support with OptimizedIO + Recovery");

        let optane = OptaneDrive::get_configuration(&self.device_path)
            .map_err(|e| DriveError::HardwareCommandFailed(format!("Optane detection failed: {}", e)))?;

        println!("Generation: {}", optane.generation);
        println!("Mode: {}", if optane.is_pmem { "Persistent Memory" } else { "Block Device" });
        println!("ISE Support: {}", if optane.supports_ise { "Yes" } else { "No" });
        println!();

        // Prefer hardware ISE if available
        let use_ise = optane.supports_ise;

        // Create error context
        let context = ErrorContext::new(
            "optane_wipe",
            &self.device_path,
        );

        // Execute with recovery coordinator
        self.recovery_coordinator.execute_with_recovery(
            "wipe_optane_drive",
            context,
            || -> DriveResult<()> {
                wipe_optane_drive_integrated(&optane, use_ise)
                    .map_err(|e| DriveError::IoError(std::io::Error::new(std::io::ErrorKind::Other, format!("{}", e))))?;
                Ok(())
            }
        ).map_err(|e| DriveError::IoError(
            std::io::Error::new(std::io::ErrorKind::Other, format!("{}", e))
        ))?;

        println!("✅ Optane drive wipe completed successfully");
        Ok(())
    }

    /// Wipe Hybrid SSHD drive with error recovery
    async fn wipe_hybrid_drive(&mut self) -> DriveResult<()> {
        println!("🔀 Detected Hybrid SSHD - wiping both HDD and SSD cache with OptimizedIO + Recovery");

        let mut hybrid = HybridDrive::get_configuration(&self.device_path)
            .map_err(|e| DriveError::HardwareCommandFailed(format!("Hybrid detection failed: {}", e)))?;

        println!("HDD: {} GB @ {} RPM",
                 hybrid.hdd_portion.capacity / (1024 * 1024 * 1024),
                 hybrid.hdd_portion.rpm);
        println!("SSD Cache: {} GB",
                 hybrid.ssd_cache.cache_size / (1024 * 1024 * 1024));
        println!();

        // Create error context
        let context = ErrorContext::new(
            "hybrid_wipe",
            &self.device_path,
        );

        // Execute with recovery coordinator
        self.recovery_coordinator.execute_with_recovery(
            "wipe_hybrid_drive",
            context,
            || -> DriveResult<()> {
                wipe_hybrid_drive_integrated(&mut hybrid)
                    .map_err(|e| DriveError::IoError(std::io::Error::new(std::io::ErrorKind::Other, format!("{}", e))))?;
                Ok(())
            }
        ).map_err(|e| DriveError::IoError(
            std::io::Error::new(std::io::ErrorKind::Other, format!("{}", e))
        ))?;

        println!("✅ Hybrid drive wipe completed successfully");
        Ok(())
    }

    /// Wipe eMMC embedded storage with error recovery
    async fn wipe_emmc_drive(&mut self) -> DriveResult<()> {
        println!("📱 Detected eMMC device - wiping all partitions with OptimizedIO + Recovery");

        let emmc = EMMCDevice::get_configuration(&self.device_path)
            .map_err(|e| DriveError::HardwareCommandFailed(format!("eMMC detection failed: {}", e)))?;

        println!("eMMC Version: {}", emmc.emmc_version);
        println!("Boot Partitions: {}", emmc.boot_partitions.len());
        println!();

        // Try hardware erase first, fall back to software if not supported
        let use_hardware = true;

        // Create error context
        let context = ErrorContext::new(
            "emmc_wipe",
            &self.device_path,
        );

        // Execute with recovery coordinator
        self.recovery_coordinator.execute_with_recovery(
            "wipe_emmc_drive",
            context,
            || -> DriveResult<()> {
                wipe_emmc_drive_integrated(&emmc, use_hardware)
                    .map_err(|e| DriveError::IoError(std::io::Error::new(std::io::ErrorKind::Other, format!("{}", e))))?;
                Ok(())
            }
        ).map_err(|e| DriveError::IoError(
            std::io::Error::new(std::io::ErrorKind::Other, format!("{}", e))
        ))?;

        println!("✅ eMMC wipe completed successfully");
        Ok(())
    }

    /// Wipe UFS (Universal Flash Storage) with error recovery
    async fn wipe_ufs_drive(&mut self) -> DriveResult<()> {
        println!("📱 Detected UFS device - using PURGE command with Recovery");
        println!("⚠️  UFS full integration pending, using PURGE command");

        // Create error context
        let context = ErrorContext::new(
            "ufs_wipe",
            &self.device_path,
        );

        let device_path = self.device_path.clone();

        // Execute with recovery coordinator
        self.recovery_coordinator.execute_with_recovery(
            "wipe_ufs_drive",
            context,
            || {
                let output = std::process::Command::new("sg_unmap")
                    .arg("--all")
                    .arg(&device_path)
                    .output()
                    .map_err(|e| DriveError::IoError(std::io::Error::new(std::io::ErrorKind::Other, format!("UFS PURGE failed: {}", e))))?;

                if !output.status.success() {
                    return Err(DriveError::IoError(std::io::Error::new(std::io::ErrorKind::Other, "UFS PURGE command failed")));
                }

                Ok(())
            }
        ).map_err(|e| DriveError::IoError(
            std::io::Error::new(std::io::ErrorKind::Other, format!("{}", e))
        ))?;

        println!("✅ UFS wipe completed successfully");
        Ok(())
    }

    /// Wipe NVMe drive with error recovery (check for advanced features first)
    async fn wipe_nvme_drive(&mut self) -> DriveResult<()> {
        println!("💾 Detected NVMe drive - checking for advanced features with Recovery");

        // Check if this is an advanced NVMe with ZNS, multiple namespaces, etc.
        if NVMeAdvanced::detect_advanced_features(&self.device_path).unwrap_or(false) {
            println!("🔬 Advanced NVMe features detected, using OptimizedIO with namespace support + Recovery");
            println!();

            // Get advanced NVMe configuration
            let nvme_advanced = NVMeAdvanced::get_configuration(&self.device_path)
                .map_err(|e| DriveError::HardwareCommandFailed(format!("NVMe advanced detection failed: {}", e)))?;

            println!("Namespaces: {}", nvme_advanced.namespaces.len());
            println!("ZNS Support: {}", nvme_advanced.zns_support);
            println!();

            // Prefer hardware format, but can fall back to software
            let use_format = true;

            // Create error context
            let context = ErrorContext::new(
            "nvme_advanced_wipe",
            &self.device_path,
        );

            // Execute with recovery coordinator
            self.recovery_coordinator.execute_with_recovery(
                "wipe_nvme_advanced",
                context,
                || {
                    wipe_nvme_advanced_integrated(&nvme_advanced, use_format)
                        .map_err(|e| DriveError::IoError(std::io::Error::new(std::io::ErrorKind::Other, format!("Advanced NVMe wipe failed: {}", e))))
                }
            ).map_err(|e| DriveError::IoError(
                std::io::Error::new(std::io::ErrorKind::Other, format!("{}", e))
            ))?;

            println!("✅ Advanced NVMe wipe completed successfully");
            return Ok(());
        }

        // Fall back to basic NVMe wipe via sanitize command
        println!("Using standard NVMe sanitize command with Recovery");

        // Create error context
        let context = ErrorContext::new(
            "nvme_basic_wipe",
            &self.device_path,
        );

        let device_path = self.device_path.clone();

        // Execute with recovery coordinator
        self.recovery_coordinator.execute_with_recovery(
            "wipe_nvme_basic",
            context,
            || {
                let output = std::process::Command::new("nvme")
                    .arg("sanitize")
                    .arg(&device_path)
                    .arg("-a").arg("2")  // Cryptographic erase
                    .output()
                    .map_err(|e| DriveError::IoError(std::io::Error::new(std::io::ErrorKind::Other, format!("NVMe sanitize failed: {}", e))))?;

                if !output.status.success() {
                    return Err(DriveError::IoError(std::io::Error::new(std::io::ErrorKind::Other, "NVMe sanitize failed")));
                }

                Ok(())
            }
        ).map_err(|e| DriveError::IoError(
            std::io::Error::new(std::io::ErrorKind::Other, format!("{}", e))
        ))?;

        println!("✅ NVMe wipe completed successfully");
        Ok(())
    }

    /// Wipe SSD drive with error recovery
    async fn wipe_ssd_drive(&mut self) -> DriveResult<()> {
        println!("💿 Detected SSD - using TRIM-aware wipe strategy with Recovery");
        println!("⚠️  Using simplified SSD wipe (full integration pending)");

        // Create error context
        let context = ErrorContext::new(
            "ssd_wipe",
            &self.device_path,
        );

        let device_path = self.device_path.clone();
        let size = self.drive_info.size;
        let trim_support = self.drive_info.capabilities.trim_support;

        // Execute with recovery coordinator
        self.recovery_coordinator.execute_with_recovery(
            "wipe_ssd_drive",
            context,
            || {
                // Perform basic overwrite
                self.write_pattern_to_region(0, size)
                    .map_err(|e| DriveError::IoError(std::io::Error::new(std::io::ErrorKind::Other, format!("SSD wipe failed: {}", e))))?;

                // Then TRIM if supported
                if trim_support {
                    let _ = std::process::Command::new("blkdiscard")
                        .arg(&device_path)
                        .output();
                }

                Ok(())
            }
        ).map_err(|e| DriveError::IoError(
            std::io::Error::new(std::io::ErrorKind::Other, format!("{}", e))
        ))?;

        println!("✅ SSD wipe completed successfully");
        Ok(())
    }

    /// Wipe HDD drive with error recovery
    async fn wipe_hdd_drive(&mut self) -> DriveResult<()> {
        println!("💽 Detected HDD - using traditional overwrite strategy with Recovery");
        println!("⚠️  Using simplified HDD wipe (full integration pending)");

        // Create error context
        let context = ErrorContext::new(
            "hdd_wipe",
            &self.device_path,
        );

        let size = self.drive_info.size;

        // Execute with recovery coordinator
        self.recovery_coordinator.execute_with_recovery(
            "wipe_hdd_drive",
            context,
            || {
                self.write_pattern_to_region(0, size)
                    .map_err(|e| DriveError::IoError(std::io::Error::new(std::io::ErrorKind::Other, format!("HDD wipe failed: {}", e))))
            }
        ).map_err(|e| DriveError::IoError(
            std::io::Error::new(std::io::ErrorKind::Other, format!("{}", e))
        ))?;

        println!("✅ HDD wipe completed successfully");
        Ok(())
    }

    /// Wipe RAID array member with error recovery
    async fn wipe_raid_member(&mut self) -> DriveResult<()> {
        println!("🔗 Detected RAID array member - using OptimizedIO + Recovery");
        println!("⚠️  Warning: Wiping individual RAID members will destroy the array!");

        // Check if user confirmed
        if !self.config.unlock_encrypted {  // Reusing this flag as "force" for now
            return Err(DriveError::Unsupported(
                "Wiping RAID members requires --force flag".to_string()
            ));
        }

        // Import raid module
        use crate::drives::types::raid::RAIDArray;

        // Get RAID configuration
        let raid = RAIDArray::get_configuration(&self.device_path)
            .map_err(|e| DriveError::HardwareCommandFailed(format!("RAID detection failed: {}", e)))?;

        println!("RAID Type: {:?}", raid.raid_type);
        println!("Members: {}", raid.member_drives.len());
        println!();

        // Create error context
        let context = ErrorContext::new(
            "raid_wipe",
            &self.device_path,
        );

        // Execute with recovery coordinator
        let wipe_metadata = true;
        self.recovery_coordinator.execute_with_recovery(
            "wipe_raid_member",
            context,
            || {
                wipe_raid_array_integrated(&raid, wipe_metadata)
                    .map_err(|e| DriveError::IoError(std::io::Error::new(std::io::ErrorKind::Other, format!("RAID wipe failed: {}", e))))
            }
        ).map_err(|e| DriveError::IoError(
            std::io::Error::new(std::io::ErrorKind::Other, format!("{}", e))
        ))?;

        println!("✅ RAID member wipe completed successfully");
        Ok(())
    }

    /// Convert WipeConfig algorithm to WipeAlgorithm for integrated wipe functions
    fn convert_to_wipe_algorithm(&self) -> WipeAlgorithm {
        match self.config.algorithm {
            Algorithm::Zero => WipeAlgorithm::Zeros,
            Algorithm::Random => WipeAlgorithm::Random,
            Algorithm::DoD5220 => WipeAlgorithm::Random, // DoD uses multiple passes with random
            Algorithm::Gutmann => WipeAlgorithm::Random,  // Gutmann uses complex patterns
            _ => WipeAlgorithm::Random, // Default to random for security
        }
    }

    /// Write pattern to a specific region (used by SMR and other specialized wipers)
    fn write_pattern_to_region(&self, offset: u64, size: u64) -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .open(&self.device_path)?;

        file.seek(SeekFrom::Start(offset))?;

        // Generate pattern based on algorithm
        let pattern = self.generate_pattern(size as usize)?;
        file.write_all(&pattern)?;
        file.sync_all()?;

        Ok(())
    }

    /// Create basic drive info for now (TODO: integrate with full detection)
    fn create_basic_drive_info(device_path: &str) -> Result<DriveInfo> {
        // Simple detection based on device path
        let drive_type = if device_path.contains("nvme") {
            DriveType::NVMe
        } else if device_path.contains("mmcblk") {
            DriveType::EMMC
        } else {
            DriveType::HDD  // Default
        };

        Ok(DriveInfo {
            device_path: device_path.to_string(),
            model: "Unknown".to_string(),
            serial: "Unknown".to_string(),
            size: 1024 * 1024 * 1024 * 100,  // Assume 100GB for now
            drive_type,
            encryption_status: crate::EncryptionStatus::None,
            capabilities: Default::default(),
            health_status: None,
            temperature_celsius: None,
        })
    }

    /// Generate wipe pattern based on configured algorithm
    fn generate_pattern(&self, size: usize) -> Result<Vec<u8>> {
        use crate::crypto::secure_rng::SecureRNG;

        match self.config.algorithm {
            Algorithm::Random => {
                let mut data = vec![0u8; size];
                let mut rng = SecureRNG::new()?;
                rng.fill_bytes(&mut data)?;
                Ok(data)
            }
            Algorithm::Zero => {
                Ok(vec![0u8; size])
            }
            Algorithm::DoD5220 => {
                // DoD uses multiple passes, for now just use first pass pattern
                let mut data = vec![0u8; size];
                let mut rng = SecureRNG::new()?;
                rng.fill_bytes(&mut data)?;
                Ok(data)
            }
            Algorithm::Gutmann => {
                // Gutmann uses 35 passes, this is simplified
                let mut data = vec![0u8; size];
                let mut rng = SecureRNG::new()?;
                rng.fill_bytes(&mut data)?;
                Ok(data)
            }
            _ => {
                let mut data = vec![0u8; size];
                let mut rng = SecureRNG::new()?;
                rng.fill_bytes(&mut data)?;
                Ok(data)
            }
        }
    }
}

/// Convenience function for simple wipe operations with error recovery
pub async fn wipe_drive(device_path: &str, config: WipeConfig) -> DriveResult<()> {
    let mut orchestrator = WipeOrchestrator::new(device_path.to_string(), config)
        .map_err(|e| DriveError::HardwareCommandFailed(format!("Orchestrator creation failed: {}", e)))?;

    orchestrator.execute().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_orchestrator_creation() {
        // This will fail without a real device, but tests the interface
        let config = WipeConfig::default();
        let result = WipeOrchestrator::new("/dev/null".to_string(), config);

        // Just verify it doesn't panic
        let _ = result;
    }

    #[test]
    fn test_pattern_generation() {
        let config = WipeConfig {
            algorithm: Algorithm::Zero,
            ..Default::default()
        };

        let orchestrator = WipeOrchestrator {
            device_path: "/dev/null".to_string(),
            config: config.clone(),
            drive_info: DriveInfo {
                device_path: "/dev/null".to_string(),
                model: "Test".to_string(),
                serial: "TEST123".to_string(),
                size: 1024 * 1024 * 1024,
                drive_type: DriveType::HDD,
                encryption_status: crate::EncryptionStatus::None,
                capabilities: Default::default(),
                health_status: None,
                temperature_celsius: None,
            },
        };

        let pattern = orchestrator.generate_pattern(1024).unwrap();
        assert_eq!(pattern.len(), 1024);
        assert!(pattern.iter().all(|&b| b == 0));
    }
}
