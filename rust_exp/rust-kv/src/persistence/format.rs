//! WAL 的文本编码格式。
//!
//! 键和值先转为十六进制，避免制表符和换行破坏记录边界；每条记录附带校验和，
//! 启动恢复时可识别截断或篡改。读取时兼容早期 v1 文件，写入使用 v2。

use crate::domain::Command;

const LEGACY_WAL_HEADER: &str = "RUST_KV_WAL\t1";
pub(crate) const MAX_RECORD_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
pub(crate) enum FormatError {
    Invalid(String),
    UnsupportedCommand,
}

/// 生成 WAL v2 文件头。base_seq 表示已经包含在快照中的最后序列号。
pub(crate) fn encode_header(base_seq: u64) -> String {
    format!("RUST_KV_WAL\t2\t{base_seq}\n")
}

/// 解析 WAL 文件头，同时兼容上一版没有 base_seq 的 v1 文件。
pub(crate) fn decode_header(line: &str) -> Result<u64, FormatError> {
    if line == LEGACY_WAL_HEADER {
        return Ok(0);
    }

    let fields: Vec<&str> = line.split('\t').collect();
    if fields.len() != 3 || fields[0] != "RUST_KV_WAL" || fields[1] != "2" {
        return Err(FormatError::Invalid(
            "missing or unsupported WAL header".into(),
        ));
    }

    fields[2]
        .parse::<u64>()
        .map_err(|_| FormatError::Invalid("invalid WAL base sequence".into()))
}

pub(crate) fn encode_record(seq: u64, command: &Command) -> Result<String, FormatError> {
    let (operation, key, value) = match command {
        Command::Set { key, value } => ("S", key.as_str(), value.as_str()),
        Command::Update { key, value } => ("U", key.as_str(), value.as_str()),
        Command::Delete { key } => ("D", key.as_str(), ""),
        Command::Get { .. } | Command::Keys | Command::Status => {
            return Err(FormatError::UnsupportedCommand);
        }
    };

    let payload = format!(
        "{seq}\t{operation}\t{}\t{}",
        encode_hex(key.as_bytes()),
        encode_hex(value.as_bytes())
    );
    let checksum = checksum64(payload.as_bytes());
    Ok(format!("{payload}\t{checksum:016X}\n"))
}

pub(crate) fn decode_record(line: &str) -> Result<(u64, Command), FormatError> {
    let fields: Vec<&str> = line.split('\t').collect();
    if fields.len() != 5 {
        return Err(FormatError::Invalid(format!(
            "expected 5 fields, found {}",
            fields.len()
        )));
    }

    let seq = fields[0]
        .parse::<u64>()
        .map_err(|_| FormatError::Invalid("invalid sequence number".into()))?;
    if seq == 0 {
        return Err(FormatError::Invalid(
            "sequence number must be greater than zero".into(),
        ));
    }

    let payload = fields[..4].join("\t");
    let expected_checksum = u64::from_str_radix(fields[4], 16)
        .map_err(|_| FormatError::Invalid("invalid checksum".into()))?;
    let actual_checksum = checksum64(payload.as_bytes());
    if expected_checksum != actual_checksum {
        return Err(FormatError::Invalid(format!(
            "checksum mismatch: expected {expected_checksum:016X}, calculated {actual_checksum:016X}"
        )));
    }

    let key = decode_utf8_hex(fields[2], "key")?;
    let value = decode_utf8_hex(fields[3], "value")?;

    let command = match fields[1] {
        "S" => Command::Set { key, value },
        "U" => Command::Update { key, value },
        "D" => {
            if !value.is_empty() {
                return Err(FormatError::Invalid(
                    "DELETE record contains a value".into(),
                ));
            }
            Command::Delete { key }
        }
        operation => {
            return Err(FormatError::Invalid(format!(
                "unknown operation: {operation}"
            )));
        }
    };

    Ok((seq, command))
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0F) as usize] as char);
    }
    output
}

fn decode_utf8_hex(value: &str, field_name: &str) -> Result<String, FormatError> {
    if !value.len().is_multiple_of(2) {
        return Err(FormatError::Invalid(format!(
            "{field_name} has an odd number of hexadecimal digits"
        )));
    }

    let mut bytes = Vec::with_capacity(value.len() / 2);
    let raw = value.as_bytes();
    for pair in raw.chunks_exact(2) {
        let high = hex_digit(pair[0]).ok_or_else(|| {
            FormatError::Invalid(format!("{field_name} contains invalid hexadecimal data"))
        })?;
        let low = hex_digit(pair[1]).ok_or_else(|| {
            FormatError::Invalid(format!("{field_name} contains invalid hexadecimal data"))
        })?;
        bytes.push((high << 4) | low);
    }

    String::from_utf8(bytes)
        .map_err(|_| FormatError::Invalid(format!("{field_name} is not valid UTF-8")))
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// 使用 FNV-1a 检测文件的意外修改或截断；它不是安全加密算法。
pub(crate) fn checksum64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_round_trip_preserves_unicode_and_whitespace() {
        let command = Command::Set {
            key: "课程\t名称".into(),
            value: "Rust\n程序设计".into(),
        };
        let encoded = encode_record(7, &command).unwrap();
        let decoded = decode_record(encoded.trim_end_matches('\n')).unwrap();

        assert_eq!(decoded, (7, command));
    }

    #[test]
    fn wal_header_round_trip_preserves_the_base_sequence() {
        let header = encode_header(42);
        assert_eq!(decode_header(header.trim_end_matches('\n')).unwrap(), 42);
        assert_eq!(decode_header(LEGACY_WAL_HEADER).unwrap(), 0);
    }

    #[test]
    fn checksum_changes_are_rejected() {
        let command = Command::Delete {
            key: "course".into(),
        };
        let mut encoded = encode_record(1, &command).unwrap();
        let checksum_index = encoded.len() - 2;
        let replacement = if &encoded[checksum_index..checksum_index + 1] == "0" {
            "1"
        } else {
            "0"
        };
        encoded.replace_range(checksum_index..checksum_index + 1, replacement);

        assert!(matches!(
            decode_record(encoded.trim_end_matches('\n')),
            Err(FormatError::Invalid(_))
        ));
    }
}
