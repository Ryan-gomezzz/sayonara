use anyhow::Result;
use rand::RngCore;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom};
use crate::ui::progress::ProgressBar;

pub struct RecoveryTest;

impl RecoveryTest {
    pub fn verify_wipe(device_path: &str, size: u64) -> Result<bool> {
        println!("Starting recovery verification test...");

        let mut bar = ProgressBar::new(48);

        // Sample random sectors
        let test_sectors = Self::generate_test_sectors(size)?;
        let total = test_sectors.len();
        let mut checked = 0usize;

        for sector in test_sectors {
            if !Self::verify_sector_wiped(device_path, sector)? {
                println!("Warning: Recoverable data found at sector {}", sector);
                bar.render(100.0, None, None);
                return Ok(false);
            }
            checked += 1;
            if checked % 50 == 0 || checked == total {
                let progress = (checked as f64 / total as f64) * 50.0; // 0–50%
                bar.render(progress, None, None);
            }
        }

        // Entropy analysis
        let entropy_score = Self::calculate_entropy(device_path, size, &mut bar)?;
        println!("Drive entropy score: {:.2}", entropy_score);

        bar.render(100.0, None, None);

        Ok(entropy_score > 7.5)
    }

    fn generate_test_sectors(size: u64) -> Result<Vec<u64>> {
        let sector_size = 512u64;
        let total_sectors = size / sector_size;
        let mut test_sectors = Vec::new();
        let mut rng = rand::thread_rng();

        // Test 1000 random sectors (or less if not available)
        let tests = std::cmp::min(1000, total_sectors as usize);
        for _ in 0..tests {
            let sector = rng.next_u64() % total_sectors;
            test_sectors.push(sector * sector_size);
        }

        Ok(test_sectors)
    }

    fn verify_sector_wiped(device_path: &str, offset: u64) -> Result<bool> {
        let mut file = OpenOptions::new()
            .read(true)
            .open(device_path)?;

        file.seek(SeekFrom::Start(offset))?;

        let mut buffer = vec![0u8; 4096]; // Read 4KB
        file.read_exact(&mut buffer)?;

        let zero_count = buffer.iter().filter(|&&b| b == 0).count();
        let ff_count = buffer.iter().filter(|&&b| b == 0xFF).count();

        // If more than 80% is the same byte, it's likely properly wiped
        let uniform_threshold = buffer.len() * 8 / 10;

        Ok(zero_count < uniform_threshold && ff_count < uniform_threshold)
    }

    fn calculate_entropy(device_path: &str, size: u64, bar: &mut ProgressBar) -> Result<f64> {
        let mut file = OpenOptions::new()
            .read(true)
            .open(device_path)?;

        let sample_size = std::cmp::min(100 * 1024 * 1024, size);
        let mut buffer = vec![0u8; sample_size as usize];

        file.read_exact(&mut buffer)?;

        let mut counts = [0u64; 256];
        let mut processed = 0usize;
        for (_i, &byte) in buffer.iter().enumerate() {
            counts[byte as usize] += 1;
            processed += 1;
            if processed % (buffer.len() / 50).max(1) == 0 {
                // progress from 50% -> 100%
                let pct = 50.0 + (processed as f64 / buffer.len() as f64) * 50.0;
                // pass bytes processed to show speed/eta relative to sample_size
                bar.render(pct, Some(processed as u64), Some(buffer.len() as u64));
            }
        }

        let length = buffer.len() as f64;
        let mut entropy = 0.0;

        for &count in &counts {
            if count > 0 {
                let probability = count as f64 / length;
                entropy -= probability * probability.log2();
            }
        }

        Ok(entropy)
    }
}
