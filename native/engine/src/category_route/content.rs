//! Bounded, local format facts for archive callbacks; no names or response MIME enter detection.

mod zip_guard;

use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use file_format::{FileFormat, Kind};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::model::ArchiveDetectedType;

const MAX_READ_BYTES: usize = 1024 * 1024;
const MAX_IO_OPERATIONS: usize = 8192;
const MAX_PARSE_TIME: Duration = Duration::from_millis(250);
const HEADER_BYTES: usize = 4096;
static CONTENT_SLOTS: Semaphore = Semaphore::const_new(2);

pub(super) struct FileFacts {
    pub header_hex: Option<String>,
    pub detected_type: Option<ArchiveDetectedType>,
}

pub(super) async fn inspect(path: PathBuf, cancel: CancellationToken) -> io::Result<FileFacts> {
    let permit = CONTENT_SLOTS.acquire().await.map_err(io::Error::other)?;
    tokio::task::spawn_blocking(move || {
        // Keep the slot until the blocking operation actually exits, including on cancellation.
        let _permit = permit;
        let file = std::fs::File::open(path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::other("archive artifact is not a regular file"));
        }
        inspect_reader(file, cancel)
    })
    .await
    .map_err(io::Error::other)?
}

fn inspect_reader(reader: impl Read + Seek, cancel: CancellationToken) -> io::Result<FileFacts> {
    // The budget wraps the actual reader so nested library buffers cannot amplify physical I/O.
    let mut reader = BufReader::new(BudgetReader::new(reader, cancel));
    let head = reader.fill_buf()?;
    let head = head
        .get(..head.len().min(HEADER_BYTES))
        .ok_or_else(|| io::Error::other("invalid archive header window"))?;
    let header_hex = (!head.is_empty()).then(|| hex::encode(head));
    let is_zip = [b"PK\x03\x04", b"PK\x05\x06", b"PK\x07\x08"]
        .iter()
        .any(|signature| head.starts_with(*signature));
    let format = if is_zip {
        if zip_guard::can_refine(&mut reader)? {
            reader.rewind()?;
            FileFormat::from_reader(&mut reader)
        } else {
            Ok(FileFormat::Zip)
        }
    } else {
        FileFormat::from_reader(&mut reader)
    };
    // file-format can swallow a deep-reader error while trying generic recognition. Preserve
    // our first I/O/budget failure rather than treating its fallback as trustworthy evidence.
    if let Some(error) = reader.get_mut().failure.take() {
        return Err(error);
    }
    reader.get_mut().check()?;
    let format = format?;
    let detected_type = match format {
        FileFormat::ArbitraryBinaryData | FileFormat::Empty => None,
        _ => Some(ArchiveDetectedType {
            kind: kind_name(format.kind()),
            mime: format.media_type().to_owned(),
            extension: format.extension().to_owned(),
        }),
    };
    Ok(FileFacts {
        header_hex,
        detected_type,
    })
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Archive => "archive",
        Kind::Audio => "audio",
        Kind::Compressed => "compressed",
        Kind::Database => "database",
        Kind::Diagram => "diagram",
        Kind::Disk => "disk",
        Kind::Document => "document",
        Kind::Ebook => "ebook",
        Kind::Executable => "executable",
        Kind::Font => "font",
        Kind::Formula => "formula",
        Kind::Geospatial => "geospatial",
        Kind::Image => "image",
        Kind::Metadata => "metadata",
        Kind::Model => "model",
        Kind::Package => "package",
        Kind::Playlist => "playlist",
        Kind::Presentation => "presentation",
        Kind::Rom => "rom",
        Kind::Spreadsheet => "spreadsheet",
        Kind::Subtitle => "subtitle",
        Kind::Video => "video",
        _ => "other",
    }
}

struct BudgetReader<R> {
    reader: R,
    bytes_left: usize,
    operations_left: usize,
    started: Instant,
    cancel: CancellationToken,
    failure: Option<io::Error>,
}

impl<R> BudgetReader<R> {
    fn new(reader: R, cancel: CancellationToken) -> Self {
        Self {
            reader,
            bytes_left: MAX_READ_BYTES,
            operations_left: MAX_IO_OPERATIONS,
            started: Instant::now(),
            cancel,
            failure: None,
        }
    }

    fn fail(&mut self, error: io::Error) -> io::Error {
        let returned = io::Error::new(error.kind(), error.to_string());
        if self.failure.is_none() {
            self.failure = Some(error);
        }
        returned
    }

