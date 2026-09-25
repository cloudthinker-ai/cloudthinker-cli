use super::*;
use flate2::{Compression, write::GzEncoder};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::thread;
use std::time::{Duration, SystemTime};
use tar::{Builder, Header};

fn valid_archive() -> Vec<u8> {
    archive_from_entries(&[
        (
            "SKILL.md",
            b"---\nname: demo\ndescription: Demo\n---\n".as_slice(),
        ),
        ("scripts/validate.ts", b"console.log('ok')\n".as_slice()),
    ])
}

fn archive_from_entries(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut compressed = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut builder = Builder::new(&mut compressed);
        for &(path, body) in entries {
            let mut header = Header::new_gnu();
            header.set_path(path).unwrap();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append(&header, body).unwrap();
        }
        builder.finish().unwrap();
    }
    compressed.finish().unwrap()
}

fn large_archive() -> (Vec<u8>, Vec<u8>) {
    let mut payload = Vec::with_capacity(330_000);
    let mut state = 0x1234_5678_u32;
    for _ in 0..330_000 {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        payload.push((state >> 24) as u8);
    }
    let archive = archive_from_entries(&[
        (
            "SKILL.md",
            b"---\nname: demo\ndescription: Demo\n---\n".as_slice(),
        ),
        ("scripts/payload.bin", payload.as_slice()),
    ]);
    (archive, payload)
}

fn missing_skill_archive() -> Vec<u8> {
    archive_from_entries(&[("scripts/validate.ts", b"console.log('ok')\n")])
}

fn symlink_archive() -> Vec<u8> {
    let mut compressed = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut builder = Builder::new(&mut compressed);
        let mut header = Header::new_gnu();
        header.set_path("scripts/validate.ts").unwrap();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_link_name("SKILL.md").unwrap();
        header.set_size(0);
        header.set_cksum();
        builder.append(&header, &[][..]).unwrap();
        builder.finish().unwrap();
    }
    compressed.finish().unwrap()
}

fn chunks(bytes: &[u8]) -> Vec<SkillBundleChunk> {
    let digest = format!("{:x}", Sha256::digest(bytes));
    let count = bytes.len().div_ceil(CHUNK_BYTES);
    bytes
        .chunks(CHUNK_BYTES)
        .enumerate()
        .map(|(index, chunk)| SkillBundleChunk {
            kind: KIND.into(),
            digest: digest.clone(),
            chunk_index: index as u64,
            chunk_count: count as u64,
            archive_size: bytes.len() as u64,
            content_base64: base64::engine::general_purpose::STANDARD.encode(chunk),
        })
        .collect()
}

fn create_staging_state(
    store: &SkillBundleStore,
    digest: &str,
    archive_size: u64,
    chunk_count: u64,
    modified: SystemTime,
) {
    let staging_root = store.root().join(".staging");
    private_directory(&staging_root).unwrap();
    let staging = staging_root.join(digest);
    let staging_dir = private_directory(&staging).unwrap();
    let state = skill_bundle_archive::BundleState {
        digest: digest.to_owned(),
        archive_size,
        chunk_count,
    };
    write_private(
        &staging_dir,
        "state.json",
        &serde_json::to_vec(&state).unwrap(),
    )
    .unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(staging.join("state.json"))
        .unwrap()
        .set_modified(modified)
        .unwrap();
}

fn create_orphan_staging(store: &SkillBundleStore, digest: &str, modified: SystemTime) {
    let staging_root = store.root().join(".staging");
    private_directory(&staging_root).unwrap();
    let staging = staging_root.join(digest);
    private_directory(&staging).unwrap();
    std::fs::File::open(staging)
        .unwrap()
        .set_modified(modified)
        .unwrap();
}

fn test_digest(prefix: u8, suffix: u8) -> String {
    format!("{prefix:02x}{suffix:02x}{}", "0".repeat(60))
}

fn traversal_archive() -> Vec<u8> {
    let mut raw = Vec::new();
    {
        let mut builder = Builder::new(&mut raw);
        let body = b"bad";
        let mut header = Header::new_gnu();
        header.set_path("safe").unwrap();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append(&header, &body[..]).unwrap();
        builder.finish().unwrap();
    }
    raw[..100].fill(0);
    raw[..9].copy_from_slice(b"../escape");
    raw[148..156].fill(b' ');
    let checksum: u32 = raw[..512].iter().map(|byte| u32::from(*byte)).sum();
    let text = format!("{checksum:06o}");
    raw[148..154].copy_from_slice(text.as_bytes());
    raw[154] = 0;
    raw[155] = b' ';
    let mut compressed = GzEncoder::new(Vec::new(), Compression::default());
    compressed.write_all(&raw).unwrap();
    compressed.finish().unwrap()
}

