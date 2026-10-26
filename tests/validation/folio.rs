use folt_core::folio_dir::{self, Inspection};
use folt_core::object::{ObjectError, ObjectKind, hash_blob};
use folt_core::store::StoreError;
use proptest::prelude::*;

use crate::common::TempHome;
use crate::support::TestFolio;

#[test]
fn created_folio_inspects_and_validates() {
    let home = TempHome::new();
    let lock = home.lock();
    let folio = TestFolio::new(&home, &lock);
    let row = folio.registry.folio_row(folio.folio.id()).unwrap().unwrap();
    assert_eq!(row.format, folio.format);
    assert!(matches!(
        folio_dir::inspect(
            home.path(),
            folio.folio.path().file_name().unwrap(),
            Some(&row)
        ),
        Inspection::Valid(_)
    ));
    assert_eq!(
        folio_dir::validate(home.path(), &row).unwrap().id(),
        folio.folio.id()
    );
}

#[test]
fn store_round_trip_deduplicates() {
    let home = TempHome::new();
    let lock = home.lock();
    let folio = TestFolio::new(&home, &lock);
    let first = folio.objects.put(&lock, ObjectKind::Blob, b"data").unwrap();
    assert_eq!(
        folio.objects.put(&lock, ObjectKind::Blob, b"data").unwrap(),
        first
    );
    assert_eq!(
        folio.objects.get(first, ObjectKind::Blob, 4).unwrap(),
        b"data"
    );
    assert_eq!(
        std::fs::read_dir(folio.folio.path().join("blobs"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn missing_blob_returns_io_error() {
    let home = TempHome::new();
    let lock = home.lock();
    let folio = TestFolio::new(&home, &lock);
    assert!(
        matches!(folio.objects.get(hash_blob(b"missing"), ObjectKind::Blob, 100), Err(StoreError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound)
    );
}

#[test]
fn corrupted_blob_is_refused() {
    let home = TempHome::new();
    let lock = home.lock();
    let folio = TestFolio::new(&home, &lock);
    let id = folio.objects.put(&lock, ObjectKind::Blob, b"data").unwrap();
    // Valid framing and length, but different content under the original hash.
    std::fs::write(
        folio.folio.path().join("blobs").join(format!("{id}.blob")),
        b"\0blob 4\0evil",
    )
    .unwrap();
    assert!(matches!(
        folio.objects.get(id, ObjectKind::Blob, 100),
        Err(StoreError::Object(ObjectError::HashMismatch))
    ));
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 16, rng_seed: proptest::test_runner::RngSeed::Fixed(97), failure_persistence: None, ..ProptestConfig::default() })]
    #[test]
    fn arbitrary_small_blobs_round_trip(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        let home = TempHome::new(); let lock = home.lock();
        let folio = TestFolio::new(&home, &lock);
        let id = folio.objects.put(&lock, ObjectKind::Blob, &bytes).unwrap();
        prop_assert_eq!(id, hash_blob(&bytes));
        prop_assert_eq!(folio.objects.get(id, ObjectKind::Blob, 256).unwrap(), bytes);
    }
}
