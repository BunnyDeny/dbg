//! 探针(烧录器)检测: 从 Linux sysfs 认出插着的是 ST-Link 还是 J-Link。

use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

/// sysfs 里 USB 设备所在的目录(每个设备一个子目录)。
const USB_DEVICES: &str = "/sys/bus/usb/devices";

/// 支持的探针种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeKind {
    StLink,
    Jlink,
}

impl ProbeKind {
    /// 该探针对应的 OpenOCD interface 配置文件。
    pub fn interface_config(self) -> &'static str {
        match self {
            ProbeKind::StLink => "interface/stlink.cfg",
            ProbeKind::Jlink => "interface/jlink.cfg",
        }
    }
}

impl fmt::Display for ProbeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ProbeKind::StLink => "ST-Link",
            ProbeKind::Jlink => "J-Link",
        })
    }
}

/// 一个检测到的探针。
#[derive(Debug)]
pub struct Probe {
    pub kind: ProbeKind,
    pub vid: u16,
    pub pid: u16,
    /// 序列号(sysfs 里可能没有这个属性)。
    pub serial: Option<String>,
}

impl fmt::Display for Probe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.serial {
            Some(serial) => write!(f, "{} (SN: {serial})", self.kind),
            None => write!(f, "{} (无序列号)", self.kind),
        }
    }
}

/// 探针检测的错误。
/// thiserror 会依据下面的属性自动生成 Display 与 std::error::Error,
/// `#[from]` 还会顺带生成 From<io::Error> 并把 io 错误挂进错误链(source)。
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    /// 没插任何受支持的探针
    #[error("没插任何受支持的探针(ST-Link / J-Link)")]
    NotFound,
    /// 读取系统 USB 信息失败
    #[error("读取系统 USB 信息失败")]
    Io(#[from] io::Error),
}

/// 检测当前插着的探针。
/// 插了多个时返回枚举到的第一个(按序列号挑选留待以后)。
pub fn detect_probe() -> Result<Probe, ProbeError> {
    for entry in fs::read_dir(USB_DEVICES)? {
        let dir = entry?.path();
        if !is_device_dir(&dir) {
            continue;
        }
        let (Some(vid), Some(pid)) = (read_hex(&dir, "idVendor"), read_hex(&dir, "idProduct"))
        else {
            continue;
        };
        if let Some(kind) = identify(vid, pid) {
            return Ok(Probe {
                kind,
                vid,
                pid,
                serial: read_text(&dir, "serial"),
            });
        }
    }
    Err(ProbeError::NotFound)
}

/// 只认设备目录: 跳过根 hub(`usb1`)和接口目录(`1-11:1.0`, 名字里带冒号)。
fn is_device_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| !name.starts_with("usb") && !name.contains(':'))
}

/// 读一个 sysfs 文本属性并去掉行尾空白。
fn read_text(dir: &Path, name: &str) -> Option<String> {
    fs::read_to_string(dir.join(name))
        .ok()
        .map(|text| text.trim().to_string())
}

/// 读一个十六进制属性(如 idVendor = "0483")。
fn read_hex(dir: &Path, name: &str) -> Option<u16> {
    u16::from_str_radix(&read_text(dir, name)?, 16).ok()
}

/// VID/PID → 探针种类。数值来自系统 usb.ids。
/// ST 的 0x0483 下还有大量非调试产品, 必须按 PID 白名单;
/// SEGGER 的 0x1366 下基本都是调试器, 按 VID 识别即可。
fn identify(vid: u16, pid: u16) -> Option<ProbeKind> {
    match (vid, pid) {
        (0x0483, 0x3744 | 0x3748 | 0x374b | 0x374d | 0x374e | 0x374f | 0x3752 | 0x3753) => {
            Some(ProbeKind::StLink)
        }
        (0x1366, _) => Some(ProbeKind::Jlink),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_known_probes() {
        assert_eq!(identify(0x0483, 0x3748), Some(ProbeKind::StLink)); // ST-LINK/V2
        assert_eq!(identify(0x0483, 0x374b), Some(ProbeKind::StLink)); // ST-LINK/V2.1
        assert_eq!(identify(0x0483, 0x374e), Some(ProbeKind::StLink)); // STLINK-V3
        assert_eq!(identify(0x1366, 0x0101), Some(ProbeKind::Jlink)); // J-Link
    }

    #[test]
    fn ignores_non_probe_devices() {
        assert_eq!(identify(0x0483, 0x5740), None); // ST 虚拟串口, 不是调试器
        assert_eq!(identify(0x1d6b, 0x0002), None); // Linux 根 hub
    }

    #[test]
    fn maps_kind_to_interface_config() {
        assert_eq!(ProbeKind::StLink.interface_config(), "interface/stlink.cfg");
        assert_eq!(ProbeKind::Jlink.interface_config(), "interface/jlink.cfg");
    }

    #[test]
    fn prints_human_readable_probe() {
        let probe = Probe {
            kind: ProbeKind::StLink,
            vid: 0x0483,
            pid: 0x3748,
            serial: Some("066FFF".to_string()),
        };
        assert_eq!(probe.to_string(), "ST-Link (SN: 066FFF)");

        let no_serial = Probe {
            serial: None,
            ..probe
        };
        assert_eq!(no_serial.to_string(), "ST-Link (无序列号)");
    }

    /// 真机测试: 直接跑 detect_probe() 扫真实的 /sys/bus/usb/devices。
    /// 用 `cargo test detect_probe -- --nocapture` 看它认出了什么。
    /// 不断言"必须插着探针"(否则没插硬件的机器上会失败), 只要求结果自洽。
    #[test]
    fn detect_probe_on_real_sysfs() {
        match detect_probe() {
            Ok(probe) => {
                println!("检测到探针: {probe} -> -f {}", probe.kind.interface_config());
                assert!(probe.kind.interface_config().starts_with("interface/"));
                assert_ne!(probe.vid, 0);
            }
            Err(e) => println!("未检测到探针: {e}"),
        }
    }
}