#[test]
fn installs_chunks_and_reuses_verified_bundle_after_restart() {
    let temporary = tempfile::tempdir().unwrap();
    let bytes = valid_archive();
    let chunks = chunks(&bytes);
    let first = SkillBundleStore::new(temporary.path());
    for chunk in &chunks {
        first.install(chunk).unwrap();
    }
    let digest = &chunks[0].digest;
    let final_dir = first.root().join(digest);
    assert!(final_dir.join("scripts/validate.ts").is_file());
    assert!(first.verify_all().is_ok());
    let second = SkillBundleStore::new(temporary.path());
    assert!(second.verify_all().is_ok());
    assert_eq!(
        second.install(&chunks[0]).unwrap()["installed"].as_bool(),
        Some(true)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(final_dir.join("scripts/validate.ts"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o444
        );
    }
}

#[test]
fn out_of_order_chunks_resume_after_restart_and_preserve_exact_bytes() {
    let temporary = tempfile::tempdir().unwrap();
    let (bytes, payload) = large_archive();
    assert!(bytes.len() > CHUNK_BYTES);
    let chunks = chunks(&bytes);
    assert!(chunks.len() >= 3);
    let first = SkillBundleStore::new(temporary.path());
    assert_eq!(
        first.install(&chunks[0]).unwrap()["installed"].as_bool(),
        Some(false)
    );
    assert!(!first.root().join(&chunks[0].digest).exists());

    let second = SkillBundleStore::new(temporary.path());
    let mut order = vec![2_usize, 1];
    order.extend(3..chunks.len());
    let order_len = order.len();
    for (position, index) in order.into_iter().enumerate() {
        let result = second.install(&chunks[index]).unwrap();
        if position + 1 == order_len {
            assert_eq!(result["installed"].as_bool(), Some(true));
        } else {
            assert_eq!(result["installed"].as_bool(), Some(false));
            assert!(!second.root().join(&chunks[0].digest).exists());
        }
    }
    assert_eq!(
        fs::read(
            second
                .root()
                .join(&chunks[0].digest)
                .join("scripts/payload.bin")
        )
        .unwrap(),
        payload
    );
}

#[test]
fn rejects_traversal_without_publishing() {
    let temporary = tempfile::tempdir().unwrap();
    let bytes = traversal_archive();
    let chunks = chunks(&bytes);
    let store = SkillBundleStore::new(temporary.path());
    for chunk in &chunks {
        let result = store.install(chunk);
        if chunk.chunk_index + 1 == chunk.chunk_count {
            assert_eq!(result, Err("BUNDLE_ARCHIVE_UNSAFE_PATH"));
        }
    }
    assert!(!store.root().join(&chunks[0].digest).exists());
}

#[test]
fn malformed_skill_archives_fail_without_publishing() {
    let cases = [
        (
            "missing skill",
            missing_skill_archive(),
            "BUNDLE_SKILL_MD_MISSING",
        ),
        (
            "unsafe path",
            traversal_archive(),
            "BUNDLE_ARCHIVE_UNSAFE_PATH",
        ),
        ("symlink", symlink_archive(), "BUNDLE_ARCHIVE_UNSAFE_MEMBER"),
        ("malformed gzip", vec![1, 2, 3], "BUNDLE_ARCHIVE_INVALID"),
    ];
    for (name, bytes, expected) in cases {
        let temporary = tempfile::tempdir().unwrap();
        let chunks = chunks(&bytes);
        let store = SkillBundleStore::new(temporary.path());
        let result = store.install(&chunks[0]);
        assert_eq!(result, Err(expected), "{name}");
        assert!(!store.root().join(&chunks[0].digest).exists(), "{name}");
    }
}

#[test]
fn detects_tampered_files_before_execution() {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    let temporary = tempfile::tempdir().unwrap();
    let bytes = valid_archive();
    let chunks = chunks(&bytes);
    let store = SkillBundleStore::new(temporary.path());
    for chunk in &chunks {
        store.install(chunk).unwrap();
    }
    let file = store.root().join(&chunks[0].digest).join("SKILL.md");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(&file, b"tampered").unwrap();
    assert_eq!(store.verify_all(), Err("BUNDLE_CACHE_TAMPERED"));
}

#[test]
fn concurrent_chunk_delivery_publishes_one_verified_tree() {
    let temporary = tempfile::tempdir().unwrap();
    let chunks = chunks(&valid_archive());
    let store = SkillBundleStore::new(temporary.path());
    let handles = (0..4)
        .map(|_| {
            let store = store.clone();
            let chunk = chunks[0].clone();
            thread::spawn(move || store.install(&chunk))
        })
        .collect::<Vec<_>>();
    for handle in handles {
        assert!(handle.join().unwrap().is_ok());
    }
    assert!(store.verify_all().is_ok());
}

#[test]
fn staging_quota_blocks_new_bundles_but_allows_existing_resume() {
    let temporary = tempfile::tempdir().unwrap();
    let store = SkillBundleStore::new(temporary.path());
    let bytes = valid_archive();
    let bundle_chunks = chunks(&bytes);
    let now = SystemTime::now();
    let fake_digests = (1..=MAX_STAGING_BUNDLES)
        .map(|index| test_digest(index as u8, 0))
        .collect::<Vec<_>>();
    for digest in &fake_digests {
        create_staging_state(&store, digest, 1, 1, now);
    }

    assert_eq!(store.install(&bundle_chunks[0]), Err("BUNDLE_CACHE_BUSY"));

    skill_bundle_archive::remove_staging_tree(
        &store.root().join(".staging").join(&fake_digests[0]),
    )
    .unwrap();
    create_staging_state(
        &store,
        &bundle_chunks[0].digest,
        bundle_chunks[0].archive_size,
        bundle_chunks[0].chunk_count,
        now,
    );
    assert_eq!(
        store.install(&bundle_chunks[0]).unwrap()["installed"].as_bool(),
        Some(true)
    );
}

#[test]
fn stale_staging_cleanup_removes_idle_entries_and_preserves_locked_entries() {
    let temporary = tempfile::tempdir().unwrap();
    let store = SkillBundleStore::new(temporary.path());
    let now = SystemTime::now();
    let old = now
        .checked_sub(STAGING_IDLE_TTL + Duration::from_secs(1))
        .unwrap();
    let fresh_digest = test_digest(1, 0);
    let old_digest = test_digest(2, 0);
    let locked_digest = test_digest(3, 0);
    let orphan_digest = test_digest(4, 0);
    create_staging_state(&store, &fresh_digest, 1, 1, now);
    create_staging_state(&store, &old_digest, 1, 1, old);
    create_staging_state(&store, &locked_digest, 1, 1, old);
    create_orphan_staging(&store, &orphan_digest, old);
    let _locked = lock_file(&store.root, &locked_digest).unwrap();

    cleanup_stale_stages_at(&store.root, &store.root().join(".staging"), now).unwrap();

    assert!(store.root().join(".staging").join(&fresh_digest).exists());
    assert!(!store.root().join(".staging").join(&old_digest).exists());
    assert!(store.root().join(".staging").join(&locked_digest).exists());
    assert!(!store.root().join(".staging").join(&orphan_digest).exists());
}

#[test]
fn rejected_digests_reuse_a_bounded_lock_slot_set() {
    let temporary = tempfile::tempdir().unwrap();
    let store = SkillBundleStore::new(temporary.path());
    let now = SystemTime::now();
    for prefix in 0..MAX_STAGING_BUNDLES {
        create_staging_state(&store, &test_digest(prefix as u8, 0), 1, 1, now);
    }
    let rejected = (0..=u8::MAX)
        .flat_map(|prefix| [test_digest(prefix, 1), test_digest(prefix, 2)])
        .map(|digest| SkillBundleChunk {
            kind: KIND.to_owned(),
            digest,
            chunk_index: 0,
            chunk_count: 1,
            archive_size: 1,
            content_base64: base64::engine::general_purpose::STANDARD.encode([0_u8]),
        })
        .collect::<Vec<_>>();
    for chunk in &rejected {
        assert_eq!(store.install(chunk), Err("BUNDLE_CACHE_BUSY"));
    }
    let lock_count = fs::read_dir(store.root())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_str().is_some_and(is_lock_name))
        .count();
    assert_eq!(lock_count, 256);
}

