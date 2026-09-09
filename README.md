# dbg

实现一个 Linux 平台的通用 MCU 调试器。

## 结构

```
src/lib.rs        核心库: 声明 openocd 模块
src/openocd.rs    OpenOCD 调试后端(目前: 连接)
src/main.rs       可执行程序入口(目前: 仅 hello world)
```

分工: OpenOCD 负责硬件层(断点/运行/读写内存); 我们的程序是语义层,
两者只通过 OpenOCD 的 telnet 端口(默认 4444)通信。板子/调试器型号
只是 OpenOCD 启动时的 `-f` 参数, 与本项目无关。

## 当前进度

里程碑 0 的第一小步: **能连上 OpenOCD**。公共接口只有两个:

```rust
use dbg::openocd::OpenOcd;

let mut ocd = OpenOcd::new("127.0.0.1", 4444); // 构造函数(不联网, 不会失败)
ocd.connect()?;                                   // 连接并完成握手
```

用法:
- `cargo build` / `cargo run`(打印 hello world)
- `cargo test`: 单元测试内置假 OpenOCD 服务器, 无需硬件即可验证连接

先按你的板子启动 OpenOCD(例如 `openocd -f interface/stlink.cfg -f
target/stm32f4x.cfg`), 再调用上面两行代码即可验证与真实 OpenOCD 连通。

## 路线图

| 里程碑 | 内容 | 状态 |
|---|---|---|
| 0 | OpenOCD 连接(new / connect) | ✅ |
| 1 | 命令收发(halt/resume/读内存…) | ⬜ |
| 2 | 固件符号解析, 按名字打断点/看变量 | ⬜ |
| 远期 | 上层程序: CLI / 变量面板; 或自研 ADIv5 | ⬜ |
