//! Opening the database: migrations on a fresh file, and again on an existing one.

use std::time::Duration;

use tubarr::store::{
    Db, GrabStatus, MappingKind, NewGrab, NewMapping, NewUnmatched, SeenVideo, UnmatchedState,
    VideoSource, unix_now,
};

#[tokio::test]
async fn migrations_run_on_fresh_and_existing_databases() {
    let dir = tempfile::tempdir().unwrap();
    // A folder that does not exist yet, like a first start on an empty volume.
    let path = dir.path().join("data").join("tubarr.db");

    let db = Db::open(&path).await.unwrap();
    let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(journal, "wal");
    let stored = db
        .insert_mapping(&NewMapping {
            kind: MappingKind::Film,
            tvdb_id: None,
            tmdb_id: Some(352_173),
            title: "Theri".to_owned(),
            language: "Hindi".to_owned(),
            channel_ids: vec![],
            episode_regex: None,
            excludes: vec![],
            min_duration_secs: None,
        })
        .await
        .unwrap();
    db.pool().close().await;

    let db = Db::open(&path).await.unwrap();
    assert_eq!(db.mapping(stored.id).await.unwrap(), Some(stored));
    let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(db.pool())
        .await
        .unwrap();
    // Every migration ran once, none twice.
    let files = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|x| x == "sql")
        })
        .count();
    assert_eq!(applied, i64::try_from(files).unwrap());
}

#[tokio::test]
async fn unwritable_location_fails_with_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("file");
    std::fs::write(&blocker, b"").unwrap();
    let err = Db::open(&blocker.join("tubarr.db")).await.unwrap_err();
    assert!(err.to_string().contains("file"), "{err}");
}

/// A database file migrated by an older build (every migration before the one whose file name
/// starts with `before`, else every one but the newest), with its pool still open so a test can
/// add rows. Returns the pool, the path and the number of migrations.
async fn older_release_db(
    dir: &std::path::Path,
    before: Option<&str>,
) -> (sqlx::SqlitePool, std::path::PathBuf, usize) {
    let old = dir.join("old-migrations");
    std::fs::create_dir(&old).unwrap();
    let mut files: Vec<_> = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "sql"))
        .collect();
    files.sort();
    let total = files.len();
    let older = before.map_or(total - 1, |prefix| {
        files
            .iter()
            .position(|f| f.file_name().unwrap().to_str().unwrap().starts_with(prefix))
            .unwrap()
    });
    for file in &files[..older] {
        std::fs::copy(file, old.join(file.file_name().unwrap())).unwrap();
    }
    let path = dir.join("tubarr.db");
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let pool = sqlx::SqlitePool::connect_with(options).await.unwrap();
    sqlx::migrate::Migrator::new(old.as_path())
        .await
        .unwrap()
        .run(&pool)
        .await
        .unwrap();
    (pool, path, total)
}

#[tokio::test]
async fn database_from_an_older_release_gets_the_newer_migrations() {
    let dir = tempfile::tempdir().unwrap();
    let (pool, path, total) = older_release_db(dir.path(), None).await;
    pool.close().await;

    let db = Db::open(&path).await.unwrap();
    let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(applied, i64::try_from(total).unwrap());
}

