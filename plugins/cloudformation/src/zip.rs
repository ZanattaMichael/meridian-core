//! Reading the entries of a zip archive.
//!
//! The resource provider schemas come as one zip per region, a few
//! megabytes holding a JSON file per type, stored flat or deflated. That is
//! all this reader handles: the central directory at the end of the file,
//! each entry's local header, and the two methods the archive uses. Nothing
//! is written, no entry is extracted to disk, and an archive that is not
//! what this expects is an error with the reason.

use std::io::Read as _;

use flate2::read::DeflateDecoder;

/// An entry's name and its bytes, inflated.
pub struct Entry {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// How large one inflated entry may be: a schema is tens of kilobytes;
/// this is generous.
const MAX_ENTRY_BYTES: u64 = 64 << 20;

const END_OF_CENTRAL_DIRECTORY: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
const CENTRAL_FILE_HEADER: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];
const LOCAL_FILE_HEADER: [u8; 4] = [0x50, 0x4b, 0x03, 0x04];
const STORED: u16 = 0;
const DEFLATED: u16 = 8;

fn u16_at(bytes: &[u8], at: usize) -> Result<u16, String> {
    bytes
        .get(at..at + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or_else(|| "the archive ends inside a header".to_owned())
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, String> {
    bytes
        .get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| "the archive ends inside a header".to_owned())
}

/// Every entry of the archive, in the order of its central directory,
/// handed to `each` as it is read. Directories (names ending in `/`) are
/// skipped.
pub fn read_entries(
    bytes: &[u8],
    mut each: impl FnMut(Entry) -> Result<(), String>,
) -> Result<(), String> {
    // The end-of-central-directory record is the last thing in the file,
    // followed only by a comment of up to 64 KB.
    let tail_start = bytes.len().saturating_sub(22 + 0xffff);
    let eocd = (tail_start..bytes.len().saturating_sub(21))
        .rev()
        .find(|&i| bytes[i..i + 4] == END_OF_CENTRAL_DIRECTORY)
        .ok_or_else(|| "not a zip archive: no end of central directory".to_owned())?;
    let entries = u16_at(bytes, eocd + 10)? as usize;
    let mut at = u32_at(bytes, eocd + 16)? as usize;
    for _ in 0..entries {
        if bytes.get(at..at + 4) != Some(&CENTRAL_FILE_HEADER) {
            return Err("the central directory is damaged".to_owned());
        }
        let method = u16_at(bytes, at + 10)?;
        let compressed = u32_at(bytes, at + 20)? as usize;
        let uncompressed = u32_at(bytes, at + 24)? as u64;
        let name_len = u16_at(bytes, at + 28)? as usize;
        let extra_len = u16_at(bytes, at + 30)? as usize;
        let comment_len = u16_at(bytes, at + 32)? as usize;
        let local = u32_at(bytes, at + 42)? as usize;
        let name = bytes
            .get(at + 46..at + 46 + name_len)
            .ok_or_else(|| "the archive ends inside a name".to_owned())?;
        let name = String::from_utf8_lossy(name).into_owned();
        at += 46 + name_len + extra_len + comment_len;
        if name.ends_with('/') {
            continue;
        }
        if uncompressed > MAX_ENTRY_BYTES {
            return Err(format!(
                "{name} is {uncompressed} bytes, more than this reader takes"
            ));
        }
        if bytes.get(local..local + 4) != Some(&LOCAL_FILE_HEADER) {
            return Err(format!("the local header of {name} is damaged"));
        }
        let local_name_len = u16_at(bytes, local + 26)? as usize;
        let local_extra_len = u16_at(bytes, local + 28)? as usize;
        let start = local + 30 + local_name_len + local_extra_len;
        let data = bytes
            .get(start..start + compressed)
            .ok_or_else(|| format!("the archive ends inside {name}"))?;
        let bytes = match method {
            STORED => data.to_vec(),
            DEFLATED => {
                let mut out = Vec::with_capacity(uncompressed as usize);
                DeflateDecoder::new(data)
                    .take(MAX_ENTRY_BYTES)
                    .read_to_end(&mut out)
                    .map_err(|e| format!("{name} could not be inflated: {e}"))?;
                out
            }
            other => {
                return Err(format!(
                    "{name} uses compression method {other}, which this reader does not take"
                ))
            }
        };
        each(Entry { name, bytes })?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds an archive of stored entries, the way any zip writer would.
    pub(crate) fn stored_archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data) in entries {
            let offset = out.len() as u32;
            let crc = 0u32; // Not checked by the reader.
            out.extend_from_slice(&LOCAL_FILE_HEADER);
            out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);
            central.extend_from_slice(&CENTRAL_FILE_HEADER);
            central.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let central_offset = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&END_OF_CENTRAL_DIRECTORY);
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&central_offset.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out
    }

    #[test]
    fn stored_entries_are_read_in_order() {
        let archive = stored_archive(&[("a.json", b"{\"a\":1}"), ("dir/", b""), ("b.json", b"{}")]);
        let mut seen = Vec::new();
        read_entries(&archive, |e| {
            seen.push((e.name, String::from_utf8(e.bytes).unwrap()));
            Ok(())
        })
        .unwrap();
        assert_eq!(
            seen,
            [
                ("a.json".to_owned(), "{\"a\":1}".to_owned()),
                ("b.json".to_owned(), "{}".to_owned())
            ]
        );
    }

    #[test]
    fn a_deflated_entry_is_inflated() {
        use flate2::write::DeflateEncoder;
        use flate2::Compression;
        use std::io::Write as _;
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(b"hello hello hello hello").unwrap();
        let data = encoder.finish().unwrap();
        let mut archive = stored_archive(&[("x.txt", &data)]);
        // Flip the method of the one entry to deflated, in both headers,
        // and its uncompressed size in the central directory.
        archive[8] = 8;
        let central = archive.len() - 22 - (46 + 5);
        archive[central + 10] = 8;
        archive[central + 24..central + 28].copy_from_slice(&23u32.to_le_bytes());
        let mut seen = Vec::new();
        read_entries(&archive, |e| {
            seen.push(e.bytes);
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, [b"hello hello hello hello".to_vec()]);
    }

    #[test]
    fn something_that_is_not_a_zip_is_refused() {
        let e = read_entries(b"{\"not\": \"a zip\"}", |_| Ok(())).unwrap_err();
        assert!(e.contains("not a zip archive"), "{e}");
    }
}
