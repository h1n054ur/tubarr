//! A test-only pause in the actual repository call, after its retention SELECT.
//! The pause runs no SQL and changes neither the connection nor its transaction.

use std::{sync::Arc, time::Duration};

use sqlx::{Connection, SqliteConnection};
use tokio::sync::Semaphore;

use super::*;
use crate::store::{SeenVideo, VideoSource, mappings::tests::drama, test_db};

fn note(video_id: &str, state: UnmatchedState, reason: &str) -> NewUnmatched {
    NewUnmatched {
        video_id: video_id.to_owned(),
        state,
        season: Some(1),
        episode: Some(25),
        part: None,
        reason: reason.to_owned(),
    }
}

tokio::task_local! {
    static AFTER_READ: Gate;
}

#[derive(Clone)]
struct Gate {
    read: Arc<Semaphore>,
    resume: Arc<Semaphore>,
}

impl Gate {
    fn new() -> Self {
        Self {
            read: Arc::new(Semaphore::new(0)),
            resume: Arc::new(Semaphore::new(0)),
        }
    }

    async fn reached(&self) {
        self.read.acquire().await.unwrap().forget();
    }
}

pub(super) async fn after_retention_read() {
    if let Ok(gate) = AFTER_READ.try_with(Clone::clone) {
        gate.read.add_permits(1);
        gate.resume.acquire().await.unwrap().forget();
    }
}

async fn seen(db: &Db, id: &str, published_at: i64) {
    db.upsert_seen_video(&SeenVideo {
        video_id: id.to_owned(),
        channel_id: "UCchannel".to_owned(),
        title: "a video".to_owned(),
        published_at: Some(published_at),
        published_approx: false,
        duration_secs: None,
        source: VideoSource::Rss,
    })
    .await
    .unwrap();
}

fn code(error: &StoreError) -> Option<String> {
    match error {
        StoreError::Sql(error) => error.as_database_error()?.code().map(|c| c.into_owned()),
        _ => None,
    }
}

async fn writer(db: &Db) -> SqliteConnection {
    // A separate physical connection, not a second query on the transaction's connection.
    // Fail promptly when write intent is already reserved by replace_unmatched.
    SqliteConnection::connect_with(
        &db.pool()
            .connect_options()
            .as_ref()
            .clone()
            .busy_timeout(Duration::ZERO),
    )
    .await
    .unwrap()
}

async fn age_video(conn: &mut SqliteConnection) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE channel_videos SET published_at = 1 WHERE video_id = 'candidate'")
        .execute(conn)
        .await?;
    Ok(())
}

async fn interleaved_replace(delete_only: bool) {
    let (db, _dir) = test_db::fresh().await;
    let mapping = db.insert_mapping(&drama()).await.unwrap().id;
    seen(&db, "candidate", unix_now()).await;
    db.replace_unmatched(mapping, &[note("removed", UnmatchedState::Unsure, "old")])
        .await
        .unwrap();
    let db = db.with_note_retention(Duration::from_secs(86_400));
    let mut writer = writer(&db).await;
    assert_eq!(
        sqlx::query_scalar::<_, String>("PRAGMA journal_mode")
            .fetch_one(&mut writer)
            .await
            .unwrap(),
        "wal"
    );
    let notes = if delete_only {
        vec![]
    } else {
        vec![note("candidate", UnmatchedState::Unsure, "new")]
    };
    let gate = Gate::new();
    let replace = AFTER_READ.scope(gate.clone(), db.replace_unmatched(mapping, &notes));
    let other = async {
        gate.reached().await;
        // On the unfixed path this commits after the real SELECT and before the real write.
        // After write intent is acquired first it returns ordinary SQLITE_BUSY instead.
        let result = age_video(&mut writer).await;
        gate.resume.add_permits(1);
        result
    };
    let (result, competing) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(replace, other)
    })
    .await
    .expect("the actual store call and competing writer must both finish");
    assert!(
        result.is_ok(),
        "actual replace_unmatched failed after retention read, SQLite code {:?}: {result:?}; competing writer: {competing:?}",
        result.as_ref().err().and_then(code)
    );
    assert_eq!(result.unwrap(), u64::from(!delete_only));
    let busy =
        competing.expect_err("the note transaction must reserve write intent before reading");
    assert_eq!(
        busy.as_database_error().unwrap().code().as_deref(),
        Some("5")
    );
    let kept = db.unmatched(mapping).await.unwrap();
    if delete_only {
        assert!(kept.is_empty());
    } else {
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].video_id, "candidate");
        assert_eq!(db.match_counts().await.unwrap().unmatched, 1);
    }

    // The independent writer succeeds once the note transaction commits. The next pass reads
    // that committed eligibility change and drops the now-old settled note.
    age_video(&mut writer).await.unwrap();
    assert_eq!(db.replace_unmatched(mapping, &notes).await.unwrap(), 0);
    assert!(db.unmatched(mapping).await.unwrap().is_empty());
    writer.close().await.unwrap();
    db.pool().close().await;
}

