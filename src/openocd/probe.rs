//! 探针(烧录器)检测: 从 Linux sysfs 认出插着的是 ST-Link 还是 J-Link。

use std::fmt;
use std::fs;
use std::io;
use std::os::fd::AsFd;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use nix::errno::Errno;
use nix::poll::{poll, PollFd, PollFlags};
use nix::sys::eventfd::{EfdFlags, EventFd};
use udev::{EventType, MonitorBuilder};

/// sysfs 里 USB 设备所在的目录(每个设备一个子目录)。
const USB_DEVICES: &str = "/sys/bus/usb/devices";

/// 轮询兜底超时(毫秒): 关闭由 eventfd 立即唤醒, 这里只是保险丝。
const POLL_TIMEOUT_MS: u16 = 1000;

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
    /// 监听 USB 插拔事件失败
    #[error("监听 USB 插拔事件失败")]
    Watch(#[source] io::Error),
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

/// 监听探针插拔(基于 udev 内核事件)。
///
/// 后台线程只在状态**发生变化**时往通道里放一个 [`ProbeState`],
/// 上层用 [`ProbeWatcher::try_next`] 非阻塞地取。
/// 丢弃 watcher 时会停止并等待线程收工(`Drop` 里加入), 不留游离线程。
pub struct ProbeWatcher {
    states: Receiver<ProbeState>,
    /// 通知监听线程收工
    stop: Arc<AtomicBool>,
    /// 写一个字节就能立刻叫醒卡在 poll 上的监听线程
    wake: Arc<EventFd>,
    /// 监听线程句柄; `Drop` 里 join 之后才算真正关闭
    thread: Option<JoinHandle<()>>,
}

impl Drop for ProbeWatcher {
    /// 置停止标志 → 唤醒线程 → 等它退出。
    /// 关闭是确定性的, 不依赖"下次碰巧有 USB 事件"。
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.wake.write(1);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl ProbeWatcher {
    /// 启动监听, 并立刻把当前状态作为第一个事件放进通道。
    pub fn start() -> Result<Self, ProbeError> {
        let (sender, states) = mpsc::channel();
        let (ready_sender, ready) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        // eventfd 只用来"叫醒"阻塞中的监听线程, 让它立刻看到停止标志
        let wake = Arc::new(
            EventFd::from_value_and_flags(0, EfdFlags::EFD_NONBLOCK)
                .map_err(|e| ProbeError::Watch(io::Error::from(e)))?,
        );
        let thread_stop = Arc::clone(&stop);
        let thread_wake = Arc::clone(&wake);

        // udev 的监听器内部是裸指针, 不是 Send, 因此只能在监听线程里创建并持有它
        let thread = thread::spawn(move || {
            let socket = match MonitorBuilder::new()
                .and_then(|builder| builder.match_subsystem_devtype("usb", "usb_device"))
                .and_then(|builder| builder.listen())
            {
                Ok(socket) => socket,
                Err(e) => {
                    let _ = ready_sender.send(Err(ProbeError::Watch(e)));
                    return;
                }
            };
            let fd = socket.as_fd();
            let mut last = match detect_probe() {
                Ok(state) => state,
                Err(e) => {
                    let _ = ready_sender.send(Err(e));
                    return;
                }
            };
            let _ = ready_sender.send(Ok(())); // 启动成功, 放行 start()
            let _ = sender.send(last.clone());

            loop {
                if thread_stop.load(Ordering::Relaxed) {
                    return;
                }
                // 同时等: udev 事件 fd 与"被 Drop 叫醒"的 eventfd
                let mut fds = [
                    PollFd::new(fd, PollFlags::POLLIN),
                    PollFd::new(thread_wake.as_fd(), PollFlags::POLLIN),
                ];
                match poll(&mut fds, POLL_TIMEOUT_MS) {
                    Ok(0) => continue, // 兜底超时, 回头看看停止标志
                    Ok(_) => {}
                    Err(Errno::EINTR) => continue, // 被信号打断, 重试
                    Err(_) => return,
                }
                if thread_stop.load(Ordering::Relaxed) {
                    return; // 这次醒来是 Drop 叫的
                }

                let Some(event) = socket.iter().next() else {
                    continue;
                };
                if !matches!(event.event_type(), EventType::Add | EventType::Remove) {
                    continue;
                }
                // 内核事件与 sysfs 增删之间可能有极短的时间差, 稍等一下再扫
                thread::sleep(Duration::from_millis(50));
                let Ok(state) = detect_probe() else { continue };
                if state != last {
                    last = state.clone();
                    if sender.send(state).is_err() {
                        return; // 上层已丢弃 watcher, 收工
                    }
                }
            }
        });

        // 等监听线程报告"启动成功/失败"
        let started = ready
            .recv()
            .map_err(|e| ProbeError::Watch(io::Error::other(e)))?;
        if let Err(e) = started {
            let _ = thread.join();
            return Err(e);
        }
        Ok(Self {
            states,
            stop,
            wake,
            thread: Some(thread),
        })
    }

    /// 非阻塞取一个状态变化; `None` 表示自上次之后没有变化。
    pub fn try_next(&self) -> Option<ProbeState> {
        self.states.try_recv().ok()
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
    use std::time::Instant;

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

    /// 监听器启动后应立刻给出当前状态, 免得上层还要自己查一次。
    #[test]
    fn watcher_reports_state_on_start() {
        let watcher = ProbeWatcher::start().expect("启动 udev 监听失败");
        let first = watcher.try_next().expect("启动后应立刻有一个状态");
        println!("启动时的状态: {first}");
    }

    /// 丢弃 watcher 应当停止监听线程并立刻返回(确定性关闭)。
    #[test]
    fn watcher_stops_on_drop() {
        let watcher = ProbeWatcher::start().expect("启动 udev 监听失败");
        let started = Instant::now();
        drop(watcher); // 内部: 置停止标志 + join
        let elapsed = started.elapsed();
        println!("关闭耗时: {elapsed:?}");
        assert!(
            elapsed < Duration::from_millis(50),
            "关闭太慢: {elapsed:?} (应当被 eventfd 立即唤醒)"
        );
    }

    /// 真机插拔测试(默认忽略): 跑起来后插上再拔掉探针。
    /// 跑法: `cargo test hotplug -- --ignored --show-output`
    #[test]
    #[ignore = "需要手动拔插探针"]
    fn hotplug_reports_changes() {
        let watcher = ProbeWatcher::start().unwrap();
        // 启动时给出的当前状态不算"变化", 先取走
        println!(
            "启动时的状态: {}",
            watcher.try_next().expect("启动后应立刻有状态")
        );
        println!("最长监听 90 秒: 请插上/拔掉探针各一次(观察到两次变化就提前结束)");

        let deadline = Instant::now() + Duration::from_secs(90);
        let mut changes = 0;
        while Instant::now() < deadline && changes < 2 {
            while let Some(state) = watcher.try_next() {
                println!("状态变化: {state}");
                changes += 1;
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert!(changes > 0, "监听期间没观察到任何插拔");
    }
}
