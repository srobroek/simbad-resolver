// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Restart-durability tests for the file-backed redb store.
//!
//! `tests/cache.rs` and `tests/queue.rs` also run against `Store::in_memory()`,
//! which cannot prove "pending items survive a restart" because an in-memory
//! database vanishes when the last handle drops. These tests open a real
//! on-disk database with [`Store::open`], write to it, drop the store, then
//! reopen the SAME file and assert the state persisted.

use std::sync::atomic::{AtomicU32, Ordering};

use simbad_resolver::identity::namespace;
use simbad_resolver::{
    AliasKind, Cache, ObjectType, Queue, ResolvedAlias, ResolvedIdentity, Store, TargetSource,
};

/// A temp-dir database path unique to this process + call, so parallel tests
/// never share a file.
fn unique_db_path() -> std::path::PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("simbad-resolver-restart-{}-{n}.redb", std::process::id()))
}

fn m31() -> ResolvedIdentity {
    ResolvedIdentity {
        simbad_oid: Some(1_575_544),
        primary_designation: "M 31".to_owned(),
        common_name: Some("Andromeda Galaxy".to_owned()),
        object_type: ObjectType::Galaxy,
        otype_raw: "G".to_owned(),
        ra_deg: 10.684_708,
        dec_deg: 41.268_75,
        v_mag: Some(3.44),
        galdim_majaxis_arcmin: Some(199.53),
        galdim_minaxis_arcmin: Some(70.79),
        galdim_angle_deg: Some(35),
        aliases: vec![
            ResolvedAlias::new("M 31", AliasKind::Designation),
            ResolvedAlias::new("NGC 224", AliasKind::Designation),
        ],
        source: TargetSource::Resolved,
    }
}

#[tokio::test]
async fn cache_and_queue_state_survive_a_reopen() {
    let path = unique_db_path();
    let ns = namespace("simbad-resolver.restart");

    // First "run": open a fresh db, write a queued item + a cached target.
    {
        let store = Store::open(&path).expect("open new database");
        store.queue().enqueue("img-1", "M 31").await.unwrap();
        store.cache().upsert(&m31(), &ns).await.unwrap();
        assert_eq!(store.queue().pending_count().await.unwrap(), 1);
        // Drop the store to release the database handle.
    }

    // Second "run": reopen the SAME file; committed state must still be there.
    {
        let store = Store::open(&path).expect("reopen existing database");

        let pending = store.queue().get("img-1").await.unwrap().expect("pending item persisted");
        assert_eq!(pending.query, "M 31");
        assert_eq!(store.queue().pending_count().await.unwrap(), 1);

        let cached = store
            .cache()
            .get_by_simbad_oid(1_575_544)
            .await
            .unwrap()
            .expect("cached target persisted");
        assert_eq!(cached.primary_designation, "M 31");
        assert!(cached.aliases.iter().any(|a| a.alias == "NGC 224"));
    }

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn open_is_idempotent() {
    // Opening the same file twice (fresh, then already-initialised) must both
    // succeed — table creation is guarded and re-runnable.
    let path = unique_db_path();

    let first = Store::open(&path).expect("first open initialises tables");
    drop(first);

    let second = Store::open(&path).expect("second open re-runs cleanly");
    assert!(second.cache().list().await.unwrap().is_empty());
    drop(second);

    let _ = std::fs::remove_file(&path);
}
