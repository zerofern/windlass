//! Read a `.torrent` file far enough to name and size it: the SHA-1 of its
//! bencoded `info` dictionary, and the length of all its files.

use std::fmt::Write as _;

use sha1::{Digest, Sha1};

use crate::core::{ByteSize, TorrentHash};

/// Nesting deeper than this is not a torrent file.
const MAX_DEPTH: u8 = 32;

/// What Windlass needs from a torrent file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TorrentFile {
    pub hash: TorrentHash,
    pub size: ByteSize,
}

/// A bencoded value; strings and the info dictionary keep their bytes.
#[derive(Debug, PartialEq, Eq)]
enum Value<'a> {
    Integer(i64),
    Bytes(&'a [u8]),
    List(Vec<Self>),
    Dictionary(Vec<(&'a [u8], Self, &'a [u8])>),
}

/// Read a version 1 torrent file; anything else is `None`.
pub(super) fn read(bytes: &[u8]) -> Option<TorrentFile> {
    let (value, rest) = parse(bytes, 0)?;
    if !rest.is_empty() {
        return None;
    }
    let Value::Dictionary(entries) = value else {
        return None;
    };
    let (_, info, encoded) = entries.iter().find(|(key, _, _)| *key == b"info")?;
    let Value::Dictionary(info) = info else {
        return None;
    };
    let field = |name: &[u8]| {
        info.iter()
            .find(|(key, _, _)| *key == name)
            .map(|(_, value, _)| value)
    };
    let size = match (field(b"length"), field(b"files")) {
        (Some(Value::Integer(length)), None) => u64::try_from(*length).ok()?,
        (None, Some(Value::List(files))) => files.iter().try_fold(0_u64, |total, file| {
            let Value::Dictionary(file) = file else {
                return None;
            };
            let length = file.iter().find_map(|(key, value, _)| match value {
                Value::Integer(length) if *key == b"length" => u64::try_from(*length).ok(),
                _ => None,
            })?;
            total.checked_add(length)
        })?,
        _ => return None,
    };
    let hex = Sha1::digest(encoded)
        .iter()
        .fold(String::with_capacity(40), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
    Some(TorrentFile {
        hash: TorrentHash::parse(&hex)?,
        size: ByteSize(size),
    })
}

/// Parse one value; the rest of the input follows it.
fn parse(input: &[u8], depth: u8) -> Option<(Value<'_>, &[u8])> {
    if depth > MAX_DEPTH {
        return None;
    }
    match input.first()? {
        b'i' => {
            let end = input.iter().position(|&byte| byte == b'e')?;
            let digits = std::str::from_utf8(input.get(1..end)?).ok()?;
            Some((Value::Integer(digits.parse().ok()?), input.get(end + 1..)?))
        }
        b'l' => {
            let mut rest = input.get(1..)?;
            let mut items = Vec::new();
            while *rest.first()? != b'e' {
                let (item, after) = parse(rest, depth + 1)?;
                items.push(item);
                rest = after;
            }
            Some((Value::List(items), rest.get(1..)?))
        }
        b'd' => {
            let mut rest = input.get(1..)?;
            let mut entries = Vec::new();
            while *rest.first()? != b'e' {
                let (key, after) = parse(rest, depth + 1)?;
                let Value::Bytes(key) = key else {
                    return None;
                };
                let (value, after_value) = parse(after, depth + 1)?;
                let encoded = after.get(..after.len() - after_value.len())?;
                entries.push((key, value, encoded));
                rest = after_value;
            }
            Some((Value::Dictionary(entries), rest.get(1..)?))
        }
        b'0'..=b'9' => {
            let colon = input.iter().position(|&byte| byte == b':')?;
            let length: usize = std::str::from_utf8(input.get(..colon)?)
                .ok()?
                .parse()
                .ok()?;
            let start = colon + 1;
            let end = start.checked_add(length)?;
            Some((Value::Bytes(input.get(start..end)?), input.get(end..)?))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use sha1::{Digest, Sha1};

    use crate::core::ByteSize;

    use super::read;

    const INFO_SINGLE: &[u8] =
        b"d6:lengthi1048576e4:name8:book.m4b12:piece lengthi262144e6:pieces0:e";
    const INFO_MULTI: &[u8] = b"d5:filesld4:attr1:p6:lengthi100e4:pathl5:a.mp3eed6:lengthi23e4:pathl5:b.mp3eee4:name4:book12:piece lengthi16384e6:pieces0:e";

    fn torrent(info: &[u8]) -> Vec<u8> {
        let mut bytes = b"d8:announce26:https://t.example/announce4:info".to_vec();
        bytes.extend_from_slice(info);
        bytes.push(b'e');
        bytes
    }

    fn hex(info: &[u8]) -> String {
        Sha1::digest(info)
            .iter()
            .fold(String::new(), |mut hex, byte| {
                let _ = write!(hex, "{byte:02x}");
                hex
            })
    }

    #[test]
    fn names_and_sizes_single_and_multi_file_torrents() {
        for (info, size) in [(INFO_SINGLE, 1_048_576), (INFO_MULTI, 123)] {
            let file = read(&torrent(info));
            assert_eq!(
                file.as_ref().map(|file| file.hash.as_str().to_owned()),
                Some(hex(info))
            );
            assert_eq!(file.map(|file| file.size), Some(ByteSize(size)));
        }
    }

    #[test]
    fn refuses_anything_that_is_not_a_version_one_torrent() {
        let mut trailing = torrent(INFO_SINGLE);
        trailing.push(b'x');
        let deep = format!("{}{}", "l".repeat(40), "e".repeat(40));
        for bytes in [
            b"".as_slice(),
            b"<html>Error</html>",
            b"{\"success\":false}",
            &trailing,
            b"le",
            b"de",
            b"d4:infoi1ee",
            b"d4:infod4:name1:xee",
            b"d4:infod6:lengthi-1eee",
            b"d4:infod5:filesli1eeee",
            b"d4:infod5:filesldeeee",
            b"d4:infod6:lengthi1e5:filesleee",
            b"di1ei1ee",
            b"i12",
            b"ixe",
            b"9:short",
            b"x:",
            b"l",
            b"d",
            b"d1:a",
            b"d1:ax",
            b"dxe",
            b"lx",
            b"i\xffe",
            b"\xff:",
            b"99999999999999999999:x",
            b"18446744073709551615:",
            deep.as_bytes(),
        ] {
            assert_eq!(read(bytes), None, "{}", String::from_utf8_lossy(bytes));
        }
    }
}