#[tokio::test]
async fn retention_insert_does_not_upgrade_a_stale_wal_snapshot() {
    interleaved_replace(false).await;
}

#[tokio::test]
async fn retention_delete_does_not_upgrade_a_stale_wal_snapshot() {
    interleaved_replace(true).await;
}

#[tokio::test]
async fn matcher_completes_and_keeps_existing_releases_during_channel_write() {
    use crate::matcher::{
        Candidate, Candidates, Known, Metadata, Wanted, WantedEpisode, WantedSeries, match_shows,
    };

    struct Inputs {
        series: WantedSeries,
        videos: Vec<Candidate>,
    }
    impl Candidates for Inputs {
        async fn on_channels(&self, _: &[String]) -> Result<Vec<Candidate>, StoreError> {
            Ok(self.videos.clone())
        }
    }
    impl Wanted for Inputs {
        async fn series(&self) -> Result<Vec<WantedSeries>, StoreError> {
            Ok(vec![self.series.clone()])
        }
    }
    impl Metadata for Inputs {
        async fn known(&self, _: &str) -> Result<Known, StoreError> {
            Ok(Known {
                height: Some(1080),
                ..Known::default()
            })
        }
    }

    let (db, _dir) = test_db::fresh().await;
    let mapping = db.insert_mapping(&drama()).await.unwrap();
    let videos = [("released", 1, 2400), ("candidate", 2, 100)]
        .into_iter()
        .map(|(id, episode, duration)| Candidate {
            video_id: id.to_owned(),
            channel_id: mapping.channel_ids[0].clone(),
            title: format!("{} Episode {episode}", mapping.title),
            published_at: unix_now(),
            published_approx: false,
            duration_secs: Some(duration),
        })
        .collect();
    let mapping_id = mapping.id;
    let inputs = Inputs {
        series: WantedSeries {
            mapping,
            episodes: [1, 2]
                .into_iter()
                .map(|episode| WantedEpisode {
                    season: 1,
                    episode,
                    air_date: None,
                    title: None,
                    monitored: true,
                })
                .collect(),
        },
        videos,
    };
    seen(&db, "candidate", unix_now()).await;
    let db = db.with_note_retention(Duration::from_secs(86_400));
    let first = match_shows(&db, &inputs, &inputs, &inputs).await.unwrap();
    assert_eq!((first.released, first.notes_changed), (1, 1));
    let release = db.release_by_video("released").await.unwrap().unwrap();
    let notes = db.unmatched(mapping_id).await.unwrap();
    let mut writer = writer(&db).await;
    let gate = Gate::new();
    let matching = AFTER_READ.scope(gate.clone(), match_shows(&db, &inputs, &inputs, &inputs));
    let other = async {
        gate.reached().await;
        let result = age_video(&mut writer).await;
        gate.resume.add_permits(1);
        result
    };
    let (result, competing) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(matching, other)
    })
    .await
    .expect("matcher and coordinated writer must finish");
    // Check preservation even on the red path, before requiring a successful matcher result.
    assert_eq!(
        db.release_by_video("released").await.unwrap(),
        Some(release)
    );
    assert_eq!(db.unmatched(mapping_id).await.unwrap(), notes);
    assert!(
        result.is_ok(),
        "actual matcher failed, SQLite code {:?}: {result:?}; competing writer: {competing:?}",
        result.as_ref().err().and_then(code)
    );
    let report = result.unwrap();
    assert_eq!(
        (report.released, report.replaced, report.notes_changed),
        (0, 0, 0)
    );
    assert_eq!(report.matched, 1);
    let busy = competing.expect_err("matcher note transaction must already own write intent");
    assert_eq!(
        busy.as_database_error().unwrap().code().as_deref(),
        Some("5")
    );
    age_video(&mut writer).await.unwrap();
    writer.close().await.unwrap();
    db.pool().close().await;
}