#[cfg(unix)]
#[test]
fn readonly_staging_tree_is_recoverable_after_restart() {
    use std::os::unix::fs::PermissionsExt;

    let temporary = tempfile::tempdir().unwrap();
    let tree = temporary.path().join("tree");
    let nested = tree.join("scripts");
    fs::create_dir_all(&nested).unwrap();
    let file = nested.join("validate.ts");
    fs::write(&file, b"validate").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o444)).unwrap();
    fs::set_permissions(&nested, fs::Permissions::from_mode(0o555)).unwrap();
    fs::set_permissions(&tree, fs::Permissions::from_mode(0o700)).unwrap();
    super::skill_bundle_archive::remove_staging_tree(&tree).unwrap();
    assert!(!tree.exists());
}

#[test]
fn completed_cache_is_bounded_without_removing_installed_bundles() {
    let temp = tempfile::tempdir().unwrap();
    let store = SkillBundleStore::new(temp.path());
    let installed = chunks(&valid_archive());
    store.install(&installed[0]).unwrap();
    for index in 0..63 {
        let archive = archive_from_entries(&[("SKILL.md", format!("skill {index}").as_bytes())]);
        store.install(&chunks(&archive)[0]).unwrap();
    }
    let incoming = chunks(&archive_from_entries(&[("SKILL.md", b"another skill")]));
    assert_eq!(store.install(&incoming[0]), Err("BUNDLE_CACHE_FULL"));
    assert!(
        !store
            .root()
            .join(".staging")
            .join(&incoming[0].digest)
            .exists()
    );
    assert_eq!(store.install(&installed[0]).unwrap()["installed"], true);
}

