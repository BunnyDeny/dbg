# dbg

实现一个 Linux 平台的通用 MCU 调试器。

## 结构

```
src/lib.rs            核心库: 声明 openocd 模块
src/openocd.rs        OpenOCD 调试后端: 探针检测/热插拔 + 连接
src/openocd/probe.rs  探针子模块(私有): USB 识别 ST-Link / J-Link + udev 插拔监听
src/main.rs           可执行程序入口(目前: 仅 hello world)
```

分工: OpenOCD 负责硬件层(断点/运行/读写内存); 我们的程序是语义层,
两者只通过 OpenOCD 的 telnet 端口(默认 4444)通信。板子/调试器型号
只是 OpenOCD 启动时的 `-f` 参数, 与本项目无关。

## 当前进度

里程碑 0: **认出插着的烧录器(含热插拔), 并能连上 OpenOCD**。公开接口:

```rust
use dbg::openocd::{detect_probe, OpenOcd, ProbeState, ProbeWatcher};

// 1) 查一次探针状态("没插"是状态, 不是错误)
match detect_probe()? {
    ProbeState::Connected(probe) => println!("{probe} -> -f {}", probe.kind.interface_config()),
    ProbeState::Disconnected => println!("没插探针"),
}
// 例如: ST-Link (SN: 003F00313234510136303532) -> -f interface/stlink.cfg

// 2) 监听插拔: 每次调用扫一次 sysfs, 状态变了才返回
let mut watcher = ProbeWatcher::start()?;   // 记录当前状态作为基线
loop {
    if let Some(state) = watcher.try_next()? {   // 非阻塞; 没变化返回 Ok(None)
        println!("状态变化: {state}");
    }
    std::thread::sleep(std::time::Duration::from_millis(100));
}

// 3) 连接 OpenOCD
let mut ocd = OpenOcd::new("127.0.0.1", 4444);     // 构造函数(不联网, 不会失败)
ocd.connect()?;                                    // 连接并完成握手
```

探针识别的实现要点:
- 扫 `/sys/bus/usb/devices/` 读 `idVendor` / `idProduct` / `serial`;
- **ST-Link 按 PID 白名单认**(`0483` 下还有虚拟串口等非调试产品);
  **J-Link 按 VID 认**(`1366` 下基本只有调试器);
- 目前只区分 ST-Link / J-Link 两大类; 插多个时取第一个;
- "没插探针"是**状态**(`ProbeState::Disconnected`), 不是错误。

错误处理约定(照 minigrep 的简单路子):
- 不使用自定义错误类型: 模块里凡可能失败的地方都返回 `io::Result<...>`;
- 失败用 `?` 一路往上抛, 最终由最上层的 main 打印并退出:
  `if let Err(e) = run() { eprintln!("{e}"); process::exit(1); }`;
- 没有第三方依赖。

热插拔的做法(刻意选了最朴素的一种):
- **没有后台线程**: `try_next()` 就是"扫一次 sysfs, 和上次比", 变了才返回;
- 实测一次扫描约 **41µs**, 所以按 100Hz 调用也只占单核 0.4%, 不值得为它开线程;
- 不存在"线程怎么关"的问题, 运行环境也不需要 libudev;
- 将来若真需要"没人调用时也能收到事件", 可以把 udev 事件塞进 `try_next()` 内部,
  公开接口不用改。

用法:
- `cargo build` / `cargo run`(打印 hello world)
- `cargo test`: 8 个单元测试, 无需硬件
- 真机插拔测试(默认忽略, 需手动插拔探针, 90 秒内插上再拔掉):
  `cargo test hotplug -- --ignored --show-output`

先按你的板子启动 OpenOCD(例如 `openocd -f interface/stlink.cfg -f
target/stm32f4x.cfg`), 再调用 `OpenOcd::connect()` 即可验证连通。

## 路线图

| 里程碑 | 内容 | 状态 |
|---|---|---|
| 0 | 探针检测 + 热插拔监听 + OpenOCD 连接 | ✅ |
| 1 | 命令收发(halt/resume/读内存…) | ⬜ |
| 2 | 固件符号解析, 按名字打断点/看变量 | ⬜ |
| 远期 | 上层程序: CLI / 变量面板; 或自研 ADIv5 | ⬜ |
