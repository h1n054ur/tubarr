//! The `drafts` table.

use serde::Serialize;
use sqlx::types::Json;

use super::Kind;
use crate::store::{Db, StoreError, unix_now};

/// A stored draft as the endpoints return it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StoredDraft {
    pub id: i64,
    pub kind: Kind,
    pub title: String,
    pub request: serde_json::Value,
    pub draft: serde_json::Value,
    pub text: String,
    pub created_at: i64,
}

type Row = (
    i64,
    String,
    String,
    Json<serde_json::Value>,
    Json<serde_json::Value>,
    String,
    i64,
);

pub(super) async fn insert(
    db: &Db,
    kind: Kind,
    title: &str,
    request: serde_json::Value,
    draft: serde_json::Value,
    text: String,
) -> Result<StoredDraft, StoreError> {
    let created_at = unix_now();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO drafts (kind, title, request, draft, text, created_at) \
         VALUES (?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(kind.as_str())
    .bind(title)
    .bind(Json(&request))
    .bind(Json(&draft))
    .bind(&text)
    .bind(created_at)
    .fetch_one(db.pool())
    .await?;
    Ok(StoredDraft {
        id,
        kind,
        title: title.to_owned(),
        request,
        draft,
        text,
        created_at,
    })
}

pub(super) async fn get(db: &Db, id: i64) -> Result<Option<StoredDraft>, StoreError> {
    let row: Option<Row> = sqlx::query_as(
        "SELECT id, kind, title, request, draft, text, created_at FROM drafts WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(db.pool())
    .await?;
    Ok(row.map(
        |(id, kind, title, request, draft, text, created_at)| StoredDraft {
            id,
            kind: if kind == "film" {
                Kind::Film
            } else {
                Kind::Show
            },
            title,
            request: request.0,
            draft: draft.0,
            text,
            created_at,
        },
    ))
}

/// One row of the drafts list: no request, draft or text, only what the list shows.
#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow)]
pub struct DraftSummary {
    pub id: i64,
    pub kind: String,
    pub title: String,
    /// The check's verdict when the draft was made, like `probably_missing`.
    pub verdict: Option<String>,
    /// Shows only: the episodes drafted.
    pub episodes: Option<i64>,
    pub created_at: i64,
}

/// The filter the list and its count share. Binds: kind, `LIKE` pattern. A macro so `concat!`
/// keeps the SQL a literal.
macro_rules! list_where {
    () => {
        "WHERE (?1 IS NULL OR kind = ?1) AND (?2 IS NULL OR title LIKE ?2 ESCAPE '\\')"
    };
}

/// Newest first. `kind` is `show` or `film`, `pattern` a `LIKE` pattern for the title.
pub(super) async fn list(
    db: &Db,
    kind: Option<&str>,
    pattern: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<(Vec<DraftSummary>, i64), StoreError> {
    let items = sqlx::query_as(concat!(
        "SELECT id, kind, title, json_extract(draft, '$.check.verdict') AS verdict, \
         json_array_length(draft, '$.episodes') AS episodes, created_at FROM drafts ",
        list_where!(),
        " ORDER BY created_at DESC, id DESC LIMIT ?3 OFFSET ?4"
    ))
    .bind(kind)
    .bind(pattern)
    .bind(limit)
    .bind(offset)
    .fetch_all(db.pool())
    .await?;
    let total = sqlx::query_scalar(concat!("SELECT count(*) FROM drafts ", list_where!()))
        .bind(kind)
        .bind(pattern)
        .fetch_one(db.pool())
        .await?;
    Ok((items, total))
}

/// Whether a draft with `id` was there to delete.
pub(super) async fn delete(db: &Db, id: i64) -> Result<bool, StoreError> {
    let done = sqlx::query("DELETE FROM drafts WHERE id = ?")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(done.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::test_db;

    #[tokio::test]
    async fn drafts_round_trip() {
        let (db, _dir) = test_db::fresh().await;
        let stored = insert(
            &db,
            Kind::Film,
            "Theri",
            serde_json::json!({"video_id": "ldmPWRb-XcM"}),
            serde_json::json!({"title": "Theri"}),
            "TMDB: add movie".to_owned(),
        )
        .await
        .unwrap();
        assert_eq!(get(&db, stored.id).await.unwrap(), Some(stored));
        assert_eq!(get(&db, 999).await.unwrap(), None);
    }

    #[tokio::test]
    async fn lists_newest_first_filters_and_deletes() {
        let (db, _dir) = test_db::fresh().await;
        let film = insert(
            &db,
            Kind::Film,
            "Theri",
            serde_json::json!({}),
            serde_json::json!({"title": "Theri", "check": {"verdict": "probably_missing"}}),
            String::new(),
        )
        .await
        .unwrap();
        let show = insert(
            &db,
            Kind::Show,
            "Tere Bin 100%",
            serde_json::json!({}),
            serde_json::json!({"episodes": [{}, {}], "check": {"verdict": "found"}}),
            String::new(),
        )
        .await
        .unwrap();
        let (items, total) = list(&db, None, None, 25, 0).await.unwrap();
        assert_eq!(total, 2);
        assert_eq!(
            items.iter().map(|d| d.id).collect::<Vec<_>>(),
            [show.id, film.id]
        );
        assert_eq!(items[0].episodes, Some(2));
        assert_eq!(items[0].verdict.as_deref(), Some("found"));
        assert_eq!(items[1].episodes, None);

        let (items, total) = list(&db, Some("film"), None, 25, 0).await.unwrap();
        assert_eq!((items.len(), total), (1, 1));
        let (items, _) = list(&db, None, Some("%100\\%%"), 25, 0).await.unwrap();
        assert_eq!(items[0].title, "Tere Bin 100%");
        let (items, total) = list(&db, None, None, 1, 1).await.unwrap();
        assert_eq!((items[0].id, total), (film.id, 2));

        assert!(delete(&db, film.id).await.unwrap());
        assert!(!delete(&db, film.id).await.unwrap());
        assert_eq!(get(&db, film.id).await.unwrap(), None);
    }
}