#[test]
fn staging_reserves_cache_capacity_and_can_complete_when_full() {
    let temp = tempfile::tempdir().unwrap();
    let store = SkillBundleStore::new(temp.path());
    let staged = chunks(&large_archive().0);
    assert_eq!(store.install(&staged[0]).unwrap()["installed"], false);
    for index in 0..63 {
        let archive = archive_from_entries(&[("SKILL.md", format!("skill {index}").as_bytes())]);
        store.install(&chunks(&archive)[0]).unwrap();
    }
    assert_eq!(
        store.install(&chunks(&valid_archive())[0]),
        Err("BUNDLE_CACHE_FULL")
    );
    for chunk in &staged[1..] {
        store.install(chunk).unwrap();
    }
    assert_eq!(store.install(&staged[0]).unwrap()["installed"], true);
}

#[test]
fn archives_over_the_extraction_limits_fail_without_publishing() {
    let skill = (
        "SKILL.md".to_owned(),
        b"---\nname: demo\ndescription: Demo\n---\n".to_vec(),
    );
    let mut many = vec![skill.clone()];
    many.extend((0..MAX_FILES).map(|index| (format!("scripts/f-{index}.ts"), b"ok\n".to_vec())));
    let cases = [
        ("too many files", many, "BUNDLE_ARCHIVE_TOO_MANY_FILES"),
        (
            "single file over the ceiling",
            vec![
                skill.clone(),
                (
                    "scripts/huge.bin".to_owned(),
                    vec![0_u8; (MAX_FILE_BYTES + 1) as usize],
                ),
            ],
            "BUNDLE_ARCHIVE_TOO_LARGE",
        ),
        (
            "duplicate path",
            vec![
                skill.clone(),
                ("scripts/validate.ts".to_owned(), b"first".to_vec()),
                ("scripts/validate.ts".to_owned(), b"second".to_vec()),
            ],
            "BUNDLE_ARCHIVE_DUPLICATE_PATH",
        ),
    ];
    for (name, entries, expected) in cases {
        let borrowed = entries
            .iter()
            .map(|(path, body)| (path.as_str(), body.as_slice()))
            .collect::<Vec<_>>();
        let bytes = archive_from_entries(&borrowed);
        assert!(bytes.len() <= CHUNK_BYTES, "{name} needs a single chunk");
        let temporary = tempfile::tempdir().unwrap();
        let store = SkillBundleStore::new(temporary.path());
        let chunks = chunks(&bytes);
        assert_eq!(store.install(&chunks[0]), Err(expected), "{name}");
        assert!(!store.root().join(&chunks[0].digest).exists(), "{name}");
        assert!(store.verify_all().is_ok(), "{name}");
    }
}

#[test]
fn a_chunk_that_contradicts_a_stored_one_is_refused_without_overwriting_it() {
    let temporary = tempfile::tempdir().unwrap();
    let store = SkillBundleStore::new(temporary.path());
    let chunks = chunks(&large_archive().0);
    assert!(chunks.len() >= 3);
    assert_eq!(store.install(&chunks[0]).unwrap()["installed"], false);
    let stored = fs::read(
        store
            .root()
            .join(".staging")
            .join(&chunks[0].digest)
            .join("chunks/0"),
    )
    .unwrap();

    let mut contradicting = chunks[0].clone();
    contradicting.content_base64 =
        base64::engine::general_purpose::STANDARD.encode(vec![0_u8; stored.len()]);
    assert_eq!(store.install(&contradicting), Err("BUNDLE_CHUNK_MISMATCH"));

    let mut short = chunks[0].clone();
    short.content_base64 =
        base64::engine::general_purpose::STANDARD.encode(&stored[..stored.len() - 1]);
    assert_eq!(store.install(&short), Err("BUNDLE_INVALID_CHUNK"));

    assert_eq!(
        fs::read(
            store
                .root()
                .join(".staging")
                .join(&chunks[0].digest)
                .join("chunks/0")
        )
        .unwrap(),
        stored
    );
    for chunk in &chunks[1..] {
        store.install(chunk).unwrap();
    }
    assert_eq!(store.install(&chunks[0]).unwrap()["installed"], true);
    assert!(store.verify_all().is_ok());
}
