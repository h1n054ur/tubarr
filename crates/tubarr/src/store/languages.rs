//! Who set a mapping's language, and the language a show takes from its channel.
//!
//! Sonarr has no Urdu and reports Pakistani dramas as English. Release names should still say
//! URDU (Sonarr reads an unknown language word as Unknown and falls back to the series language,
//! so the name is accepted as before). A show whose language is still the arr's English or
//! Unknown takes the `[channels]` language of its first channel. A language the user set is never
//! touched. Films keep the arr's language: their dubs come from title words per release.

use super::{Db, StoreError, unix_now};
use crate::channels::Channels;

/// Where a mapping's language came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, serde::Serialize)]
#[sqlx(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum LanguageSource {
    /// The arr's original language, written when the sync created the row.
    Arr,
    /// The configured language of the mapping's first channel.
    Channel,
    /// Set in the admin UI or with a manual mapping.
    User,
}

/// Rows that may follow their channel: shows on the arr's default, and shows that already did.
const FOLLOWERS: &str = "SELECT id, language, channel_ids FROM mappings WHERE kind <> 'film' \
     AND (language_source = 'channel' \
       OR (language_source = 'arr' AND lower(language) IN ('english', 'unknown'))) \
     AND (?1 IS NULL OR id = ?1)";

impl Db {
    /// Gives every show that follows its channel (all of them, or only `id`) the configured
    /// language of its first channel, and renames its releases still on the old language.
    /// Returns how many mappings changed.
    pub async fn languages_from_channels(
        &self,
        channels: &Channels,
        id: Option<i64>,
    ) -> Result<u64, StoreError> {
        if channels.is_empty() {
            return Ok(0);
        }
        let rows: Vec<(i64, String, sqlx::types::Json<Vec<String>>)> = sqlx::query_as(FOLLOWERS)
            .bind(id)
            .fetch_all(&self.pool)
            .await?;
        let now = unix_now();
        let mut changed = 0;
        for (id, old, channel_ids) in rows {
            let Some(language) = channel_ids.first().and_then(|c| channels.language(c)) else {
                continue;
            };
            if language == old {
                continue;
            }
            let mut tx = self.pool.begin().await?;
            sqlx::query(
                "UPDATE mappings SET language = ?, language_source = 'channel', updated_at = ? \
                 WHERE id = ?",
            )
            .bind(language)
            .bind(now)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            sqlx::query("UPDATE releases SET language = ? WHERE mapping_id = ? AND language = ?")
                .bind(language)
                .bind(id)
                .bind(&old)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            tracing::info!(mapping = id, from = %old, to = %language, "language taken from the channel");
            changed += 1;
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::{
        channels::{ChannelInfo, from_file},
        store::{ArrEntry, ArrSource, MappingKind, test_db},
    };

    const ARY: &str = "UC4JCksJF76g_MdzPVBJoC3Q";

    fn entry(kind: MappingKind, key: i64, language: &str) -> ArrEntry {
        ArrEntry {
            kind,
            key,
            arr_id: key,
            title: format!("Title {key}"),
            language: language.to_owned(),
            year: None,
            imdb_id: None,
            aliases: vec![],
        }
    }

    #[tokio::test]
    async fn only_shows_on_english_or_unknown_follow_their_channel() {
        let (db, _dir) = test_db::fresh().await;
        let shows = [
            entry(MappingKind::Show, 1, "English"),
            entry(MappingKind::Daily, 2, "Unknown"),
            entry(MappingKind::Show, 3, "Hindi"),
        ];
        db.sync_arr_mappings(ArrSource::Sonarr, &shows)
            .await
            .unwrap();
        let film = [entry(MappingKind::Film, 4, "English")];
        db.sync_arr_mappings(ArrSource::Radarr, &film)
            .await
            .unwrap();
        sqlx::query("UPDATE mappings SET channel_ids = ?")
            .bind(format!("[\"{ARY}\"]"))
            .execute(db.pool())
            .await
            .unwrap();
        let info = ChannelInfo {
            language: "Urdu".to_owned(),
            ..ChannelInfo::default()
        };
        let channels =
            from_file(BTreeMap::from([(ARY.to_owned(), info)]), BTreeMap::new()).unwrap();

        assert_eq!(
            db.languages_from_channels(&channels, None).await.unwrap(),
            2
        );
        let mut languages: Vec<(Option<i64>, Option<i64>, String)> = db
            .mappings()
            .await
            .unwrap()
            .into_iter()
            .map(|m| (m.tvdb_id, m.tmdb_id, m.language))
            .collect();
        languages.sort();
        assert_eq!(
            languages,
            [
                (None, Some(4), "English".to_owned()),
                (Some(1), None, "Urdu".to_owned()),
                (Some(2), None, "Urdu".to_owned()),
                (Some(3), None, "Hindi".to_owned()),
            ]
        );
        // Nothing left to change, and no channels configured is a no-op.
        assert_eq!(
            db.languages_from_channels(&channels, None).await.unwrap(),
            0
        );
        assert_eq!(
            db.languages_from_channels(&Channels::default(), None)
                .await
                .unwrap(),
            0
        );
    }
}
