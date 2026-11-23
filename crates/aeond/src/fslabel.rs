//! Reading a volume label straight from the start of a block device, so the data partition can
//! be found without `blkid` or udev (the image has neither).
//!
//! Supported: ext2/3/4 and FAT12/16/32 (the formats a person can make on any computer).

/// Bytes of the device needed by [`read_label`].
pub const PROBE_BYTES: usize = 2048;

fn clean(bytes: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(bytes);
    let s = s.trim_end_matches(['\0', ' ']);
    (!s.is_empty()).then(|| s.to_string())
}

/// The volume label from the first [`PROBE_BYTES`] of a device, or `None` when the filesystem
/// is unknown or has no label.
#[must_use]
pub fn read_label(head: &[u8]) -> Option<String> {
    // ext2/3/4: superblock at 1024, magic 0xEF53 at +56, 16-byte label at +120.
    if head.len() >= 1160 && head[1080..1082] == [0x53, 0xEF] {
        return clean(&head[1144..1160]);
    }
    if head.len() >= 512 && head[510..512] == [0x55, 0xAA] {
        // FAT32: type string at 82, label at 71.
        if head[82..87] == *b"FAT32" {
            return clean(&head[71..82]);
        }
        // FAT12/16: type string at 54, label at 43.
        if head[54..57] == *b"FAT" && matches!(&head[57..59], b"12" | b"16") {
            return clean(&head[43..54]);
        }
    }
    None
}

/// First device in `names` whose label equals `label` (ignoring case, since FAT upper-cases
/// labels). `probe` returns a device's label.
pub fn find_labelled<F: FnMut(&str) -> Option<String>>(
    names: &[String],
    label: &str,
    mut probe: F,
) -> Option<String> {
    names
        .iter()
        .find(|n| probe(n).is_some_and(|l| l.eq_ignore_ascii_case(label)))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ext4(label: &str) -> Vec<u8> {
        let mut b = vec![0u8; PROBE_BYTES];
        b[1080] = 0x53;
        b[1081] = 0xEF;
        b[1144..1144 + label.len()].copy_from_slice(label.as_bytes());
        b
    }

    fn fat(label: &str, kind: &str) -> Vec<u8> {
        let mut b = vec![0u8; PROBE_BYTES];
        b[510] = 0x55;
        b[511] = 0xAA;
        let mut l = format!("{label:<11}").into_bytes();
        l.truncate(11);
        if kind == "FAT32" {
            b[71..82].copy_from_slice(&l);
            b[82..90].copy_from_slice(b"FAT32   ");
        } else {
            b[43..54].copy_from_slice(&l);
            b[54..62].copy_from_slice(format!("{kind:<8}").as_bytes());
        }
        b
    }

    #[test]
    fn reads_ext4_and_fat_labels() {
        assert_eq!(read_label(&ext4("AEON-DATA")).as_deref(), Some("AEON-DATA"));
        assert_eq!(read_label(&ext4("")), None);
        assert_eq!(
            read_label(&fat("AEON-DATA", "FAT32")).as_deref(),
            Some("AEON-DATA")
        );
        assert_eq!(read_label(&fat("USB", "FAT16")).as_deref(), Some("USB"));
        assert_eq!(read_label(&fat("X", "FAT12")).as_deref(), Some("X"));
    }

    #[test]
    fn unknown_or_short_input_has_no_label() {
        assert_eq!(read_label(&[0u8; PROBE_BYTES]), None);
        assert_eq!(read_label(&[]), None);
        assert_eq!(read_label(&[0xEF; 100]), None);
        // an exFAT/NTFS boot sector is not recognised
        let mut b = vec![0u8; PROBE_BYTES];
        b[510] = 0x55;
        b[511] = 0xAA;
        b[3..11].copy_from_slice(b"EXFAT   ");
        assert_eq!(read_label(&b), None);
    }

    #[test]
    fn finds_by_label_ignoring_case() {
        let names: Vec<String> = ["sda1", "sdb", "sdc1"].map(String::from).to_vec();
        let found = find_labelled(&names, "AEON-DATA", |n| {
            Some(if n == "sdb" { "aeon-data" } else { "OTHER" }.to_string())
        });
        assert_eq!(found.as_deref(), Some("sdb"));
        assert_eq!(find_labelled(&names, "AEON-DATA", |_| None), None);
    }
}
