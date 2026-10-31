//! 最小化地验证跨设备信令：只发一条 Leave，看对端收不收得到。
//!
//! 用法：cargo run -p kanami-net --example sig_probe -- <IP:端口>

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use kanami_net::signaling::{Signal, SignalKind};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr: SocketAddr = std::env::args().nth(1).expect("用法：sig_probe <IP:端口>").parse()?;
    println!("连接 {addr}");

    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    println!("✓ TCP 已建立，本端 {}", stream.local_addr()?);

    let frame = Signal::new(SignalKind::Leave, b"probe".to_vec()).encode();
    println!("准备发送 {} 字节：{:02x?}", frame.len(), frame);
    let n = stream.write(&frame)?;
    stream.flush()?;
    println!("✓ 已写出 {n} 字节");
    std::thread::sleep(Duration::from_millis(200));

    // 试试能不能读到对端的任何回应
    let mut buf = [0u8; 64];
    match stream.read(&mut buf) {
        Ok(0) => println!("对端关闭了连接"),
        Ok(n) => println!("收到 {n} 字节：{:02x?}", &buf[..n]),
        Err(e) => println!("读失败：{e}"),
    }
    Ok(())
}
