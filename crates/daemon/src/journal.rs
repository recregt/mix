//! Audit entries written to the systemd journal with its native protocol.
//!
//! Note: Without a running journald the entry is dropped.

use std::os::unix::net::UnixDatagram;

/// Datagram socket journald reads native protocol entries from.
const SOCKET: &str = "/run/systemd/journal/socket";

fn encode(fields: &[(&str, String)]) -> Vec<u8> {
    let mut entry = Vec::new();
    for (name, value) in fields {
        entry.extend_from_slice(name.as_bytes());
        if value.contains('\n') {
            entry.push(b'\n');
            entry.extend_from_slice(&(value.len() as u64).to_le_bytes());
        } else {
            entry.push(b'=');
        }
        entry.extend_from_slice(value.as_bytes());
        entry.push(b'\n');
    }
    entry
}

pub fn record(fields: &[(&str, String)]) {
    let _ = UnixDatagram::unbound().and_then(|socket| socket.send_to(&encode(fields), SOCKET));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_value_is_written_as_name_equals_value() {
        assert_eq!(
            encode(&[
                ("MIX_USER", "alice".to_string()),
                ("PRIORITY", "6".to_string())
            ]),
            b"MIX_USER=alice\nPRIORITY=6\n"
        );
    }

    #[test]
    fn a_value_with_a_newline_is_written_with_its_length() {
        let mut expected = b"MESSAGE\n".to_vec();
        expected.extend_from_slice(&3u64.to_le_bytes());
        expected.extend_from_slice(b"a\nb\n");

        assert_eq!(encode(&[("MESSAGE", "a\nb".to_string())]), expected);
    }
}
