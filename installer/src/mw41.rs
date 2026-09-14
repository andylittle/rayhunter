//! Installer for the Alcatel LinkZone MW41MP.
//!
//! Installation process:
//! 1. Find the device's USB mass-storage block device and send a vendor SCSI command that
//!    switches it into a debug mode where it starts `adbd` as root. This is a documented
//!    vendor feature (the same command TCL's own factory service tool, TCL-SWITCH-TOOL, sends),
//!    not an exploit, and does not persist across a reboot.
//! 2. Wait for ADB to become available.
//! 3. Push the daemon + config to /cache/rayhunter, the only writable partition on this device
//!    with meaningful free space (there is no separate /data partition).
//! 4. Point QMDL recordings at the microSD card (auto-mounted at /media/card), since /cache
//!    alone is far too small to hold more than a few hours of recording.
//! 5. Install the init script and reboot; the device starts rayhunter automatically from then
//!    on, the same as the other supported devices.

use std::time::Duration;

use adb_client::{ADBDeviceExt, ADBUSBDevice, RustADBError};
use anyhow::{Context, Result, bail};
use tokio::time::sleep;

use crate::Mw41Args as Args;
use crate::RAYHUNTER_DAEMON_INIT;
use crate::output::{eprintln, print, println};

const MW41_VENDOR_ID: u16 = 0x1bbb;
const MW41_NORMAL_PRODUCT_ID: u16 = 0x0195;
const MW41_DEBUG_PRODUCT_ID: u16 = 0x0196;

/// The SCSI CDB that switches the device into debug mode. Publicly documented since 2021
/// (Alex Studer / jtanx/LinkZoneRoot); the same command TCL's own factory tool sends.
const DEBUG_MODE_CDB: [u8; 16] = [0x16, 0xf9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

pub async fn install(
    Args {
        reset_config,
        skip_sdcard,
    }: Args,
) -> Result<()> {
    print!("Looking for the device... ");
    activate_debug_mode()?;
    println!("ok");

    print!("Waiting for ADB connection... ");
    let mut adb_device = wait_for_adb().await?;
    println!("ok");

    print!("Checking for a microSD card... ");
    let qmdl_store_path = check_sdcard(&mut adb_device, skip_sdcard)?;
    println!("ok, using {qmdl_store_path}");

    print!("Installing rayhunter files... ");
    install_rayhunter_files(&mut adb_device, reset_config, &qmdl_store_path)?;
    println!("ok");

    print!("Installing startup script... ");
    install_startup_script(&mut adb_device)?;
    println!("ok");

    println!("Installation complete. Rebooting device...");
    let _ = adb_device.reboot(adb_client::RebootType::System);

    println!(
        "Device is rebooting. Once it's back up, check out the web interface at http://192.168.0.1:8080"
    );

    Ok(())
}

/// Send the debug-mode SCSI command if the device isn't already in debug mode.
#[cfg(target_os = "linux")]
fn activate_debug_mode() -> Result<()> {
    match find_mw41_usb_device()? {
        Mw41UsbState::AlreadyInDebugMode => Ok(()),
        Mw41UsbState::NormalMode(block_device) => {
            use scsir::Scsi;

            let scsi = Scsi::new(&block_device).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to open {} for the SCSI command: {e}",
                    block_device.display()
                )
            })?;
            scsi.issue(&DebugModeSwitchCommand)
                .map_err(|e| anyhow::anyhow!("Failed to send the debug-mode SCSI command: {e}"))?;
            // The device takes a couple of seconds to re-enumerate as adbd starts.
            std::thread::sleep(Duration::from_secs(2));
            Ok(())
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn activate_debug_mode() -> Result<()> {
    bail!(
        "Automated MW41MP installation is currently only supported on Linux. On other \
         platforms, send the debug-mode SCSI command manually (see doc/mw41.md), then re-run \
         this installer once `adb devices` shows the device."
    );
}

#[cfg(target_os = "linux")]
enum Mw41UsbState {
    /// Not yet switched. Holds the path to the mass-storage block device (e.g. /dev/sdb).
    NormalMode(std::path::PathBuf),
    AlreadyInDebugMode,
}

/// Scan connected USB mass-storage block devices for the MW41MP, identified by its USB
/// vendor/product ID. Returns an error if none or more than one candidate is found -- in the
/// latter case the user likely has another Alcatel/TCL device plugged in, since this exact
/// VID:PID pair isn't unique to the MW41MP.
#[cfg(target_os = "linux")]
fn find_mw41_usb_device() -> Result<Mw41UsbState> {
    use std::fs;

    let mut already_in_debug_mode = false;
    let mut candidates = Vec::new();

    for entry in fs::read_dir("/sys/block").context("Failed to read /sys/block")? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        // Only consider things that look like SCSI/USB disks, to avoid e.g. loop devices.
        if !name.starts_with("sd") {
            continue;
        }

        let device_link = entry.path().join("device");
        let Ok(device_path) = fs::canonicalize(&device_link) else {
            continue;
        };

        let Some((vendor_id, product_id)) = find_usb_ids(&device_path) else {
            continue;
        };

        if vendor_id != MW41_VENDOR_ID {
            continue;
        }

        if product_id == MW41_NORMAL_PRODUCT_ID {
            candidates.push(std::path::PathBuf::from("/dev").join(name));
        } else if product_id == MW41_DEBUG_PRODUCT_ID {
            already_in_debug_mode = true;
        }
    }

    if already_in_debug_mode {
        return Ok(Mw41UsbState::AlreadyInDebugMode);
    }

    match candidates.len() {
        0 => bail!(
            "No MW41MP found on USB. Make sure it's plugged in via USB and fully booted \
             (WiFi network visible)."
        ),
        1 => Ok(Mw41UsbState::NormalMode(candidates.remove(0))),
        _ => bail!(
            "Found more than one matching USB device ({candidates:?}). The MW41MP's USB ID \
             isn't unique to it -- please unplug any other Alcatel/TCL USB devices and try again."
        ),
    }
}

