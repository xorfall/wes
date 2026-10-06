//! Test-owned executable compiled into a temporary directory; no host shell or real tool is needed.
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    net::TcpStream,
    time::Duration,
};

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    let mode = args.get(1).expect("fixture mode");
    let _lock = if let Some(path) = args.get(2).filter(|s| !s.is_empty()) {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        file.lock().unwrap();
        Some(file)
    } else {
        None
    };
    let mut control = if let Some(endpoint) = args
        .get(3)
        .filter(|s| !s.is_empty() && !mode.starts_with("descendant") && mode != "background")
    {
        let mut stream = TcpStream::connect(endpoint).unwrap();
        stream.write_all(b"ready").unwrap();
        Some(stream)
    } else {
        None
    };
    match mode.as_str() {
        "handover" => {
            let stream = control.as_mut().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream.read_exact(&mut [0; 1]).unwrap();
            std::process::exit(7);
        }
        "conversation" => {
            std::io::stdout().write_all(b"answer: ").unwrap();
            std::io::stdout().flush().unwrap();
            let mut input = Vec::new();
            std::io::stdin().read_to_end(&mut input).unwrap();
            std::io::stdout().write_all(&input).unwrap();
            std::io::stderr().write_all(b"done").unwrap();
            std::process::exit(7);
        }
        "duplex" => {
            // Fill both output pipes before reading the large answer. The parent must keep
            // draining while its stdin write is blocked, then close stdin after ordered EOF.
            let errors = std::thread::spawn(|| {
                std::io::stderr()
                    .write_all(&vec![b'e'; 128 * 1024])
                    .unwrap()
            });
            std::io::stdout()
                .write_all(&vec![b'o'; 128 * 1024])
                .unwrap();
            errors.join().unwrap();
            let mut input = Vec::new();
            std::io::stdin().read_to_end(&mut input).unwrap();
            assert_eq!(input.len(), 256 * 1024);
        }
        "bytes" => {
            std::io::stdout().write_all(&[0, 255, 1, 10]).unwrap();
            std::io::stderr().write_all(&[254, 0, 2]).unwrap();
            std::process::exit(7);
        }
        "arg" => std::io::stdout()
            .write_all(
                args.get(4)
                    .map_or(b"absent".as_slice(), |arg| arg.as_bytes()),
            )
            .unwrap(),
        "flood" => {
            let writer = std::thread::spawn(|| {
                std::io::stdout()
                    .write_all(&vec![b'o'; 128 * 1024])
                    .unwrap()
            });
            std::io::stderr()
                .write_all(&vec![b'e'; 128 * 1024])
                .unwrap();
            writer.join().unwrap();
        }
        "limit" => {
            let _ = std::io::stdout().write_all(&vec![b'x'; 1024 * 1024]);
            std::thread::sleep(Duration::from_secs(2));
        }
        "stdin" => {
            let mut input = Vec::new();
            std::io::stdin().read_to_end(&mut input).unwrap();
            print!("{}", input.len());
        }
        "wait" => std::thread::sleep(Duration::from_secs(3)),
        "descendant" | "descendant-flood" | "background" => {
            // The control channel proves this descendant started and observes its actual exit.
            // It remains in the invocation's execution group when the parent exits.
            let descendant = std::path::Path::new(&args[2]).with_extension("desc.lock");
            let mut program = std::process::Command::new(std::env::current_exe().unwrap());
            program.arg(if mode == "descendant-flood" { "pipe-owner-flood" } else { "pipe-owner" }).arg(descendant).arg(&args[3])
                .stdin(std::process::Stdio::null());
            if mode == "background" {
                program.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
            }
            let child = program.spawn().unwrap();
            drop(child);
        }
        "pipe-owner" | "pipe-owner-flood" => {
            if mode == "pipe-owner-flood" {
                let _ = std::io::stdout().write_all(&vec![b'x'; 8192]);
            }
            let stream = control.as_mut().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let _ = stream.read_exact(&mut [0; 1]);
        }
        other => panic!("unknown synthetic fixture: {other}"),
    }
}
