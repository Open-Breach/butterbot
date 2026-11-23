//! The `directory` certificate store.
//!
//! A certificate named `<name>` lives in `<path>/<name>/`:
//!
//! - in `pem` format as `fullchain.pem` (the certificate and its chain) and
//!   `privkey.pem` (the private key);
//! - in `pfx` format as `certificate.pfx` (all three in one bundle).
//!
//! Files that hold the private key are readable only by their owner. A file is
//! replaced by writing a temporary file next to it and renaming it over the
//! old one, so a reader sees the complete old file or the complete new one.
//!
//! The two files of the `pem` format are renamed one right after the other,
//! which is as close together as a filesystem allows. If the program is killed
//! exactly between the two, the key file and the certificate file do not
//! belong together. The store reports such a pair as holding no certificate,
//! so the next run issues and writes a complete pair.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tempfile::NamedTempFile;

use super::encode;
use super::{CertStore, Format, IssuedCert, StoredCert};
use crate::{Error, Result};

pub const FULLCHAIN_FILE: &str = "fullchain.pem";
pub const PRIVATE_KEY_FILE: &str = "privkey.pem";
pub const BUNDLE_FILE: &str = "certificate.pfx";

/// File mode of a file that holds a private key: readable and writable by its
/// owner only.
const PRIVATE: u32 = 0o600;

/// File mode of the certificate file, which holds nothing secret.
const PUBLIC: u32 = 0o644;

/// A certificate store that is a directory on the filesystem.
#[derive(Debug, Clone)]
pub struct DirectoryStore {
    path: PathBuf,
}

impl DirectoryStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn directory_of(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

#[async_trait]
impl CertStore for DirectoryStore {
    async fn current(&self, name: &str, format: Format) -> Result<Option<StoredCert>> {
        let directory = self.directory_of(name);
        blocking(move || read(&directory, format)).await
    }

    async fn put(&self, name: &str, certificate: &IssuedCert, format: Format) -> Result<()> {
        let directory = self.directory_of(name);
        let files: Vec<(&'static str, u32, zeroize::Zeroizing<Vec<u8>>)> = match format {
            // The key file comes first: see `write`.
            Format::Pem => vec![
                (
                    PRIVATE_KEY_FILE,
                    PRIVATE,
                    encode::private_key_pem(certificate)
                        .as_bytes()
                        .to_vec()
                        .into(),
                ),
                (
                    FULLCHAIN_FILE,
                    PUBLIC,
                    encode::fullchain_pem(certificate).into_bytes().into(),
                ),
            ],
            Format::Pfx => {
                let bundle = encode::pkcs12(certificate, name).map_err(Error::permanent)?;
                vec![(BUNDLE_FILE, PRIVATE, bundle)]
            }
        };
        blocking(move || write(&directory, &files)).await
    }
}

/// Runs filesystem work off the async threads.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|error| Err(Error::permanent(format!("filesystem work failed: {error}"))))
}

fn read(directory: &Path, format: Format) -> Result<Option<StoredCert>> {
    let unreadable = |file: &str, problem: String| {
        Error::permanent(format!(
            "{} is unreadable: {problem}",
            directory.join(file).display()
        ))
    };
    match format {
        Format::Pem => {
            let Some(fullchain) = read_file(&directory.join(FULLCHAIN_FILE))? else {
                return Ok(None);
            };
            let text = String::from_utf8(fullchain)
                .map_err(|_| unreadable(FULLCHAIN_FILE, "not text".to_owned()))?;
            let certificate = encode::certificate_from_pem(&text)
                .map_err(|problem| unreadable(FULLCHAIN_FILE, problem))?;
            // A certificate without its key is of no use to a consumer, and a
            // key that belongs to another certificate is what a killed run
            // can leave behind. Either way the store does not hold the
            // certificate.
            let Some(key) = read_file(&directory.join(PRIVATE_KEY_FILE))? else {
                return Ok(None);
            };
            let key = String::from_utf8(key)
                .ok()
                .and_then(|text| encode::private_key_from_pem(&text).ok());
            let complete = key.is_some_and(|key| encode::key_belongs_to(&certificate, &key));
            Ok(complete.then_some(certificate))
        }
        Format::Pfx => {
            let Some(bundle) = read_file(&directory.join(BUNDLE_FILE))? else {
                return Ok(None);
            };
            encode::certificate_from_pkcs12(&bundle)
                .map(Some)
                .map_err(|problem| unreadable(BUNDLE_FILE, problem))
        }
    }
}

/// The content of a file, or `None` when the file or its directory does not exist.
fn read_file(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(classify(&format!("reading {}", path.display()), &error)),
    }
}

/// Writes `files` into `directory`, each as (file name, mode, content).
///
/// Every file is first written completely under a temporary name. Only then
/// are the temporary files renamed to their names, one right after the other,
/// in the order given.
fn write(directory: &Path, files: &[(&str, u32, zeroize::Zeroizing<Vec<u8>>)]) -> Result<()> {
    let failed = |action: &str, error: &io::Error| {
        classify(&format!("{action} in {}", directory.display()), error)
    };
    fs::create_dir_all(directory).map_err(|error| failed("creating the directory", &error))?;

    let mut prepared = Vec::with_capacity(files.len());
    for (name, mode, content) in files {
        // A temporary file is created readable by its owner only, so a key is
        // never readable by others, not even while it is being written.
        let mut file = tempfile::Builder::new()
            .prefix(&format!(".{name}."))
            .suffix(".tmp")
            .tempfile_in(directory)
            .map_err(|error| failed("creating a temporary file", &error))?;
        file.write_all(content)
            .map_err(|error| failed("writing a temporary file", &error))?;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(*mode))
            .and_then(|()| file.as_file().sync_all())
            .map_err(|error| failed("writing a temporary file", &error))?;
        prepared.push((file, directory.join(name)));
    }
    for (file, target) in prepared {
        persist(file, &target)
            .map_err(|error| failed(&format!("replacing {}", target.display()), &error))?;
    }
    // Make the renames durable. Not every filesystem can do this for a
    // directory, and the files are in place either way.
    let _ = fs::File::open(directory).and_then(|directory| directory.sync_all());
    Ok(())
}

fn persist(file: NamedTempFile, target: &Path) -> io::Result<()> {
    file.persist(target).map(drop).map_err(|error| error.error)
}

/// Maps a filesystem error to an error class. What cannot get better by
/// trying again is permanent; anything else may be a passing failure of the
/// storage behind the path and is transient.
fn classify(action: &str, error: &io::Error) -> Error {
    use io::ErrorKind::*;

    let message = format!("{action}: {error}");
    match error.kind() {
        NotFound | PermissionDenied | ReadOnlyFilesystem | StorageFull | QuotaExceeded
        | InvalidInput | InvalidFilename | NotADirectory | IsADirectory | AlreadyExists
        | Unsupported => Error::permanent(message),
        _ => Error::transient(message),
    }
}

#[cfg(test)]
mod tests;
