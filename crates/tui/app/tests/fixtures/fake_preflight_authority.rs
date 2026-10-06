use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use serde_json::json;

fn main() -> io::Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    let [_, state, audit] = args.as_slice() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected fixed state and audit paths",
        ));
    };

    let state = Path::new(state);
    let audit = Path::new(audit);
    let bytes = fs::read(state)?;
    let mut log = OpenOptions::new().create(true).append(true).open(audit)?;
    log.write_all(
        format!(
            "{}\n",
            json!({"method":"authority", "state_bytes": bytes.len()})
        )
        .as_bytes(),
    )?;
    if bytes
        .windows(b"\"fixture_slow\":true".len())
        .any(|window| window == b"\"fixture_slow\":true")
    {
        std::thread::sleep(std::time::Duration::from_secs(3));
        fs::write(
            audit.with_extension("survived"),
            "authority survived timeout",
        )?;
    }

    if bytes
        .windows(b"\"fixture_fail\":true".len())
        .any(|window| window == b"\"fixture_fail\":true")
    {
        return Err(io::Error::other("synthetic authority failure"));
    }

    io::stdout().write_all(&bytes)?;
    Ok(())
}
