//! OpenOCD 调试后端。所有与 OpenOCD 打交道的细节都收在本模块, 上层只调用公开函数:
//! - `detect_probe()`: 认出插着的是哪个烧录器(内部由 probe 子模块实现, 不对外暴露)
//! - `OpenOcd::new` / `connect`: 连接 OpenOCD

mod probe;

pub use probe::{detect_probe, Probe, ProbeError, ProbeKind};

use std::io::{self, Read};
use std::net::TcpStream;

/// OpenOCD telnet 客户端。
pub struct OpenOcd {
    host: String,
    port: u16,
    stream: Option<TcpStream>, // 连接成功后才 Some
}

impl OpenOcd {
    /// 构造函数: 登记目标 OpenOCD 的地址, 不联网, 不会失败。
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            stream: None,
        }
    }

    /// 连接 OpenOCD 并完成握手(读到欢迎语末尾的提示符)。
    /// 已连接时重复调用直接成功。
    pub fn connect(&mut self) -> io::Result<()> {
        if self.stream.is_some() {
            return Ok(());
        }
        let mut stream = TcpStream::connect((self.host.as_str(), self.port))?;
        wait_welcome(&mut stream)?;
        self.stream = Some(stream);
        Ok(())
    }
}

/// 读掉欢迎语直到看见提示符 "> ", 确认握手成功。
/// 只检测结尾标记, 不保留内容——不产生任何"读到了却没用的数据"。
fn wait_welcome(stream: &mut TcpStream) -> io::Result<()> {
    let mut prev: u8 = 0; // 上一个读到的字节, 用来识别跨块出现的 "> "
    let mut chunk = [0u8; 512];
    loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "欢迎语未读完, 连接就被关闭了",
            ));
        }
        for &b in &chunk[..n] {
            if prev == b'>' && b == b' ' {
                return Ok(());
            }
            prev = b;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::{SocketAddr, TcpListener};
    use std::thread;

    /// 假 OpenOCD: 接受连接后发送真实格式的欢迎语, 然后断开。
    fn fake_openocd() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            // 真实抓包内容: IAC 协商字节 + 标题 + 提示符
            let _ = s.write_all(
                b"\xff\xfb\x03\xff\xfb\x01\xff\xfd\x03\xff\xfe\x01Open On-Chip Debugger\r\n\r> ",
            );
        });
        addr
    }

    #[test]
    fn connect_ok() {
        let addr = fake_openocd();
        let mut ocd = OpenOcd::new("127.0.0.1", addr.port());
        ocd.connect().unwrap();
        assert!(ocd.stream.is_some());
    }

    #[test]
    fn connect_refused_errors() {
        // 绑定后立刻释放, 得到一个无人监听的端口
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut ocd = OpenOcd::new("127.0.0.1", port);
        assert!(ocd.connect().is_err());
    }
}
