use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

pub struct PtySession {
    pub parser: Arc<Mutex<vt100::Parser>>,
    pub writer: Box<dyn Write + Send>,
    pub child: Box<dyn portable_pty::Child + Send + Sync>,
    pub master: Box<dyn MasterPty + Send>,
}

pub fn spawn_pty_session(path: &Path, cmd: &str, args: &[&str], size: PtySize, scrollback: usize) -> Option<PtySession> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(size).ok()?;
    let mut builder = CommandBuilder::new(cmd);
    for arg in args {
        builder.arg(arg);
    }
    builder.cwd(path);
    let child = pair.slave.spawn_command(builder).ok()?;
    drop(pair.slave);
    let writer = pair.master.take_writer().ok()?;
    let reader = pair.master.try_clone_reader().ok()?;
    let parser = Arc::new(Mutex::new(vt100::Parser::new(size.rows, size.cols, scrollback)));
    let parser_clone = Arc::clone(&parser);
    std::thread::spawn(move || pty_reader_thread(reader, parser_clone));
    Some(PtySession { parser, writer, child, master: pair.master })
}

fn pty_reader_thread(mut reader: Box<dyn Read + Send>, parser: Arc<Mutex<vt100::Parser>>) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if let Ok(mut p) = parser.lock() {
                    p.process(&buf[..n]);
                }
            }
        }
    }
}
