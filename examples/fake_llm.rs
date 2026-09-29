// 流式调试假 LLM：每 150ms 吐一帧（供手动 serve 联调）
use std::io::{Read, Write};
use std::net::TcpListener;
fn main() {
    let l = TcpListener::bind("127.0.0.1:18311").unwrap();
    println!("fake-llm on 18311");
    while let Ok((mut s, _)) = l.accept() {
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 65536];
            let _ = s.read(&mut buf);
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").unwrap();
            for i in 0..5 {
                let frame = format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"字{i}\"}}}}]}}\n\n");
                s.write_all(format!("{:x}\r\n{frame}\r\n", frame.len()).as_bytes()).unwrap();
                s.flush().unwrap();
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
            let tail = "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
            s.write_all(format!("{:x}\r\n{tail}\r\n0\r\n\r\n", tail.len()).as_bytes()).unwrap();
            s.flush().unwrap();
            println!("served one");
        });
    }
}
