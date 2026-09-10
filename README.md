# dbg

实现一个 Linux 平台的通用 MCU 调试器。

## 结构

```
src/lib.rs            核心库: 声明 openocd 模块
src/openocd.rs        OpenOCD 调试后端: 探针检测 + 连接
src/openocd/probe.rs  探针检测子模块(私有): USB 识别 ST-Link / J-Link
src/main.rs           可执行程序入口(目前: 仅 hello world)
```

分工: OpenOCD 负责硬件层(断点/运行/读写内存); 我们的程序是语义层,
两者只通过 OpenOCD 的 telnet 端口(默认 4444)通信。板子/调试器型号
只是 OpenOCD 启动时的 `-f` 参数, 与本项目无关。

## 当前进度

里程碑 0: **认出插着的烧录器, 并能连上 OpenOCD**。公开接口:

```rust
use dbg::openocd::{detect_probe, OpenOcd};

// 1) 认出探针, 顺便拿到它该用的 OpenOCD 配置
let probe = detect_probe()?;                       // 没插 -> Err(ProbeError::NotFound)
println!("{probe} -> -f {}", probe.kind.interface_config());
// 例如: ST-Link (SN: 066FFF) -> -f interface/stlink.cfg

// 2) 连接 OpenOCD
let mut ocd = OpenOcd::new("127.0.0.1", 4444);     // 构造函数(不联网, 不会失败)
ocd.connect()?;                                    // 连接并完成握手
```

探针识别的实现要点:
- 扫 `/sys/bus/usb/devices/` 读 `idVendor` / `idProduct` / `serial`;
- **ST-Link 按 PID 白名单认**(`0483` 下还有虚拟串口等非调试产品);
  **J-Link 按 VID 认**(`1366` 下基本只有调试器);
- 目前只区分 ST-Link / J-Link 两大类; 插多个时取第一个。

用法:
- `cargo build` / `cargo run`(打印 hello world)
- `cargo test`: 6 个单元测试(假 OpenOCD 服务器 + 探针识别表), 无需硬件

先按你的板子启动 OpenOCD(例如 `openocd -f interface/stlink.cfg -f
target/stm32f4x.cfg`), 再调用 `OpenOcd::connect()` 即可验证连通。

## 路线图

| 里程碑 | 内容 | 状态 |
|---|---|---|
| 0 | 探针检测(detect_probe) + OpenOCD 连接(new / connect) | ✅ |
| 1 | 命令收发(halt/resume/读内存…) | ⬜ |
| 2 | 固件符号解析, 按名字打断点/看变量 | ⬜ |
| 远期 | 上层程序: CLI / 变量面板; 或自研 ADIv5 | ⬜ |
