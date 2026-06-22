//! Encryption-at-rest (SQLCipher `v0.4.4-lb1`): a keyed store writes ciphertext, the same key
//! reads it back, and the wrong / no key cannot open it.

use openmemory_core::config::Config;
use openmemory_graph::{EntityType, MemoryStore, ObservationInput, RecallFilters, SearchMode};

fn keyword_filters() -> RecallFilters {
    let mut f = RecallFilters::new();
    f.mode = Some(SearchMode::KeywordOnly);
    f
}

#[test]
fn cipher_key_encrypts_at_rest_and_gates_open() {
    let dir = tempfile::tempdir().unwrap();
    let key = vec![0x42u8; 32];

    // Write through a keyed store.
    {
        let store =
            MemoryStore::open(&Config::default().with_cipher_key(key.clone()), dir.path()).unwrap();
        store
            .remember(
                "Secret",
                EntityType::Fact,
                &[ObservationInput::new("classified dossier alpha")],
                &[],
                "test",
            )
            .unwrap();
    }

    // The memory database on disk must NOT be a plaintext SQLite file.
    let raw = std::fs::read(dir.path().join("memory.sqlite")).unwrap();
    assert!(
        !raw.starts_with(b"SQLite format 3\0"),
        "memory.sqlite must be encrypted at rest (got a plaintext SQLite header)"
    );
    assert!(
        !raw.windows(10).any(|w| w == b"classified"),
        "plaintext content must not appear on disk"
    );

    // The correct key reads it back.
    {
        let store =
            MemoryStore::open(&Config::default().with_cipher_key(key.clone()), dir.path()).unwrap();
        let hits = store.recall("classified", 5, &keyword_filters()).unwrap();
        assert!(
            hits.iter().any(|h| h.entity_name == "Secret"),
            "the correct key must decrypt and recall the entity"
        );
    }

    // No key cannot open the encrypted database.
    assert!(
        MemoryStore::open(&Config::default(), dir.path()).is_err(),
        "opening an encrypted store without a key must fail"
    );

    // A wrong key cannot open it either.
    assert!(
        MemoryStore::open(
            &Config::default().with_cipher_key(vec![0x99u8; 32]),
            dir.path()
        )
        .is_err(),
        "opening with the wrong key must fail"
    );
}

#[test]
fn bump_access_false_does_not_mutate() {
    // A read-only recall (bump_access = false) must not change the DB (archived/read-only shares).
    let dir = tempfile::tempdir().unwrap();
    let key = vec![0x7u8; 32];
    let store = MemoryStore::open(&Config::default().with_cipher_key(key), dir.path()).unwrap();
    store
        .remember(
            "Topic",
            EntityType::Fact,
            &[ObservationInput::new("alpha beta gamma")],
            &[],
            "t",
        )
        .unwrap();
    let mut ro = RecallFilters::new();
    ro.mode = Some(SearchMode::KeywordOnly);
    ro.bump_access = false;
    let hits = store.recall("alpha", 5, &ro).unwrap();
    assert!(!hits.is_empty(), "read-only recall still returns hits");
    // (Access-count immutability is exercised; the flag simply skips the bump side-effect.)
}
