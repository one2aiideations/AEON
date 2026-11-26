//! Artifacts: the documents, sheets, code and media a conversation produces. Versions are
//! immutable and content-hashed.

use crate::error::{hex, Error, Result};
use crate::ids::IdGen;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Broad class of an artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Word-processing and text documents.
    Document,
    /// Spreadsheets and tables.
    Spreadsheet,
    /// Slides.
    Presentation,
    /// Source code.
    Code,
    /// Images.
    Image,
    /// Audio.
    Audio,
    /// Video.
    Video,
    /// JSON, XML, YAML and similar.
    Data,
    /// Anything else.
    Other,
}

impl Kind {
    /// Classify by media type.
    #[must_use]
    pub fn from_media_type(mt: &str) -> Self {
        let mt = mt
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        match mt.as_str() {
            "application/pdf" | "text/markdown" | "text/plain" | "application/rtf" => {
                Self::Document
            }
            "application/msword" => Self::Document,
            m if m
                .starts_with("application/vnd.openxmlformats-officedocument.wordprocessingml") =>
            {
                Self::Document
            }
            "text/csv" | "application/vnd.ms-excel" => Self::Spreadsheet,
            m if m.starts_with("application/vnd.openxmlformats-officedocument.spreadsheetml") => {
                Self::Spreadsheet
            }
            "application/vnd.ms-powerpoint" => Self::Presentation,
            m if m.starts_with("application/vnd.openxmlformats-officedocument.presentationml") => {
                Self::Presentation
            }
            "application/json" | "application/xml" | "text/xml" | "application/yaml"
            | "text/yaml" => Self::Data,
            "text/x-rust"
            | "text/x-python"
            | "text/x-c"
            | "text/javascript"
            | "application/javascript" => Self::Code,
            m if m.starts_with("image/") => Self::Image,
            m if m.starts_with("audio/") => Self::Audio,
            m if m.starts_with("video/") => Self::Video,
            _ => Self::Other,
        }
    }

    /// Classify by file extension (without the dot).
    #[must_use]
    pub fn from_extension(ext: &str) -> Self {
        match ext.to_ascii_lowercase().as_str() {
            "pdf" | "docx" | "doc" | "md" | "txt" | "rtf" => Self::Document,
            "xlsx" | "xls" | "csv" | "tsv" => Self::Spreadsheet,
            "pptx" | "ppt" => Self::Presentation,
            "json" | "xml" | "yaml" | "yml" | "toml" => Self::Data,
            "rs" | "py" | "c" | "h" | "js" | "ts" | "go" | "sh" => Self::Code,
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" => Self::Image,
            "wav" | "mp3" | "flac" | "ogg" => Self::Audio,
            "mp4" | "mkv" | "webm" | "mov" => Self::Video,
            _ => Self::Other,
        }
    }
}

/// Metadata of one stored version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    /// Identifier of this version.
    pub id: String,
    /// Name shared by all versions.
    pub name: String,
    /// Classification.
    pub kind: Kind,
    /// Media type.
    pub media_type: String,
    /// Version number from one.
    pub version: u32,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Content length in bytes.
    pub len: usize,
    /// Logical creation time.
    pub tick: u64,
    /// Previous version's id.
    pub parent: Option<String>,
}

/// Store of artifacts for one workspace.
#[derive(Debug, Default)]
pub struct ArtifactStore {
    items: BTreeMap<String, (Artifact, Vec<u8>)>,
    by_name: BTreeMap<String, Vec<String>>,
}