/// Walk up from a USB device's sysfs path to find the nearest ancestor exposing
/// `idVendor`/`idProduct` (the actual USB device node, as opposed to one of its interfaces).
#[cfg(target_os = "linux")]
fn find_usb_ids(device_path: &std::path::Path) -> Option<(u16, u16)> {
    for ancestor in device_path.ancestors() {
        let vendor = std::fs::read_to_string(ancestor.join("idVendor")).ok();
        let product = std::fs::read_to_string(ancestor.join("idProduct")).ok();
        if let (Some(vendor), Some(product)) = (vendor, product) {
            let vendor_id = u16::from_str_radix(vendor.trim(), 16).ok()?;
            let product_id = u16::from_str_radix(product.trim(), 16).ok()?;
            return Some((vendor_id, product_id));
        }
    }
    None
}

#[cfg(target_os = "linux")]
struct DebugModeSwitchCommand;

#[cfg(target_os = "linux")]
impl scsir::Command for DebugModeSwitchCommand {
    type CommandBuffer = [u8; 16];
    type DataBuffer = ();
    type DataBufferWrapper = ();
    type ReturnType = scsir::Result<()>;

    fn direction(&self) -> scsir::DataDirection {
        scsir::DataDirection::None
    }

    fn command(&self) -> Self::CommandBuffer {
        DEBUG_MODE_CDB
    }

    fn data(&self) -> Self::DataBufferWrapper {}

    fn data_size(&self) -> u32 {
        0
    }

    fn process_result(
        &self,
        result: scsir::ResultData<Self::DataBufferWrapper>,
    ) -> Self::ReturnType {
        result.check_ioctl_error()?;
        result.check_common_error()?;
        Ok(())
    }
}

async fn wait_for_adb() -> Result<ADBUSBDevice> {
    const MAX_ATTEMPTS: u32 = 30;
    let mut attempts = 0;

    loop {
        if attempts >= MAX_ATTEMPTS {
            bail!("Timeout waiting for ADB connection after switching to debug mode");
        }

        match ADBUSBDevice::new(MW41_VENDOR_ID, MW41_DEBUG_PRODUCT_ID) {
            Ok(mut device) => {
                let mut buf = Vec::<u8>::new();
                if device.shell_command(&["echo", "test"], &mut buf).is_ok()
                    && String::from_utf8_lossy(&buf).contains("test")
                {
                    return Ok(device);
                }
            }
            Err(RustADBError::DeviceNotFound(_)) => {}
            Err(e) => bail!("ADB connection error: {e}"),
        }

        sleep(Duration::from_secs(1)).await;
        attempts += 1;
    }
}

/// Confirm the microSD card is mounted, returning the QMDL store path to use. By default,
/// bails if no card is present -- /cache alone (~25MB free) fills up within hours of
/// recording. Pass `skip_sdcard` to store recordings there anyway.
fn check_sdcard(adb_device: &mut ADBUSBDevice, skip_sdcard: bool) -> Result<String> {
    let mut buf = Vec::<u8>::new();
    let has_sdcard = adb_device
        .shell_command(&["mount"], &mut buf)
        .map(|_| String::from_utf8_lossy(&buf).contains("/media/card"))
        .unwrap_or(false);

    if has_sdcard {
        return Ok("/media/card/rayhunter/qmdl".to_string());
    }

    if skip_sdcard {
        eprintln!(
            "warning: no microSD card detected. Storing recordings in /cache instead, which \
             only has a few tens of MB free -- rayhunter will stop recording within hours."
        );
        return Ok("/cache/rayhunter/qmdl".to_string());
    }

    bail!(
        "No microSD card detected at /media/card. Rayhunter needs one to store more than a few \
         hours of recordings on this device (the internal /cache partition is far too small). \
         Insert a FAT32-formatted microSD card and try again, or pass --skip-sdcard to store \
         recordings in /cache anyway (not recommended)."
    );
}