#[tokio::test]
async fn retention_rewrite_keeps_identity_ignored_flags_and_unchanged_rows() {
    let (db, _dir) = test_db::fresh().await;
    let mapping = db.insert_mapping(&drama()).await.unwrap().id;
    seen(&db, "old", 1).await;
    seen(&db, "pending", 1).await;
    let initial = [
        note("stable", UnmatchedState::Unsure, "same"),
        note("changed", UnmatchedState::Unsure, "before"),
        note("old", UnmatchedState::Unsure, "too old"),
        note("pending", UnmatchedState::Pending, "unknown"),
    ];
    assert_eq!(db.replace_unmatched(mapping, &initial).await.unwrap(), 4);
    sqlx::query("UPDATE unmatched SET ignored = 1, created_at = 11, updated_at = 22")
        .execute(db.pool())
        .await
        .unwrap();
    let before = db.unmatched(mapping).await.unwrap();
    let db = db.with_note_retention(Duration::from_secs(86_400));
    let mut next = initial.clone();
    next[1].reason = "after".to_owned();
    assert_eq!(db.replace_unmatched(mapping, &next).await.unwrap(), 1);
    let after = db.unmatched(mapping).await.unwrap();
    assert_eq!(after.len(), 3);
    for id in ["stable", "pending"] {
        assert_eq!(
            after.iter().find(|n| n.video_id == id),
            before.iter().find(|n| n.video_id == id)
        );
    }
    let changed = after.iter().find(|n| n.video_id == "changed").unwrap();
    let prior = before.iter().find(|n| n.video_id == "changed").unwrap();
    assert_eq!((changed.id, changed.created_at), (prior.id, 11));
    assert_eq!(changed.reason, "after");
    assert!(changed.updated_at > 22);
    let ignored: i64 = sqlx::query_scalar("SELECT count(*) FROM unmatched WHERE ignored = 1")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(ignored, 3);
    assert_eq!(db.replace_unmatched(mapping, &next).await.unwrap(), 0);
    assert_eq!(db.unmatched(mapping).await.unwrap(), after);
    let counts = db.match_counts().await.unwrap();
    assert_eq!((counts.pending, counts.unmatched), (1, 2));
    db.pool().close().await;
}

#[tokio::test]
async fn failed_note_write_rolls_back_prior_changes_and_propagates_error() {
    let (db, _dir) = test_db::fresh().await;
    let mapping = db.insert_mapping(&drama()).await.unwrap().id;
    let initial = [
        note("changed", UnmatchedState::Unsure, "before"),
        note("removed", UnmatchedState::Unsure, "must survive rollback"),
    ];
    db.replace_unmatched(mapping, &initial).await.unwrap();
    let before = db.unmatched(mapping).await.unwrap();
    sqlx::query(
        "CREATE TRIGGER reject_note BEFORE INSERT ON unmatched \
         WHEN NEW.video_id = 'reject' BEGIN SELECT RAISE(ABORT, 'contract note failure'); END",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let db = db.with_note_retention(Duration::from_secs(86_400));
    let error = db
        .replace_unmatched(
            mapping,
            &[
                note("changed", UnmatchedState::Duplicate, "after"),
                note("reject", UnmatchedState::Unsure, "fail here"),
            ],
        )
        .await
        .expect_err("a database failure must not become an empty or successful count");
    assert_eq!(code(&error).as_deref(), Some("1811"));
    assert_eq!(db.unmatched(mapping).await.unwrap(), before);
    // Also proves the dropped failed transaction releases its write reservation.
    assert_eq!(db.replace_unmatched(mapping, &initial).await.unwrap(), 0);
    db.pool().close().await;
}

#[tokio::test]
async fn real_writer_lock_times_out_without_using_uncommitted_eligibility() {
    let (mut db, _dir) = test_db::fresh().await;
    let mapping = db.insert_mapping(&drama()).await.unwrap().id;
    seen(&db, "candidate", unix_now()).await;
    let notes = [note("candidate", UnmatchedState::Unsure, "keep")];
    db.replace_unmatched(mapping, &notes).await.unwrap();
    let before = db.unmatched(mapping).await.unwrap();
    let production_timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(production_timeout, 5000);
    let options = db
        .pool()
        .connect_options()
        .as_ref()
        .clone()
        .busy_timeout(Duration::from_millis(50));
    db.pool().close().await;
    db.pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .unwrap();
    let db = db.with_note_retention(Duration::from_secs(86_400));
    let mut writer = writer(&db).await;
    let mut held = writer.begin_with("BEGIN IMMEDIATE").await.unwrap();
    age_video(&mut held).await.unwrap();
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        db.replace_unmatched(mapping, &notes),
    )
    .await
    .expect("a real busy writer must respect a bounded busy timeout")
    .expect_err("a held writer lock must not be swallowed");
    assert_eq!(code(&error).as_deref(), Some("5"));
    assert_eq!(db.unmatched(mapping).await.unwrap(), before);
    held.rollback().await.unwrap();
    assert_eq!(db.replace_unmatched(mapping, &notes).await.unwrap(), 0);
    assert_eq!(db.unmatched(mapping).await.unwrap(), before);
    writer.close().await.unwrap();
    db.pool().close().await;
}
