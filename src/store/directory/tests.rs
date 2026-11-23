//! Task 4.3: the directory store on a temporary directory.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tempfile::TempDir;

use super::*;
use crate::store::testing::issue;

const NAME: &str = "wildcard-example-com";

fn store() -> (TempDir, DirectoryStore) {
    let directory = TempDir::new().unwrap();
    let store = DirectoryStore::new(directory.path());
    (directory, store)
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn files_in(path: &Path) -> BTreeSet<String> {
    fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

// certificate-stores, "Store types", scenario "Directory store", and
// "Directory layout", scenario "PEM files".
#[tokio::test]
async fn pem_format_writes_fullchain_and_owner_only_key_under_the_certificate_name() {
    let (directory, store) = store();
    let certificate = issue(&["*.example.com", "example.com"]);

    store
        .put(NAME, &certificate, Format::Pem)
        .await
        .expect("written");

    let folder = directory.path().join(NAME);
    assert_eq!(
        files_in(&folder),
        BTreeSet::from(["fullchain.pem".into(), "privkey.pem".into()])
    );
    assert_eq!(
        fs::read_to_string(folder.join("fullchain.pem")).unwrap(),
        encode::fullchain_pem(&certificate)
    );
    assert_eq!(
        fs::read_to_string(folder.join("privkey.pem")).unwrap(),
        *encode::private_key_pem(&certificate)
    );
    assert_eq!(
        mode(&folder.join("privkey.pem")),
        0o600,
        "readable only by its owner"
    );
    assert_eq!(mode(&folder.join("fullchain.pem")), 0o644);
}

#[tokio::test]
async fn pfx_format_writes_one_owner_only_bundle() {
    let (directory, store) = store();
    let certificate = issue(&["example.com"]);

    store
        .put(NAME, &certificate, Format::Pfx)
        .await
        .expect("written");

    let folder = directory.path().join(NAME);
    assert_eq!(
        files_in(&folder),
        BTreeSet::from(["certificate.pfx".into()])
    );
    assert_eq!(
        mode(&folder.join("certificate.pfx")),
        0o600,
        "the bundle holds the private key"
    );
    let bundle = fs::read(folder.join("certificate.pfx")).unwrap();
    assert_eq!(
        encode::certificate_from_pkcs12(&bundle).as_ref(),
        Ok(certificate.leaf())
    );
}

// certificate-stores, "Reading the current certificate", scenario "Nothing stored".
#[tokio::test]
async fn store_that_holds_nothing_reports_no_certificate() {
    let (directory, store) = store();
    for format in [Format::Pem, Format::Pfx] {
        assert_eq!(
            store.current(NAME, format).await,
            Ok(None),
            "no folder for the name"
        );
    }
    fs::create_dir(directory.path().join(NAME)).unwrap();
    for format in [Format::Pem, Format::Pfx] {
        assert_eq!(
            store.current(NAME, format).await,
            Ok(None),
            "an empty folder"
        );
    }
    let missing = DirectoryStore::new(directory.path().join("does-not-exist"));
    assert_eq!(
        missing.current(NAME, Format::Pem).await,
        Ok(None),
        "no store directory yet"
    );
}

// certificate-stores, "Reading the current certificate", scenario "Certificate present".
#[tokio::test]
async fn written_certificate_is_read_back_in_both_formats() {
    for format in [Format::Pem, Format::Pfx] {
        let (_directory, store) = store();
        let certificate = issue(&["*.example.com", "example.com"]);
        store
            .put(NAME, &certificate, format)
            .await
            .expect("written");

        let current = store.current(NAME, format).await.expect("readable");
        assert_eq!(current.as_ref(), Some(certificate.leaf()), "{format}");
        assert_eq!(store.current("other", format).await, Ok(None));
    }
}

#[tokio::test]
async fn certificate_is_held_in_the_format_it_was_written_in() {
    let (_directory, store) = store();
    let certificate = issue(&["example.com"]);
    store
        .put(NAME, &certificate, Format::Pem)
        .await
        .expect("written");
    assert_eq!(store.current(NAME, Format::Pfx).await, Ok(None));
}

// certificate-stores, "Adoption of existing certificates", scenario "Certificate from
// the previous tool": the store reports it; the run that leaves it alone is
// `plain_run_evaluates_every_certificate_and_exits_with_0_when_none_is_due` in tests/cli.rs.
#[tokio::test]
async fn files_placed_by_another_tool_are_read_like_its_own() {
    let (directory, store) = store();
    let certificate = issue(&["example.com"]);
    let folder = directory.path().join(NAME);
    fs::create_dir(&folder).unwrap();
    fs::write(
        folder.join("fullchain.pem"),
        encode::fullchain_pem(&certificate),
    )
    .unwrap();
    fs::write(
        folder.join("privkey.pem"),
        encode::private_key_pem(&certificate).as_bytes(),
    )
    .unwrap();

    let current = store.current(NAME, Format::Pem).await.expect("readable");
    assert_eq!(current.as_ref(), Some(certificate.leaf()));
}

// certificate-stores, "Reading the current certificate", scenario "Store cannot be read".
#[tokio::test]
async fn content_that_is_not_a_certificate_is_an_error() {
    let (directory, store) = store();
    let folder = directory.path().join(NAME);
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("fullchain.pem"), "not a certificate").unwrap();
    fs::write(folder.join("certificate.pfx"), b"not a bundle").unwrap();

    for (format, file) in [
        (Format::Pem, "fullchain.pem"),
        (Format::Pfx, "certificate.pfx"),
    ] {
        let error = store.current(NAME, format).await.expect_err("unreadable");
        assert_eq!(error.class(), "permanent");
        let message = error.to_string();
        assert!(
            message.contains(file) && message.contains("is unreadable"),
            "{message}"
        );
    }
}

#[tokio::test]
async fn store_path_that_is_a_file_is_an_error() {
    let directory = TempDir::new().unwrap();
    let file = directory.path().join("a-file");
    fs::write(&file, "x").unwrap();
    let store = DirectoryStore::new(&file);

    let error = store
        .put(NAME, &issue(&["example.com"]), Format::Pem)
        .await
        .expect_err("not a directory");
    assert_eq!(error.class(), "permanent", "{error}");
    let error = store
        .current(NAME, Format::Pem)
        .await
        .expect_err("not a directory");
    assert_eq!(error.class(), "permanent", "{error}");
}

#[tokio::test]
async fn certificate_without_its_own_key_is_not_held() {
    let (directory, store) = store();
    let first = issue(&["example.com"]);
    let second = issue(&["example.com"]);
    let folder = directory.path().join(NAME);
    store.put(NAME, &first, Format::Pem).await.expect("written");

    // What a run leaves behind that is killed between the two renames: the
    // new key next to the old certificate.
    fs::write(
        folder.join("privkey.pem"),
        encode::private_key_pem(&second).as_bytes(),
    )
    .unwrap();
    assert_eq!(
        store.current(NAME, Format::Pem).await,
        Ok(None),
        "the key is another one"
    );

    fs::write(folder.join("privkey.pem"), "not a key").unwrap();
    assert_eq!(
        store.current(NAME, Format::Pem).await,
        Ok(None),
        "the key is unreadable"
    );

    fs::remove_file(folder.join("privkey.pem")).unwrap();
    assert_eq!(
        store.current(NAME, Format::Pem).await,
        Ok(None),
        "the key is missing"
    );

    // The next run writes a complete pair again.
    store
        .put(NAME, &second, Format::Pem)
        .await
        .expect("written");
    let current = store.current(NAME, Format::Pem).await.expect("readable");
    assert_eq!(current.as_ref(), Some(second.leaf()));
}

#[tokio::test]
async fn renewal_replaces_the_files_and_leaves_nothing_else_behind() {
    let (directory, store) = store();
    let folder = directory.path().join(NAME);
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("README"), "kept").unwrap();
    let first = issue(&["example.com"]);
    let second = issue(&["example.com"]);

    for format in [Format::Pem, Format::Pfx] {
        store.put(NAME, &first, format).await.expect("written");
        store.put(NAME, &second, format).await.expect("replaced");
        let current = store.current(NAME, format).await.expect("readable");
        assert_eq!(current.as_ref(), Some(second.leaf()), "{format}");
    }

    assert_eq!(
        files_in(&folder),
        BTreeSet::from([
            "README".into(),
            "certificate.pfx".into(),
            "fullchain.pem".into(),
            "privkey.pem".into()
        ]),
        "no temporary file is left, and a file the store does not own is untouched"
    );
    assert_eq!(fs::read_to_string(folder.join("README")).unwrap(), "kept");
    assert_eq!(
        mode(&folder.join("privkey.pem")),
        0o600,
        "also after a replacement"
    );
}