    fn check(&mut self) -> io::Result<()> {
        if self.failure.is_some() {
            return Err(io::Error::other("archive content reader already stopped"));
        }
        if self.cancel.is_cancelled() {
            return Err(self.fail(io::Error::other("archive content recognition cancelled")));
        }
        if self.operations_left == 0 || self.started.elapsed() >= MAX_PARSE_TIME {
            return Err(self.fail(io::Error::other(
                "archive content recognition budget exceeded",
            )));
        }
        self.operations_left -= 1;
        Ok(())
    }
}

impl<R: Read> Read for BudgetReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.check()?;
        if self.bytes_left == 0 {
            return Err(self.fail(io::Error::other("archive content read limit exceeded")));
        }
        let length = buf.len().min(self.bytes_left);
        match self.reader.read(
            buf.get_mut(..length)
                .ok_or_else(|| io::Error::other("invalid content read buffer"))?,
        ) {
            Ok(count) => {
                self.bytes_left -= count;
                Ok(count)
            }
            Err(error) => Err(self.fail(error)),
        }
    }
}

impl<R: Seek> Seek for BudgetReader<R> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.check()?;
        self.reader.seek(position).map_err(|error| self.fail(error))
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read, Seek, SeekFrom, Write};

    use tokio_util::sync::CancellationToken;

    use super::{BudgetReader, inspect_reader};

    #[test]
    fn archive_content_recognizes_bytes_and_keeps_unknowns_unknown() {
        for (bytes, expected) in [
            (
                b"\x89PNG\r\n\x1a\n".as_slice(),
                Some(("image", "image/png", "png")),
            ),
            (
                b"%PDF-1.7\n".as_slice(),
                Some(("document", "application/pdf", "pdf")),
            ),
            (b"\0\0\0\0\0\0\0\0".as_slice(), None),
            (b"".as_slice(), None),
        ] {
            let facts = inspect_reader(Cursor::new(bytes), CancellationToken::new()).unwrap();
            assert_eq!(
                facts.detected_type.as_ref().map(|value| (
                    value.kind,
                    value.mime.as_str(),
                    value.extension.as_str()
                )),
                expected
            );
            assert_eq!(
                facts.header_hex,
                (!bytes.is_empty()).then(|| hex::encode(bytes))
            );
        }
    }

    #[test]
    fn archive_content_distinguishes_zip_subtypes_without_trusting_allocation_sizes() {
        for (entry, payload, expected) in [
            ("data.bin", b"content".as_slice(), ("archive", "zip")),
            (
                "mimetype",
                b"application/epub+zip".as_slice(),
                ("ebook", "epub"),
            ),
            (
                "word/document.xml",
                b"<document/>".as_slice(),
                ("document", "docx"),
            ),
        ] {
            let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file(entry, options).unwrap();
            zip.write_all(payload).unwrap();
            let bytes = zip.finish().unwrap().into_inner();
            let facts = inspect_reader(Cursor::new(&bytes), CancellationToken::new()).unwrap();
            let detected = facts.detected_type.unwrap();
            assert_eq!((detected.kind, detected.extension.as_str()), expected);
            if entry == "mimetype" {
                let mut hostile = bytes;
                let start = hostile
                    .windows(4)
                    .position(|bytes| bytes == b"PK\x01\x02")
                    .unwrap();
                hostile.get_mut(start + 20..start + 28).unwrap().fill(0xff);
                let facts = inspect_reader(Cursor::new(hostile), CancellationToken::new()).unwrap();
                let detected = facts.detected_type.unwrap();
                assert_eq!(
                    (detected.kind, detected.extension.as_str()),
                    ("archive", "zip")
                );
            }
        }
    }

    #[test]
    fn archive_content_limits_reads_and_seeks_without_hiding_exhaustion() {
        let mut reader = BudgetReader::new(Cursor::new(vec![b'a'; 256]), CancellationToken::new());
        reader.bytes_left = 64;
        let mut output = Vec::new();
        assert!(reader.read_to_end(&mut output).is_err());
        assert_eq!(output, vec![b'a'; 64]);
        assert!(reader.failure.is_some());

        let mut reader = BudgetReader::new(Cursor::new([]), CancellationToken::new());
        reader.operations_left = 1;
        assert_eq!(reader.seek(SeekFrom::Start(0)).unwrap(), 0);
        assert!(reader.seek(SeekFrom::Start(0)).is_err());
        assert!(reader.failure.is_some());
    }

    #[test]
    fn archive_content_cancelled_work_does_not_return_format_evidence() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(inspect_reader(Cursor::new(b"%PDF-1.7"), cancel).is_err());
    }
}