impl ArtifactStore {
    /// Empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Store a new version of `name`.
    ///
    /// # Errors
    /// Returns [`Error::Invalid`] for a blank name or media type.
    pub fn put(
        &mut self,
        name: &str,
        media_type: &str,
        bytes: &[u8],
        tick: u64,
        ids: &mut IdGen,
    ) -> Result<Artifact> {
        if name.trim().is_empty() || media_type.trim().is_empty() {
            return Err(Error::Invalid(
                "artifact name and media type are required".into(),
            ));
        }
        let versions = self.by_name.entry(name.to_string()).or_default();
        let a = Artifact {
            id: ids.next("art"),
            name: name.into(),
            kind: Kind::from_media_type(media_type),
            media_type: media_type.into(),
            version: u32::try_from(versions.len())
                .unwrap_or(u32::MAX)
                .saturating_add(1),
            sha256: hex(&Sha256::digest(bytes)),
            len: bytes.len(),
            tick,
            parent: versions.last().cloned(),
        };
        versions.push(a.id.clone());
        self.items.insert(a.id.clone(), (a.clone(), bytes.to_vec()));
        Ok(a)
    }

    /// Fetch a version and its bytes, verifying the hash.
    ///
    /// # Errors
    /// [`Error::NotFound`] for an unknown id, [`Error::Invalid`] if the stored bytes no longer
    /// match their hash.
    pub fn get(&self, id: &str) -> Result<(&Artifact, &[u8])> {
        let (a, b) = self
            .items
            .get(id)
            .ok_or_else(|| Error::NotFound(format!("artifact {id}")))?;
        if hex(&Sha256::digest(b)) != a.sha256 {
            return Err(Error::Invalid(format!(
                "artifact {id} content hash mismatch"
            )));
        }
        Ok((a, b))
    }

    /// Newest version of `name`.
    #[must_use]
    pub fn latest(&self, name: &str) -> Option<&Artifact> {
        let id = self.by_name.get(name)?.last()?;
        self.items.get(id).map(|(a, _)| a)
    }

    /// The newest version of every name, sorted by name.
    #[must_use]
    pub fn latest_of_each(&self) -> Vec<&Artifact> {
        self.by_name.keys().filter_map(|n| self.latest(n)).collect()
    }

    /// All versions of `name`, oldest first.
    #[must_use]
    pub fn versions(&self, name: &str) -> Vec<&Artifact> {
        self.by_name
            .get(name)
            .map(|v| {
                v.iter()
                    .filter_map(|id| self.items.get(id).map(|(a, _)| a))
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        assert_eq!(Kind::from_extension("XLSX"), Kind::Spreadsheet);
        assert_eq!(Kind::from_extension("rs"), Kind::Code);
        assert_eq!(Kind::from_media_type("image/png"), Kind::Image);
        assert_eq!(
            Kind::from_media_type("text/csv; charset=utf-8"),
            Kind::Spreadsheet
        );
        assert_eq!(
            Kind::from_media_type(
                "application/vnd.openxmlformats-officedocument.presentationml.presentation"
            ),
            Kind::Presentation
        );
        assert_eq!(Kind::from_media_type("application/x-unknown"), Kind::Other);
    }

    #[test]
    fn versions_chain_and_verify() {
        let (mut s, mut ids) = (ArtifactStore::new(), IdGen::new());
        let a = s
            .put("report.md", "text/markdown", b"v1", 1, &mut ids)
            .unwrap();
        let b = s
            .put("report.md", "text/markdown", b"v2!", 2, &mut ids)
            .unwrap();
        assert_eq!((a.version, b.version), (1, 2));
        assert_eq!(b.parent.as_deref(), Some(a.id.as_str()));
        assert_eq!(s.latest("report.md").unwrap().id, b.id);
        assert_eq!(s.versions("report.md").len(), 2);
        assert_eq!(s.get(&a.id).unwrap().1, b"v1");
        assert_ne!(a.sha256, b.sha256);
        assert!(s.get("nope").is_err());
        assert!(s.put(" ", "text/plain", b"", 3, &mut ids).is_err());
    }

    #[test]
    fn corruption_is_detected() {
        let (mut s, mut ids) = (ArtifactStore::new(), IdGen::new());
        let a = s.put("x", "text/plain", b"abc", 1, &mut ids).unwrap();
        s.items.get_mut(&a.id).unwrap().1[0] = b'z';
        assert!(matches!(s.get(&a.id), Err(Error::Invalid(_))));
    }
}