// certificate-stores, "Several stores per certificate", scenario "Two stores".
#[tokio::test]
async fn two_stores_hold_the_same_certificate() {
    let (_first_directory, first) = store();
    let (_second_directory, second) = store();
    let certificate = issue(&["example.com"]);

    first
        .put(NAME, &certificate, Format::Pem)
        .await
        .expect("written");
    second
        .put(NAME, &certificate, Format::Pfx)
        .await
        .expect("written");

    assert_eq!(
        first.current(NAME, Format::Pem).await.expect("readable"),
        second.current(NAME, Format::Pfx).await.expect("readable")
    );
}

// certificate-stores, "Directory layout", scenario "Reader during a write".
#[tokio::test(flavor = "multi_thread")]
async fn reader_during_a_write_sees_a_complete_old_or_new_file() {
    let (directory, store) = store();
    let old = issue(&["example.com"]);
    let new = issue(&["example.com"]);
    store.put(NAME, &old, Format::Pem).await.expect("written");
    store.put(NAME, &old, Format::Pfx).await.expect("written");

    let folder = directory.path().join(NAME);
    let complete: Vec<(PathBuf, [Vec<u8>; 2])> = vec![
        (
            folder.join("fullchain.pem"),
            [
                encode::fullchain_pem(&old).into_bytes(),
                encode::fullchain_pem(&new).into_bytes(),
            ],
        ),
        (
            folder.join("privkey.pem"),
            [
                encode::private_key_pem(&old).as_bytes().to_vec(),
                encode::private_key_pem(&new).as_bytes().to_vec(),
            ],
        ),
    ];
    let bundle_path = folder.join("certificate.pfx");

    let writing = Arc::new(AtomicBool::new(true));
    let reader = std::thread::spawn({
        let writing = writing.clone();
        let (old_leaf, new_leaf) = (old.leaf().clone(), new.leaf().clone());
        move || {
            let mut reads = 0_u32;
            while writing.load(Ordering::Relaxed) {
                for (path, versions) in &complete {
                    let content = fs::read(path).expect("the file always exists");
                    assert!(
                        versions.contains(&content),
                        "{} is partly written",
                        path.display()
                    );
                }
                let bundle = fs::read(&bundle_path).expect("the file always exists");
                let leaf = encode::certificate_from_pkcs12(&bundle).expect("a complete bundle");
                assert!(leaf == old_leaf || leaf == new_leaf);
                reads += 1;
            }
            reads
        }
    });

    // Building a bundle is slow next to writing a file, so the bundle is
    // replaced less often than the two PEM files.
    for round in 0..200 {
        let certificate = if round % 2 == 0 { &new } else { &old };
        store
            .put(NAME, certificate, Format::Pem)
            .await
            .expect("written");
        if round % 10 == 0 {
            store
                .put(NAME, certificate, Format::Pfx)
                .await
                .expect("written");
        }
    }
    writing.store(false, Ordering::Relaxed);

    let reads = reader.join().expect("no read saw a partly written file");
    assert!(
        reads > 0,
        "the reader ran while the files were being replaced"
    );
}

#[test]
fn filesystem_errors_are_classified() {
    use io::ErrorKind::*;

    for kind in [
        PermissionDenied,
        ReadOnlyFilesystem,
        StorageFull,
        NotADirectory,
        NotFound,
    ] {
        assert_eq!(
            classify("writing", &io::Error::from(kind)).class(),
            "permanent",
            "{kind:?}"
        );
    }
    for kind in [Interrupted, TimedOut, WouldBlock, Other] {
        assert!(
            classify("writing", &io::Error::from(kind)).is_transient(),
            "{kind:?}"
        );
    }
}