#[tokio::test]
async fn raw_heights_become_resolution_labels() {
    let dir = tempfile::tempdir().unwrap();
    let (pool, path, _) = older_release_db(dir.path(), Some("0022_")).await;
    sqlx::query(
        "INSERT INTO mappings (id, kind, tvdb_id, title, language, created_at, updated_at) \
         VALUES (1, 'show', 1, 'Main Manto Nahi Hoon', 'Urdu', 0, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    // (video, raw height, cached width or none, label wanted)
    let cases = [
        ("crop1080aaa", 804, Some(1920), 1080),
        ("crop720aaaa", 536, Some(1280), 720),
        ("fourthree1k", 1080, Some(1440), 1080),
        ("plain1080aa", 1080, Some(1920), 1080),
        ("sd480aaaaaa", 480, Some(854), 480),
        ("nowidthaaaa", 810, None, 720),
    ];
    for (n, (id, height, width, _)) in cases.iter().enumerate() {
        sqlx::query(
            "INSERT INTO videos (video_id, title, height, updated_at) VALUES (?, 't', ?, 0)",
        )
        .bind(id)
        .bind(height)
        .execute(&pool)
        .await
        .unwrap();
        if let Some(w) = width {
            let formats = format!(r#"{{"video":{{"width":{w},"height":{height}}}}}"#);
            sqlx::query(
                "INSERT INTO resolve_cache (video_id, formats, expires_at) VALUES (?, ?, 0)",
            )
            .bind(id)
            .bind(formats)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO releases (mapping_id, video_id, season, episode, video_title, language, \
             height, published_at, created_at) VALUES (1, ?, 1, ?, 't', 'Urdu', ?, 0, 0)",
        )
        .bind(id)
        .bind(i64::try_from(n).unwrap() + 1)
        .bind(height)
        .execute(&pool)
        .await
        .unwrap();
    }
    pool.close().await;

    let db = Db::open(&path).await.unwrap();
    for (id, _, _, want) in cases {
        let video: i64 = sqlx::query_scalar("SELECT height FROM videos WHERE video_id = ?")
            .bind(id)
            .fetch_one(db.pool())
            .await
            .unwrap();
        let release: i64 = sqlx::query_scalar("SELECT height FROM releases WHERE video_id = ?")
            .bind(id)
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!((video, release), (want, want), "{id}");
    }
}

#[tokio::test]
async fn old_members_only_marks_get_their_own_reason_and_a_retry() {
    let dir = tempfile::tempdir().unwrap();
    let (pool, path, _) = older_release_db(dir.path(), Some("0028_")).await;
    let marks = [
        (
            "members0001",
            "video unavailable: ERROR: [youtube] members0001: Join this channel to get access to \
             members-only content like this video, and other exclusive perks.",
        ),
        (
            "premium0001",
            "video unavailable: ERROR: [youtube] premium0001: This video is only available to \
             Music Premium members",
        ),
        (
            "removed0001",
            "video unavailable: ERROR: [youtube] removed0001: This video has been removed",
        ),
    ];
    for (id, reason) in marks {
        sqlx::query(
            "INSERT INTO unplayable_videos (video_id, reason, checked_at, retry) \
             VALUES (?, ?, 0, 0)",
        )
        .bind(id)
        .bind(reason)
        .execute(&pool)
        .await
        .unwrap();
    }
    pool.close().await;

    let db = Db::open(&path).await.unwrap();
    let got: Vec<(String, bool)> = db
        .unplayable_videos()
        .await
        .unwrap()
        .into_iter()
        .map(|u| (u.reason, u.retry))
        .collect();
    assert_eq!(
        got,
        [
            (
                "video is for members or Premium only: ERROR: [youtube] members0001: Join this \
                 channel to get access to members-only content like this video, and other \
                 exclusive perks."
                    .to_owned(),
                true
            ),
            (
                "video is for members or Premium only: ERROR: [youtube] premium0001: This video \
                 is only available to Music Premium members"
                    .to_owned(),
                true
            ),
            (marks[2].1.to_owned(), false),
        ]
    );
}

/// Upgrading to the list of released videos: every release (each part of a joined one), pin and
/// written grab of the older build is on it, a failed grab is not.
#[tokio::test]
async fn released_videos_start_from_older_releases_pins_and_grabs() {
    let dir = tempfile::tempdir().unwrap();
    let (pool, path, _) = older_release_db(dir.path(), Some("0035_")).await;
    for sql in [
        "INSERT INTO mappings (id, kind, tvdb_id, title, language, created_at, updated_at) \
         VALUES (1, 'show', 1, 'Kabhi Main Kabhi Tum', 'Urdu', 0, 0)",
        "INSERT INTO releases (mapping_id, video_id, season, episode, video_title, language, \
         height, published_at, created_at) VALUES (1, 'single00001', 1, 1, 't', 'Urdu', 1080, 0, 0)",
        "INSERT INTO releases (mapping_id, video_id, part_ids, season, episode, video_title, \
         language, height, published_at, created_at) VALUES (1, 'joined00001', \
         '[\"joined00001\",\"joined00002\"]', 1, 2, 't', 'Urdu', 1080, 0, 0)",
        "INSERT INTO pins (mapping_id, season, episode, video_id, created_at) \
         VALUES (1, 1, 3, 'pinned00001,pinned00002', 0)",
        "INSERT INTO grabs (nzo_id, release_name, video_id, category, status, storage_path, \
         created_at) VALUES ('a', 'r', 'grabbed0001,grabbed0002', 'tv', 'completed', '/d', 0)",
        "INSERT INTO grabs (nzo_id, release_name, video_id, category, status, storage_path, \
         created_at) VALUES ('b', 'r', 'failed00001', 'tv', 'failed', '', 0)",
    ] {
        sqlx::query(sql).execute(&pool).await.unwrap();
    }
    pool.close().await;

    let db = Db::open(&path).await.unwrap();
    let released: Vec<String> =
        sqlx::query_scalar("SELECT video_id FROM released_videos ORDER BY video_id")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(
        released,
        [
            "grabbed0001",
            "grabbed0002",
            "joined00001",
            "joined00002",
            "pinned00001",
            "pinned00002",
            "single00001",
        ]
    );
}

#[tokio::test]
async fn match_inputs_migration_leaves_a_large_notes_table_alone_and_runs_once() {
    let dir = tempfile::tempdir().unwrap();
    let (pool, path, total) = older_release_db(dir.path(), Some("0036_")).await;
    sqlx::query(
        "INSERT INTO mappings (id, kind, tvdb_id, title, language, created_at, updated_at) \
         VALUES (1, 'show', 1, 'Main Manto Nahi Hoon', 'Urdu', 0, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    // Our instance is past 6000 notes.
    sqlx::query(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 6500) \
         INSERT INTO unmatched (mapping_id, video_id, state, reason, created_at, updated_at) \
         SELECT 1, printf('vid%08d', i), 'capped', 'old', 0, 0 FROM n",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    let started = std::time::Instant::now();
    let db = Db::open(&path).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    let notes: i64 = sqlx::query_scalar("SELECT count(*) FROM unmatched")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(notes, 6500, "the migration must not touch the notes");
    assert_eq!(db.match_digest(1).await.unwrap(), None);
    db.set_match_digest(1, 42).await.unwrap();
    assert_eq!(db.match_digest(1).await.unwrap(), Some(42));
    let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(applied, i64::try_from(total).unwrap());

    // The statement itself is safe to run again.
    let sql = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/migrations/0036_match_inputs.sql"
    ))
    .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(db.match_digest(1).await.unwrap(), Some(42));
    db.pool().close().await;
    let db = Db::open(&path).await.unwrap();
    assert_eq!(db.match_digest(1).await.unwrap(), Some(42));
}

fn note(video_id: &str, state: UnmatchedState) -> NewUnmatched {
    NewUnmatched {
        video_id: video_id.to_owned(),
        state,
        season: Some(1),
        episode: Some(3),
        part: None,
        reason: "r".to_owned(),
    }
}

#[tokio::test]
async fn note_retention_drops_old_settled_notes_and_keeps_recent_and_pending_ones() {
    const DAY: i64 = 86_400;
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("tubarr.db")).await.unwrap();
    let mapping = db
        .insert_mapping(&NewMapping {
            kind: MappingKind::Show,
            tvdb_id: Some(1),
            tmdb_id: None,
            title: "Main Manto Nahi Hoon".to_owned(),
            language: "Urdu".to_owned(),
            channel_ids: vec!["UCchannel".to_owned()],
            episode_regex: None,
            excludes: vec![],
            min_duration_secs: None,
        })
        .await
        .unwrap()
        .id;
    let now = unix_now();
    let uploads = [
        ("old-unsure", 400),
        ("old-dup", 400),
        ("old-capped", 400),
        ("old-partial", 400),
        ("old-pending", 400),
        ("new-unsure", 10),
        ("new-partial", 10),
        ("new-dup", 179),
    ];
    for (id, days) in uploads {
        db.upsert_seen_video(&SeenVideo {
            video_id: id.to_owned(),
            channel_id: "UCchannel".to_owned(),
            title: "t".to_owned(),
            published_at: Some(now - days * DAY),
            published_approx: false,
            duration_secs: None,
            source: VideoSource::Rss,
        })
        .await
        .unwrap();
    }
    let notes = [
        note("old-unsure", UnmatchedState::Unsure),
        note("old-dup", UnmatchedState::Duplicate),
        note("old-capped", UnmatchedState::Capped),
        note("old-partial", UnmatchedState::Partial),
        note("old-pending", UnmatchedState::Pending),
        note("new-unsure", UnmatchedState::Unsure),
        note("new-partial", UnmatchedState::Partial),
        note("new-dup", UnmatchedState::Duplicate),
        // A pinned video the watcher never saw: its upload time is unknown, so it stays.
        note("pinned-only", UnmatchedState::Unsure),
    ];
    let ids = |db: Db| async move {
        let mut ids: Vec<String> = db
            .unmatched(mapping)
            .await
            .unwrap()
            .into_iter()
            .map(|n| n.video_id)
            .collect();
        ids.sort();
        ids
    };

    // Written before the retention existed: every note.
    db.replace_unmatched(mapping, &notes).await.unwrap();
    assert_eq!(ids(db.clone()).await.len(), notes.len());

    // Zero keeps every note.
    let keep_all = db.clone().with_note_retention(Duration::ZERO);
    assert_eq!(
        keep_all.replace_unmatched(mapping, &notes).await.unwrap(),
        0
    );
    assert_eq!(ids(db.clone()).await.len(), notes.len());

    let bounded = db
        .clone()
        .with_note_retention(Duration::from_secs(180 * 86_400));
    bounded.replace_unmatched(mapping, &notes).await.unwrap();
    let kept = [
        "new-dup",
        "new-partial",
        "new-unsure",
        "old-pending",
        "pinned-only",
    ];
    assert_eq!(ids(db.clone()).await, kept);
    // The rule goes by the upload alone: the same list again changes nothing, nothing comes back.
    assert_eq!(bounded.replace_unmatched(mapping, &notes).await.unwrap(), 0);
    assert_eq!(ids(db.clone()).await, kept);
}

