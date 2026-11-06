#[derive(Clone, Debug, PartialEq)]
pub struct FileEntry {
    pub path: String,
    pub name: String,
    /// `stat`'s `%F`: `regular file`, `directory`, `symbolic link`, `fifo`, ...
    pub kind: String,
    pub size: u64,
    pub modified: i64,
    pub mode: String,
}

impl FileEntry {
    pub fn is_folder(&self) -> bool {
        self.kind == "directory"
    }
}

/// Lines of `stat -c '%F\t%s\t%Y\t%A\t%n'`; folders first, then by name.
/// shortcut: a name with a newline or tab breaks its line, fine until someone browses such files.
pub fn parse_listing(text: &str) -> Vec<FileEntry> {
    let mut entries = Vec::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.splitn(5, '\t').collect();
        let [kind, size, modified, mode, path] = fields[..] else { continue };
        let (Ok(size), Ok(modified)) = (size.parse(), modified.parse()) else { continue };
        entries.push(FileEntry { path: path.to_string(), name: path.rsplit('/').next().unwrap_or_default().to_string(), kind: kind.to_string(), size, modified, mode: mode.to_string() });
    }
    sort_entries(&mut entries);
    entries
}

pub fn sort_entries(entries: &mut [FileEntry]) {
    entries.sort_by(|a, b| b.is_folder().cmp(&a.is_folder()).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
}

/// The 8-byte header of a multiplexed frame (1 stdout, 2 stderr, big-endian size): stderr, size.
pub fn frame_header(bytes: &[u8]) -> Option<(bool, usize)> {
    let header = bytes.get(..8)?;
    Some((header[0] == 2, u32::from_be_bytes([header[4], header[5], header[6], header[7]]) as usize))
}

/// Splits an exec stream without a TTY into stdout and stderr.
pub fn demultiplex(stream: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let mut offset = 0;
    while let Some((is_stderr, size)) = frame_header(&stream[offset..]) {
        let end = stream.len().min(offset + 8 + size);
        if is_stderr {
            errors.extend_from_slice(&stream[offset + 8..end]);
        } else {
            output.extend_from_slice(&stream[offset + 8..end]);
        }
        offset = end;
    }
    (output, errors)
}

/// The parent of `/etc/apk` is `/etc`; `/` is its own parent.
pub fn parent_folder(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    format!("/{}", parts[..parts.len().saturating_sub(1)].join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_listing_folders_first() {
        let entries =
            parse_listing("regular file\t12\t1700000000\t-rw-r--r--\t/etc/b.conf\ndirectory\t4096\t1700000000\tdrwxr-xr-x\t/etc/apk\nbroken line\nregular file\t3\t1\t-rw-------\t/etc/A.txt\n");
        let names: Vec<_> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, ["apk", "A.txt", "b.conf"]);
        assert!(entries[0].is_folder());
        assert_eq!(entries[2].size, 12);
    }

    #[test]
    fn splits_exec_output_from_errors() {
        let stream = [&[1, 0, 0, 0, 0, 0, 0, 2][..], b"ok", &[2, 0, 0, 0, 0, 0, 0, 3], b"bad"].concat();
        assert_eq!(demultiplex(&stream), (b"ok".to_vec(), b"bad".to_vec()));
    }

    #[test]
    fn finds_parent_folders() {
        assert_eq!(parent_folder("/etc/apk"), "/etc");
        assert_eq!(parent_folder("/etc"), "/");
        assert_eq!(parent_folder("/"), "/");
    }
}
