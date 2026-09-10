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
#[derive(Debug, Clone, PartialEq)]
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

/// 探针相关操作的错误。
/// "没插探针"不是错误, 而是 [`ProbeState::Disconnected`]; 这里只留真正的故障。
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    /// 读取系统 USB 信息失败
    #[error("读取系统 USB 信息失败")]
    Sysfs(#[from] io::Error),
}

/// 探针状态。
#[derive(Debug, Clone, PartialEq)]
pub enum ProbeState {
    /// 插着探针
    Connected(Probe),
    /// 没插探针(正常状态, 不是错误)
    Disconnected,
}

impl fmt::Display for ProbeState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProbeState::Connected(probe) => write!(f, "已连接: {probe}"),
            ProbeState::Disconnected => f.write_str("未连接探针"),
        }
    }
}

/// 监听探针插拔。
///
/// 做法很朴素: **每次调用 [`ProbeWatcher::try_next`] 就扫一次 sysfs**,
/// 和上次记录的状态比较, 变了就返回新状态。没有后台线程、没有额外依赖,
/// 也不需要"关闭"——没有任何东西在后台跑。
///
/// 调用方(通常是界面刷新循环)自己决定检查频率, 例如每 100ms 一次。
pub struct ProbeWatcher {
    /// 上次看到的状态, 用来判断"变了没有"
    last: ProbeState,
}

impl ProbeWatcher {
    /// 开始监听: 记录当前状态作为基线。
    pub fn start() -> Result<Self, ProbeError> {
        Ok(Self {
            last: detect_probe()?,
        })
    }

    /// 非阻塞检查一次: 状态有变化返回 `Some(新状态)`, 没变化返回 `None`。
    /// 扫描出错(读不了 sysfs)时保持上次状态, 也返回 `None`。
    pub fn try_next(&mut self) -> Option<ProbeState> {
        let now = detect_probe().ok()?;
        if now == self.last {
            return None;
        }
        self.last = now.clone();
        Some(now)
    }
}

/// 扫描一次 sysfs, 得知探针当前状态。
/// 插了多个时取枚举到的第一个(按序列号挑选留待以后)。
pub fn detect_probe() -> Result<ProbeState, ProbeError> {
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
            return Ok(ProbeState::Connected(Probe {
                kind,
                vid,
                pid,
                serial: read_text(&dir, "serial"),
            }));
        }
    }
    Ok(ProbeState::Disconnected)
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
    use std::thread;
    use std::time::{Duration, Instant};

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
    /// 用 `cargo test detect_probe -- --show-output` 看它认出了什么。
    /// 不断言"必须插着探针"(否则没插硬件的机器上会失败), 只要求结果自洽。
    #[test]
    fn detect_probe_on_real_sysfs() {
        match detect_probe().expect("扫描 sysfs 不应失败") {
            ProbeState::Connected(probe) => {
                println!(
                    "检测到探针: {probe} -> -f {}",
                    probe.kind.interface_config()
                );
                assert!(probe.kind.interface_config().starts_with("interface/"));
                assert_ne!(probe.vid, 0);
            }
            ProbeState::Disconnected => println!("未检测到探针: {}", ProbeState::Disconnected),
        }
    }

    /// 没有插拔时, 反复检查应当都返回 `None`(不该凭空报变化)。
    #[test]
    fn watcher_reports_nothing_when_unchanged() {
        let mut watcher = ProbeWatcher::start().expect("启动监听失败");
        for _ in 0..3 {
            assert!(watcher.try_next().is_none(), "状态没变却说变了");
        }
    }

    /// 真机插拔测试(默认忽略): 跑起来后插上/拔掉探针各一次。
    /// 跑法: `cargo test hotplug -- --ignored --show-output`
    #[test]
    #[ignore = "需要手动拔插探针"]
    fn hotplug_reports_changes() {
        let mut watcher = ProbeWatcher::start().unwrap();
        println!("当前状态: {}", detect_probe().unwrap());
        println!(
            "最长监听 90 秒: 请插上/拔掉探针各一次(每 100ms 查一次, 观察到两次变化就提前结束)"
        );

        let deadline = Instant::now() + Duration::from_secs(90);
        let mut changes = 0;
        while Instant::now() < deadline && changes < 2 {
            if let Some(state) = watcher.try_next() {
                println!("状态变化: {state}");
                changes += 1;
            }
            thread::sleep(Duration::from_millis(100));
        }
        assert!(changes > 0, "监听期间没观察到任何插拔");
    }
}