/// Run a shell command and report whether it exited successfully, by checking its output
/// rather than `shell_command`'s return value -- `adb_client` doesn't propagate the remote
/// command's exit code, so `Result::is_ok()` alone can't tell success from failure.
fn shell_test(adb_device: &mut ADBUSBDevice, args: &[&str]) -> Result<bool> {
    let mut cmd: Vec<&str> = args.to_vec();
    cmd.extend(["&&", "echo", "RAYHUNTER_OK"]);
    let mut buf = Vec::<u8>::new();
    adb_device.shell_command(&cmd, &mut buf)?;
    Ok(String::from_utf8_lossy(&buf).contains("RAYHUNTER_OK"))
}

fn install_rayhunter_files(
    adb_device: &mut ADBUSBDevice,
    reset_config: bool,
    qmdl_store_path: &str,
) -> Result<()> {
    let mut buf = Vec::<u8>::new();
    let qmdl_dir = qmdl_store_path
        .rsplit_once('/')
        .map(|(dir, _)| dir)
        .unwrap_or(qmdl_store_path);
    adb_device.shell_command(&["mkdir", "-p", "/cache/rayhunter", qmdl_dir], &mut buf)?;

    let config_path = "/cache/rayhunter/config.toml";
    let config_exists = shell_test(adb_device, &["test", "-f", config_path])?;

    if reset_config || !config_exists {
        let config_content = crate::CONFIG_TOML
            .replace(r#"#device = "orbic""#, r#"device = "mw41""#)
            .replace(
                r#"qmdl_store_path = "/data/rayhunter/qmdl""#,
                &format!(r#"qmdl_store_path = "{qmdl_store_path}""#),
            );
        let mut config_data = config_content.as_bytes();
        adb_device.push(&mut config_data, &config_path)?;
    } else {
        println!("config.toml already exists, skipping (use --reset-config to overwrite)");
    }

    let rayhunter_daemon_bin = crate::get_file!("FILE_RAYHUNTER_DAEMON");
    let mut daemon_data = rayhunter_daemon_bin;
    adb_device.push(&mut daemon_data, &"/cache/rayhunter/rayhunter-daemon")?;
    adb_device.shell_command(
        &["chmod", "755", "/cache/rayhunter/rayhunter-daemon"],
        &mut buf,
    )?;

    Ok(())
}

/// No #RAYHUNTER-PRESTART commands are needed on this device: /dev/diag is already shared
/// concurrently by several stock processes, and the microSD card auto-mounts before this
/// script would run.
///
/// Unlike other devices, we can't use a /data/rayhunter symlink to redirect the template's
/// hardcoded paths: on this device, /data gets reset to its pristine (empty) state on every
/// boot by some part of the vendor boot process, so anything placed under it -- including a
/// symlink -- doesn't survive a reboot. Patch the paths directly instead.
fn get_rayhunter_daemon_init() -> String {
    RAYHUNTER_DAEMON_INIT
        .replace("#RAYHUNTER-PRESTART", "")
        .replace("/data/rayhunter", "/cache/rayhunter")
}

fn install_startup_script(adb_device: &mut ADBUSBDevice) -> Result<()> {
    let init_script = get_rayhunter_daemon_init();
    let mut script_data = init_script.as_bytes();
    adb_device.push(&mut script_data, &"/etc/init.d/rayhunter_daemon")?;

    let mut buf = Vec::<u8>::new();
    adb_device.shell_command(&["chmod", "755", "/etc/init.d/rayhunter_daemon"], &mut buf)?;
    adb_device.shell_command(
        &[
            "ln",
            "-sf",
            "../init.d/rayhunter_daemon",
            "/etc/rc5.d/S99rayhunter_daemon",
        ],
        &mut buf,
    )?;

    Ok(())
}

/// Switch the device into debug mode and open an interactive ADB shell.
pub async fn shell() -> Result<()> {
    activate_debug_mode()?;
    let mut adb_device = wait_for_adb().await?;
    adb_device.shell(&mut std::io::stdin(), Box::new(std::io::stdout()))?;
    Ok(())
}

/// Just switch the device into debug mode, without doing anything else.
pub fn start_adb() -> Result<()> {
    activate_debug_mode()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_rayhunter_daemon_init() {
        let s = get_rayhunter_daemon_init();
        assert!(s.contains("/cache/rayhunter/rayhunter-daemon"));
        assert!(s.contains("/cache/rayhunter/config.toml"));
        assert!(!s.contains("/data/rayhunter"));
        assert!(!s.contains("#RAYHUNTER-PRESTART"));
    }
}
