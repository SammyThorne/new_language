//! Standard output. Buffered in blocks when it is a file or pipe, flushed after every write when
//! it is a terminal, as Python does.

use std::io::{BufWriter, IsTerminal, Stdout, Write};
use std::sync::Mutex;

static OUT: Mutex<Option<BufWriter<Stdout>>> = Mutex::new(None);

pub fn write(bytes: &[u8]) {
    let mut guard = OUT.lock().unwrap_or_else(|e| e.into_inner());
    let writer = guard.get_or_insert_with(|| BufWriter::with_capacity(64 * 1024, std::io::stdout()));
    // Output errors (a closed pipe, a full disk) are ignored, as when Python's standard output
    // has gone away.
    let _ = writer.write_all(bytes);
    if std::io::stdout().is_terminal() {
        let _ = writer.flush();
    }
}

pub fn flush() {
    let mut guard = OUT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(writer) = guard.as_mut() {
        let _ = writer.flush();
    }
}