#[tokio::test]
async fn pruning_drops_only_old_unused_channel_videos_and_old_grabs() {
    const DAY: i64 = 86_400;
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("tubarr.db")).await.unwrap();
    let now = unix_now();
    // (video, upload age in days, last seen age in days)
    let videos = [
        ("old-unused", 400, 300),
        ("old-seen-lately", 400, 1),
        ("new-unseen", 10, 300),
        ("old-released", 400, 300),
        ("old-part", 400, 300),
        ("old-pinned", 400, 300),
        ("old-pending", 400, 300),
        ("old-playlist", 400, 300),
    ];
    for (id, upload, _) in videos {
        db.upsert_seen_video(&SeenVideo {
            video_id: id.to_owned(),
            channel_id: "UCchannel".to_owned(),
            title: "t".to_owned(),
            published_at: Some(now - upload * DAY),
            published_approx: false,
            duration_secs: None,
            source: VideoSource::Listing,
        })
        .await
        .unwrap();
    }
    for (id, _, last_seen) in videos {
        sqlx::query("UPDATE channel_videos SET last_seen = ? WHERE video_id = ?")
            .bind(now - last_seen * DAY)
            .bind(id)
            .execute(db.pool())
            .await
            .unwrap();
    }
    let pool = db.pool();
    let mapping = db
        .insert_mapping(&NewMapping {
            kind: MappingKind::Show,
            tvdb_id: Some(1),
            tmdb_id: None,
            title: "t".to_owned(),
            language: "Urdu".to_owned(),
            channel_ids: vec!["UCchannel".to_owned()],
            episode_regex: None,
            excludes: vec![],
            min_duration_secs: None,
        })
        .await
        .unwrap()
        .id;
    assert_eq!(mapping, 1);
    sqlx::query(
        "INSERT INTO releases (mapping_id, video_id, season, episode, video_title, language, \
         height, published_at, created_at, part_ids) VALUES \
         (1, 'old-released', 1, 1, 't', 'Urdu', 1080, 0, 0, NULL), \
         (1, 'other', 1, 2, 't', 'Urdu', 1080, 0, 0, '[\"other\",\"old-part\"]')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO pins (mapping_id, season, episode, video_id, created_at) \
         VALUES (1, 1, 3, 'x,old-pinned', 0)",
    )
    .execute(pool)
    .await
    .unwrap();
    db.replace_unmatched(1, &[note("old-pending", UnmatchedState::Pending)])
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO playlist_items (playlist_id, position, video_id, title, listed_at) \
         VALUES ('PL1', 1, 'old-playlist', 't', 0)",
    )
    .execute(pool)
    .await
    .unwrap();

    let sizes = db.store_sizes().await.unwrap();
    assert_eq!((sizes.notes, sizes.channel_videos, sizes.grabs), (1, 8, 0));
    assert_eq!(
        db.prune_channel_videos(Duration::from_secs(180 * 86_400))
            .await
            .unwrap(),
        1
    );
    assert!(db.channel_video("old-unused").await.unwrap().is_none());
    assert_eq!(db.store_sizes().await.unwrap().channel_videos, 7);

    for (n, age) in [
        ("tubarr_old", 31),
        ("tubarr_failed_old", 40),
        ("tubarr_new", 2),
    ] {
        db.insert_grab(&NewGrab {
            nzo_id: n.to_owned(),
            release_name: "r".to_owned(),
            video_id: "old-released".to_owned(),
            category: "tv".to_owned(),
            status: if n.contains("failed") {
                GrabStatus::Failed
            } else {
                GrabStatus::Completed
            },
            storage_path: "/downloads/r".to_owned(),
            fail_message: None,
        })
        .await
        .unwrap();
        sqlx::query("UPDATE grabs SET created_at = ? WHERE nzo_id = ?")
            .bind(now - age * DAY)
            .bind(n)
            .execute(pool)
            .await
            .unwrap();
    }
    let month = Duration::from_secs(30 * 86_400);
    assert_eq!(db.prune_grabs(month).await.unwrap(), 2);
    let left: Vec<_> = db
        .grabs(None, -1, 0)
        .await
        .unwrap()
        .into_iter()
        .map(|g| g.nzo_id)
        .collect();
    assert_eq!(left, ["tubarr_new"]);
    assert_eq!(db.prune_grabs(month).await.unwrap(), 0);
}
