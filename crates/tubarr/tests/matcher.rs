//! The show matcher on a realistic ARY upload list: traps, re-uploads, parts, pins, idempotency.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Mutex,
    time::{Duration, SystemTime},
};

use tubarr::{
    matcher::{
        Candidate, Candidates, Known, Metadata, Report, Wanted, WantedEpisode, WantedSeries,
        match_shows,
    },
    resolver::{Resolved, StreamFormat, VideoMeta},
    store::{
        Db, EpisodeFilter, Mapping, MappingKind, NewMapping, PinTarget, StoreError, Unmatched,
        UnmatchedState,
    },
};

const ARY: &str = "UCaryDigitalOfficial000";
const ARY_ZINDAGI: &str = "UCaryZindagiOfficial000";
const OTHER: &str = "UCrandomReuploader00000";
const MIN: i64 = 60;
const DAY: i64 = 86_400;
/// 2024-07-01 00:00 UTC.
const T0: i64 = 1_719_792_000;

#[derive(Default)]
struct FakeCandidates(Mutex<Vec<Candidate>>);

impl Candidates for FakeCandidates {
    async fn on_channels(&self, channel_ids: &[String]) -> Result<Vec<Candidate>, StoreError> {
        let all = self.0.lock().unwrap().clone();
        Ok(all
            .into_iter()
            .filter(|c| channel_ids.contains(&c.channel_id))
            .collect())
    }
}

struct FakeWanted(Vec<WantedSeries>);

impl Wanted for FakeWanted {
    async fn series(&self) -> Result<Vec<WantedSeries>, StoreError> {
        Ok(self.0.clone())
    }
}

#[derive(Default)]
struct FakeMetadata(Mutex<HashMap<String, Known>>);

impl FakeMetadata {
    fn set(&self, id: &str, known: Known) {
        self.0.lock().unwrap().insert(id.to_owned(), known);
    }
    fn height(&self, id: &str, height: i64) {
        self.set(
            id,
            Known {
                height: Some(height),
                ..Known::default()
            },
        );
    }
}

impl Metadata for FakeMetadata {
    async fn known(&self, video_id: &str) -> Result<Known, StoreError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .get(video_id)
            .cloned()
            .unwrap_or_default())
    }
}

fn cand(id: &str, channel: &str, title: &str, at: i64, mins: Option<i64>) -> Candidate {
    Candidate {
        video_id: id.to_owned(),
        channel_id: channel.to_owned(),
        title: title.to_owned(),
        published_at: at,
        published_approx: false,
        duration_secs: mins.map(|m| m * MIN),
    }
}

fn kmkt(rest: &str) -> String {
    format!("Kabhi Main Kabhi Tum - {rest}")
}

/// What the watcher would have found on the ARY channels, traps included.
fn uploads() -> Vec<Candidate> {
    let day = |n: i64| T0 + n * DAY;
    vec![
        cand(
            "ep01",
            ARY,
            &kmkt("Episode 1 | Fahad Mustafa | Hania Aamir | ARY Digital"),
            day(1),
            Some(38),
        ),
        cand(
            "ep02",
            ARY,
            "Kabhi Main Kabhi Tum Ep 2 | ARY Digital",
            day(2),
            Some(37),
        ),
        // Episode 25: sponsor variant (winner), an earlier Eng Sub upload, an even earlier
        // re-upload on the second official channel, and the extras.
        cand(
            "ep25",
            ARY,
            &kmkt("Episode 25 - Presented by Happilac Paints"),
            day(25),
            Some(40),
        ),
        cand(
            "ep25sub",
            ARY,
            &kmkt("Episode 25 - [Eng Sub] - Presented by Happilac Paints"),
            day(25) - 3600,
            Some(40),
        ),
        cand(
            "ep25zin",
            ARY_ZINDAGI,
            &kmkt("Episode 25"),
            day(25) - 7200,
            Some(40),
        ),
        cand(
            "ep25promo",
            ARY,
            &kmkt("Episode 25 Promo | ARY"),
            day(24),
            Some(1),
        ),
        cand(
            "ep25teaser",
            ARY,
            &kmkt("Episode 25 Teaser"),
            day(23),
            Some(1),
        ),
        cand(
            "ep25hl",
            ARY,
            &kmkt("Episode 25 | Highlights"),
            day(26),
            Some(16),
        ),
        // A short upload next to a full one: an extra without the word, dropped quietly.
        cand("ep25short", ARY, &kmkt("Episode 25"), day(26), Some(9)),
        cand(
            "ep2425",
            ARY,
            &kmkt("Episode 24 & 25 | ARY Digital"),
            day(24),
            Some(75),
        ),
        cand(
            "last",
            ARY,
            &kmkt("Last Episode | ARY Digital"),
            day(32),
            Some(70),
        ),
        // Episode 26 only in two parts.
        cand(
            "ep26p1",
            ARY,
            &kmkt("Episode 26 - Part 1"),
            day(26),
            Some(20),
        ),
        cand(
            "ep26p2",
            ARY,
            &kmkt("Episode 26 - Part 2"),
            day(26),
            Some(20),
        ),
        // Episode 24 in parts and in full: the full one wins, the parts get no note.
        cand("ep24", ARY, &kmkt("Episode 24"), day(24), Some(39)),
        cand(
            "ep24p1",
            ARY,
            &kmkt("Episode 24 - Part 1"),
            day(24),
            Some(20),
        ),
        cand("ep27", ARY, &kmkt("Episode 27"), day(27), None),
        cand("ep28", ARY, &kmkt("Episode 28"), day(28), Some(38)),
        cand("ep29", ARY, &kmkt("Episode 29"), day(29), Some(18)),
        cand("ep30", ARY, &kmkt("Episode 30"), day(30), Some(38)),
        cand(
            "other",
            ARY,
            "Teri Chhaon Mein - Episode 25 | ARY Digital",
            day(25),
            Some(38),
        ),
        cand("offchan", OTHER, &kmkt("Episode 3"), day(3), Some(38)),
    ]
}

fn wanted_episodes() -> Vec<WantedEpisode> {
    (1..=29)
        .map(|episode| WantedEpisode {
            season: 1,
            episode,
            air_date: None,
            title: None,
            monitored: true,
        })
        .collect()
}

fn metadata() -> FakeMetadata {
    let meta = FakeMetadata::default();
    for id in [
        "ep01", "ep02", "ep24", "ep25", "ep25sub", "ep25zin", "ep26p1", "ep26p2", "ep30",
    ] {
        meta.height(id, 1080);
    }
    // ep28: duration known, height not yet.
    meta
}

struct Fixture {
    db: Db,
    _dir: tempfile::TempDir,
    mapping: Mapping,
    candidates: FakeCandidates,
    meta: FakeMetadata,
}

impl Fixture {
    async fn new() -> Self {
        Self::with(|_| {}).await
    }

    async fn with(edit: impl FnOnce(&mut NewMapping)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("tubarr.db")).await.unwrap();
        let mut new = NewMapping {
            kind: MappingKind::Show,
            tvdb_id: Some(452_001),
            tmdb_id: None,
            title: "Kabhi Main Kabhi Tum".to_owned(),
            language: "Urdu".to_owned(),
            channel_ids: vec![ARY.to_owned(), ARY_ZINDAGI.to_owned()],
            episode_regex: None,
            excludes: vec![],
            min_duration_secs: None,
        };
        edit(&mut new);
        let mapping = db.insert_mapping(&new).await.unwrap();
        Self {
            db,
            _dir: dir,
            mapping,
            candidates: FakeCandidates(Mutex::new(uploads())),
            meta: metadata(),
        }
    }

    fn wanted(&self) -> FakeWanted {
        self.wanted_with(wanted_episodes())
    }

    fn wanted_with(&self, episodes: Vec<WantedEpisode>) -> FakeWanted {
        FakeWanted(vec![WantedSeries {
            mapping: self.mapping.clone(),
            episodes,
        }])
    }

    async fn run(&self) -> Report {
        match_shows(&self.db, &self.candidates, &self.wanted(), &self.meta)
            .await
            .unwrap()
    }

    async fn run_with(&self, episodes: Vec<WantedEpisode>) -> Report {
        match_shows(
            &self.db,
            &self.candidates,
            &self.wanted_with(episodes),
            &self.meta,
        )
        .await
        .unwrap()
    }

    /// (season, episode) to video ids, sorted.
    async fn releases(&self) -> BTreeMap<(i64, i64), Vec<String>> {
        let mut out: BTreeMap<(i64, i64), Vec<String>> = BTreeMap::new();
        for r in self
            .db
            .releases_for_mappings(&[self.mapping.id], &EpisodeFilter::Any, 1000)
            .await
            .unwrap()
        {
            let PinTarget::Episode { season, episode } = r.target else {
                panic!("show release without an episode: {r:?}");
            };
            out.entry((season, episode)).or_default().push(r.key());
        }
        for ids in out.values_mut() {
            ids.sort();
        }
        out
    }

    async fn notes(&self) -> HashMap<String, Unmatched> {
        self.db
            .unmatched(self.mapping.id)
            .await
            .unwrap()
            .into_iter()
            .map(|n| (n.video_id.clone(), n))
            .collect()
    }
}

fn one(season: i64, episode: i64, id: &str) -> ((i64, i64), Vec<String>) {
    ((season, episode), vec![id.to_owned()])
}

#[tokio::test]
async fn realistic_list_releases_only_confident_matches() {
    let f = Fixture::new().await;
    let report = f.run().await;

    assert_eq!(
        f.releases().await,
        BTreeMap::from([
            one(1, 1, "ep01"),
            one(1, 2, "ep02"),
            one(1, 24, "ep24"),
            one(1, 25, "ep25"),
            one(1, 26, "ep26p1,ep26p2"),
        ])
    );
    assert_eq!(report.released, 5);
    assert_eq!(report.replaced, 0);
    assert_eq!(report.not_wanted, 1, "episode 30 is not wanted");
    assert!(report.skipped.is_empty());

    let ep25 = f.db.release_by_video("ep25").await.unwrap().unwrap();
    assert_eq!(ep25.language, "Urdu");
    assert_eq!(ep25.height, 1080);
    assert_eq!(
        ep25.video_title,
        kmkt("Episode 25 - Presented by Happilac Paints")
    );
    assert_eq!(ep25.size, None);

    let notes = f.notes().await;
    let state = |id: &str| notes.get(id).map(|n| n.state);
    assert_eq!(state("ep25sub"), Some(UnmatchedState::Duplicate));
    assert_eq!(state("ep25zin"), Some(UnmatchedState::Duplicate));
    assert!(notes["ep25zin"].reason.contains("kept ep25"));
    assert_eq!(state("ep2425"), Some(UnmatchedState::Unsure));
    assert!(
        notes["ep2425"].reason.contains("24 and 25"),
        "{}",
        notes["ep2425"].reason
    );
    assert_eq!(state("last"), Some(UnmatchedState::Unsure));
    // Episode 26 came in two parts: one release, part 1 first, no notes.
    let ep26 =
        f.db.release_by_video("ep26p1,ep26p2")
            .await
            .unwrap()
            .unwrap();
    assert_eq!(ep26.video_id, "ep26p1");
    assert_eq!(ep26.part_ids, ["ep26p1", "ep26p2"]);
    assert_eq!(ep26.video_title, kmkt("Episode 26 - Part 1"));
    assert_eq!(f.db.release_by_video("ep26p1").await.unwrap(), None);
    assert_eq!(state("ep26p1"), None);
    assert_eq!(state("ep26p2"), None);
    assert_eq!(state("ep27"), Some(UnmatchedState::Pending));
    assert!(notes["ep27"].reason.contains("duration"));
    assert_eq!(state("ep28"), Some(UnmatchedState::Pending));
    assert!(notes["ep28"].reason.contains("height"));
    assert_eq!(state("ep29"), Some(UnmatchedState::Unsure));
    assert!(
        notes["ep29"].reason.contains("18 min"),
        "{}",
        notes["ep29"].reason
    );
    // Extras, other shows, other channels, unwanted episodes and parts of a settled episode
    // leave no note.
    for quiet in [
        "ep25promo",
        "ep25teaser",
        "ep25hl",
        "ep25short",
        "ep24p1",
        "ep30",
        "other",
        "offchan",
    ] {
        assert_eq!(state(quiet), None, "{quiet}");
    }
}

#[tokio::test]
async fn running_twice_adds_nothing() {
    let f = Fixture::new().await;
    f.run().await;
    let releases = f.releases().await;
    let notes = f.notes().await;

    let again = f.run().await;
    assert_eq!(
        again,
        // `match_shows` has no input digest (that lives in the background run), so the
        // mapping counts as matched again but writes nothing.
        Report {
            matched: 1,
            not_wanted: 1,
            ..Report::default()
        }
    );
    assert_eq!(f.releases().await, releases);
    assert_eq!(f.notes().await, notes);
}

#[tokio::test]
async fn pending_videos_match_once_metadata_arrives() {
    let f = Fixture::new().await;
    f.run().await;
    f.meta.set(
        "ep27",
        Known {
            duration_secs: Some(39 * MIN),
            height: Some(720),
            ..Known::default()
        },
    );
    f.meta.height("ep28", 1080);

    let report = f.run().await;
    assert_eq!(report.released, 2);
    let releases = f.releases().await;
    assert_eq!(releases[&(1, 27)], ["ep27"]);
    assert_eq!(releases[&(1, 28)], ["ep28"]);
    assert_eq!(
        f.db.release_by_video("ep27").await.unwrap().unwrap().height,
        720
    );
    let notes = f.notes().await;
    assert!(!notes.contains_key("ep27") && !notes.contains_key("ep28"));
}

#[tokio::test]
async fn a_later_pin_replaces_the_automatic_match() {
    let f = Fixture::new().await;
    f.run().await;
    let ep25 = PinTarget::Episode {
        season: 1,
        episode: 25,
    };
    f.db.set_pin(f.mapping.id, &ep25, "ep25sub").await.unwrap();

    let report = f.run().await;
    assert_eq!(report.replaced, 1);
    assert_eq!(report.released, 1);
    assert_eq!(f.releases().await[&(1, 25)], ["ep25sub"]);
    let notes = f.notes().await;
    assert!(!notes.contains_key("ep25sub"));
    assert_eq!(notes["ep25"].state, UnmatchedState::Duplicate);
    assert!(notes["ep25"].reason.contains("pinned"));

    // And it sticks.
    assert_eq!(f.run().await.released, 0);
    assert_eq!(f.releases().await[&(1, 25)], ["ep25sub"]);
}

#[tokio::test]
async fn pins_override_the_rules() {
    let f = Fixture::new().await;
    // The joint upload pinned to both of its episodes, a promo pinned by mistake still wins, and
    // an auto matched video moved to another episode.
    for (episode, video) in [
        (24, "ep2425"),
        (25, "ep2425"),
        (29, "ep25promo"),
        (3, "ep02"),
    ] {
        let target = PinTarget::Episode { season: 1, episode };
        f.db.set_pin(f.mapping.id, &target, video).await.unwrap();
    }
    for id in ["ep2425", "ep25promo"] {
        f.meta.height(id, 1080);
    }
    f.run().await;
    let releases = f.releases().await;
    assert_eq!(releases[&(1, 24)], ["ep2425"]);
    assert_eq!(releases[&(1, 25)], ["ep2425"]);
    assert_eq!(releases[&(1, 29)], ["ep25promo"]);
    assert_eq!(releases[&(1, 3)], ["ep02"]);
    assert!(
        !releases.contains_key(&(1, 2)),
        "ep02 belongs to its pin only"
    );
    assert!(!f.notes().await.contains_key("ep2425"));
}

#[tokio::test]
async fn a_pin_moves_an_existing_release() {
    let f = Fixture::new().await;
    f.run().await;
    assert_eq!(f.releases().await[&(1, 2)], ["ep02"]);
    let ep3 = PinTarget::Episode {
        season: 1,
        episode: 3,
    };
    f.db.set_pin(f.mapping.id, &ep3, "ep02").await.unwrap();
    let report = f.run().await;
    assert_eq!((report.replaced, report.released), (1, 1));
    let releases = f.releases().await;
    assert!(!releases.contains_key(&(1, 2)));
    assert_eq!(releases[&(1, 3)], ["ep02"]);
}

#[tokio::test]
async fn a_pin_to_an_unseen_video_uses_kept_metadata_or_waits() {
    let f = Fixture::new().await;
    let pin = |episode| PinTarget::Episode { season: 1, episode };
    f.db.set_pin(f.mapping.id, &pin(31), "elsewhere1")
        .await
        .unwrap();
    f.db.set_pin(f.mapping.id, &pin(32), "elsewhere2")
        .await
        .unwrap();
    f.meta.set(
        "elsewhere1",
        Known {
            title: Some("KMKT 31 full".to_owned()),
            duration_secs: Some(40 * MIN),
            published_at: Some(T0),
            height: Some(1080),
            size: None,
        },
    );
    f.run().await;
    let releases = f.releases().await;
    assert_eq!(releases[&(1, 31)], ["elsewhere1"]);
    assert!(!releases.contains_key(&(1, 32)));
    let notes = f.notes().await;
    assert_eq!(notes["elsewhere2"].state, UnmatchedState::Pending);
    assert_eq!(notes["elsewhere2"].episode, Some(32));
}

#[tokio::test]
async fn first_channel_and_clean_upload_win_over_earlier_ones() {
    let f = Fixture::new().await;
    // Only the second channel's upload and the Eng Sub one: the first channel still wins.
    f.candidates
        .0
        .lock()
        .unwrap()
        .retain(|c| c.video_id != "ep25");
    f.run().await;
    assert_eq!(f.releases().await[&(1, 25)], ["ep25sub"]);

    // The clean upload appears later: it takes the episode over.
    f.candidates.0.lock().unwrap().push(cand(
        "ep25clean",
        ARY,
        &kmkt("Episode 25"),
        T0 + 40 * DAY,
        Some(40),
    ));
    f.meta.height("ep25clean", 1080);
    let report = f.run().await;
    assert_eq!((report.replaced, report.released), (1, 1));
    assert_eq!(f.releases().await[&(1, 25)], ["ep25clean"]);
    assert_eq!(f.notes().await["ep25sub"].state, UnmatchedState::Duplicate);
}

#[tokio::test]
async fn upload_long_before_the_air_date_is_unsure() {
    let f = Fixture::new().await;
    let mut episodes = wanted_episodes();
    // TVDB says episode 1 aired a year after the upload: probably a different season.
    episodes[0].air_date = Some("2025-07-02".to_owned());
    // Episode 2 aired the day after its upload: fine.
    episodes[1].air_date = Some("2024-07-04".to_owned());
    match_shows(&f.db, &f.candidates, &f.wanted_with(episodes), &f.meta)
        .await
        .unwrap();
    let releases = f.releases().await;
    assert!(!releases.contains_key(&(1, 1)));
    assert_eq!(releases[&(1, 2)], ["ep02"]);
    let notes = f.notes().await;
    assert_eq!(notes["ep01"].state, UnmatchedState::Unsure);
    assert!(notes["ep01"].reason.contains("aired"));
}

#[tokio::test]
async fn mapping_rules_floor_and_bad_regex() {
    let f = Fixture::with(|m| m.min_duration_secs = Some(15 * MIN)).await;
    f.meta.height("ep29", 1080);
    f.run().await;
    assert_eq!(f.releases().await[&(1, 29)], ["ep29"]);

    let bad = Fixture::with(|m| m.episode_regex = Some(r"Episode (\d+)".to_owned())).await;
    let report = bad.run().await;
    assert_eq!(report.skipped.len(), 1);
    assert!(report.skipped[0].1.contains("named group"));
    assert!(bad.releases().await.is_empty());
}

#[tokio::test]
async fn other_kinds_are_left_alone() {
    let f = Fixture::with(|m| {
        m.kind = MappingKind::Film;
        m.tvdb_id = None;
        m.tmdb_id = Some(352_173);
    })
    .await;
    let report = f.run().await;
    assert_eq!(report, Report::default());
    assert!(f.releases().await.is_empty());
}

#[tokio::test]
async fn the_database_supplies_kept_metadata_and_height() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("tubarr.db")).await.unwrap();
    assert_eq!(db.known("nothing").await.unwrap(), Known::default());

    let stream = |id: &str, height| StreamFormat {
        format_id: id.to_owned(),
        url: format!("https://rr1.googlevideo.com/videoplayback?itag={id}"),
        ext: Some("mp4".to_owned()),
        width: None,
        height,
        filesize: None,
        filesize_approx: None,
        http_headers: BTreeMap::new(),
    };
    let resolved = Resolved {
        meta: VideoMeta {
            id: "0eD-GQCutAU".to_owned(),
            title: kmkt("Episode 1"),
            duration: Some(2101),
            upload_date: Some("20240701".to_owned()),
            timestamp: None,
            channel_id: Some(ARY.to_owned()),
        },
        video: stream("137", Some(1080)),
        audio: stream("140", None),
        subtitles: vec![],
        expires_at: Some(SystemTime::now() + Duration::from_secs(6 * 3600)),
    };
    let now = tubarr::store::unix_now();
    db.store_resolved(&resolved, now, now + 3600).await.unwrap();
    assert_eq!(
        db.known("0eD-GQCutAU").await.unwrap(),
        Known {
            title: Some(kmkt("Episode 1")),
            duration_secs: Some(2101),
            published_at: Some(T0),
            height: Some(1080),
            size: None,
        }
    );

    // The height is kept with the metadata, so it outlives the streams.
    db.purge_resolved(now + 7200).await.unwrap();
    let later = db.known("0eD-GQCutAU").await.unwrap();
    assert_eq!(later.height, Some(1080));
    assert_eq!(later.duration_secs, Some(2101));
}

#[tokio::test]
async fn a_missing_part_waits_then_the_parts_release_together() {
    let f = Fixture::new().await;
    f.candidates
        .0
        .lock()
        .unwrap()
        .retain(|c| c.video_id != "ep26p2");
    for (id, size) in [("ep26p1", 300_000_000), ("ep26p2", 320_000_000)] {
        f.meta.set(
            id,
            Known {
                height: Some(720),
                size: Some(size),
                ..Known::default()
            },
        );
    }
    f.run().await;
    assert!(!f.releases().await.contains_key(&(1, 26)));
    let notes = f.notes().await;
    assert_eq!(notes["ep26p1"].state, UnmatchedState::Partial);
    assert!(
        notes["ep26p1"].reason.contains("waiting for part 2"),
        "{}",
        notes["ep26p1"].reason
    );

    f.candidates.0.lock().unwrap().push(cand(
        "ep26p2",
        ARY,
        &kmkt("Episode 26 - Part 2"),
        T0 + 27 * DAY,
        Some(20),
    ));
    let report = f.run().await;
    assert_eq!(report.released, 1);
    assert_eq!(f.releases().await[&(1, 26)], ["ep26p1,ep26p2"]);
    let joined =
        f.db.release_by_video("ep26p1,ep26p2")
            .await
            .unwrap()
            .unwrap();
    // Sizes add up, the lowest part height counts and the release dates from its last part.
    assert_eq!(joined.size, Some(620_000_000));
    assert_eq!(joined.height, 720);
    assert_eq!(joined.published_at, T0 + 27 * DAY);
    let notes = f.notes().await;
    assert!(!notes.contains_key("ep26p1") && !notes.contains_key("ep26p2"));
}

#[tokio::test]
async fn a_full_upload_beats_the_parts() {
    let f = Fixture::new().await;
    f.candidates.0.lock().unwrap().push(cand(
        "ep26",
        ARY,
        &kmkt("Episode 26"),
        T0 + 27 * DAY,
        Some(40),
    ));
    f.meta.height("ep26", 1080);
    f.run().await;
    assert_eq!(f.releases().await[&(1, 26)], ["ep26"]);
    let notes = f.notes().await;
    assert!(!notes.contains_key("ep26p1") && !notes.contains_key("ep26p2"));
}

#[tokio::test]
async fn a_pin_can_name_the_parts() {
    let f = Fixture::new().await;
    // A full upload exists, but the pin picks the parts.
    f.candidates.0.lock().unwrap().push(cand(
        "ep26",
        ARY,
        &kmkt("Episode 26"),
        T0 + 27 * DAY,
        Some(40),
    ));
    f.meta.height("ep26", 1080);
    let target = PinTarget::Episode {
        season: 1,
        episode: 26,
    };
    f.db.set_pin(f.mapping.id, &target, "ep26p1,ep26p2")
        .await
        .unwrap();
    f.run().await;
    assert_eq!(f.releases().await[&(1, 26)], ["ep26p1,ep26p2"]);
}

#[tokio::test]
async fn an_alias_names_the_show_and_the_release_keeps_the_arr_title() {
    // TVDB calls it "The Tale", the channel uses another name.
    let f = Fixture::with(|m| m.title = "The Tale".to_owned()).await;
    f.run().await;
    assert!(f.releases().await.is_empty());

    f.db.set_aliases(f.mapping.id, &["Kabhi Main Kabhi Tum".to_owned()])
        .await
        .unwrap();
    f.run().await;
    let releases = f.releases().await;
    assert_eq!(releases.get(&(1, 1)), Some(&vec!["ep01".to_owned()]));
    assert_eq!(releases.get(&(1, 25)), Some(&vec!["ep25".to_owned()]));
    let ep01 = f.db.release_by_video("ep01").await.unwrap().unwrap();
    assert_eq!(ep01.arr_title, "The Tale");
    let name = tubarr::newznab::title::release_title(
        &ep01.arr_title,
        tubarr::newznab::title::TitleTag::Episode {
            season: 1,
            episode: 1,
        },
        ep01.height,
        &ep01.language,
    );
    assert!(name.starts_with("The.Tale.S01E01."), "{name}");
}

#[tokio::test]
async fn a_rerun_counter_on_a_show_numbered_by_year_is_unsure() {
    // Sony PAL counts CID uploads by reruns, TVDB numbers it S1998E01 and on.
    let f = Fixture::with(|m| m.title = "CID".to_owned()).await;
    *f.candidates.0.lock().unwrap() = vec![
        cand(
            "cid1534",
            ARY,
            "CID - Episode 1534 | Sony PAL",
            T0,
            Some(42),
        ),
        cand("cid5", ARY, "CID - Episode 5 | Sony PAL", T0, Some(42)),
    ];
    f.meta.height("cid1534", 1080);
    f.meta.height("cid5", 1080);
    let wanted = f.wanted_with(
        (1..=10)
            .map(|episode| WantedEpisode {
                season: 1998,
                episode,
                air_date: None,
                title: None,
                monitored: true,
            })
            .collect(),
    );
    let report = match_shows(&f.db, &f.candidates, &wanted, &f.meta)
        .await
        .unwrap();
    assert_eq!(report.released, 0);
    assert!(f.releases().await.is_empty());
    let notes = f.notes().await;
    for id in ["cid1534", "cid5"] {
        assert_eq!(notes[id].state, UnmatchedState::Unsure, "{id}");
        assert!(
            notes[id].reason.contains("by year (1998 to 1998)"),
            "{}",
            notes[id].reason
        );
    }
}

/// Fixture for a web series: one channel, the given uploads, every one 1080p.
async fn web_series(title: &str, uploads: Vec<Candidate>) -> Fixture {
    let f = Fixture::with(|m| m.title = title.to_owned()).await;
    for c in &uploads {
        f.meta.height(&c.video_id, 1080);
    }
    *f.candidates.0.lock().unwrap() = uploads;
    f
}

fn wanted_season(season: i64, last: i64) -> Vec<WantedEpisode> {
    (1..=last)
        .map(|episode| WantedEpisode {
            season,
            episode,
            air_date: None,
            title: None,
            monitored: true,
        })
        .collect()
}

#[tokio::test]
async fn parts_that_count_numbered_episodes_release_each_episode() {
    // Very Parivarik S2: a three episode arc, each episode its own number and its own "Part N".
    let f = web_series(
        "Very Parivarik",
        vec![
            cand(
                "vp6",
                ARY,
                "Very Parivarik S2 | EP6 | Shaadi in the Village (Part 1)",
                T0,
                Some(30),
            ),
            cand(
                "vp7",
                ARY,
                "Very Parivarik S2 | EP7 | Shaadi in the Village (Part 2)",
                T0 + 7 * DAY,
                Some(31),
            ),
            cand(
                "vp8",
                ARY,
                "Very Parivarik S2 | EP8 | Shaadi (Part 3)",
                T0 + 14 * DAY,
                Some(29),
            ),
        ],
    )
    .await;
    let wanted = f.wanted_with(wanted_season(2, 8));
    let report = match_shows(&f.db, &f.candidates, &wanted, &f.meta)
        .await
        .unwrap();
    assert_eq!(report.released, 3);
    assert_eq!(
        f.releases().await,
        BTreeMap::from([one(2, 6, "vp6"), one(2, 7, "vp7"), one(2, 8, "vp8")])
    );
    assert!(f.notes().await.is_empty());
}

#[tokio::test]
async fn web_series_titles_release_by_season_and_episode() {
    let f = web_series(
        "Kota Factory",
        vec![
            cand(
                "kf1",
                ARY,
                "Kota Factory - S01 E01 - Inventory",
                T0,
                Some(40),
            ),
            cand(
                "kf2",
                ARY,
                "Kota Factory | S01E02 - Jee Mains",
                T0 + DAY,
                Some(38),
            ),
            cand(
                "kf5",
                ARY,
                "Kota Factory - S2E05 - Rasoi",
                T0 + 2 * DAY,
                Some(41),
            ),
            cand(
                "kftr",
                ARY,
                "Kota Factory Season 3 | Official Trailer",
                T0,
                Some(2),
            ),
            cand(
                "kfbts",
                ARY,
                "Kota Factory | Behind the Scenes",
                T0,
                Some(30),
            ),
            cand(
                "kfclip",
                ARY,
                "Kota Factory S01E01 Clip | Jeetu",
                T0,
                Some(5),
            ),
            cand(
                "kfrecap",
                ARY,
                "Kota Factory S01 Recap | 1080p",
                T0,
                Some(30),
            ),
        ],
    )
    .await;
    let mut episodes = wanted_season(1, 5);
    episodes.extend(wanted_season(2, 5));
    let wanted = f.wanted_with(episodes);
    match_shows(&f.db, &f.candidates, &wanted, &f.meta)
        .await
        .unwrap();
    assert_eq!(
        f.releases().await,
        BTreeMap::from([one(1, 1, "kf1"), one(1, 2, "kf2"), one(2, 5, "kf5")])
    );
    let notes = f.notes().await;
    // The trailer is excluded. Behind the scenes and the recap carry no number, the clip is
    // short next to a full upload: none of them is released.
    assert!(!notes.contains_key("kftr") && !notes.contains_key("kfclip"));
    assert_eq!(notes["kfbts"].state, UnmatchedState::Unsure);
    assert_eq!(notes["kfrecap"].state, UnmatchedState::Unsure);
}

#[tokio::test]
async fn episode_n_of_t_is_one_episode() {
    let f = web_series(
        "Please Find Attached",
        (1..=3)
            .map(|n| {
                cand(
                    &format!("pfa{n}"),
                    ARY,
                    &format!("Please Find Attached | Ep {n}/3 | Girliyapa"),
                    T0 + n * DAY,
                    Some(30),
                )
            })
            .collect(),
    )
    .await;
    let wanted = f.wanted_with(wanted_season(1, 3));
    match_shows(&f.db, &f.candidates, &wanted, &f.meta)
        .await
        .unwrap();
    assert_eq!(
        f.releases().await,
        BTreeMap::from([one(1, 1, "pfa1"), one(1, 2, "pfa2"), one(1, 3, "pfa3")])
    );
    assert!(f.notes().await.is_empty());
}

#[tokio::test]
async fn a_slash_on_a_long_season_stays_a_joint_upload() {
    let f = Fixture::new().await;
    *f.candidates.0.lock().unwrap() = vec![cand(
        "ep2425",
        ARY,
        &kmkt("Episode 24/25 | ARY Digital"),
        T0,
        Some(80),
    )];
    f.meta.height("ep2425", 1080);
    let wanted = f.wanted_with(wanted_season(1, 40));
    let report = match_shows(&f.db, &f.candidates, &wanted, &f.meta)
        .await
        .unwrap();
    assert_eq!(report.released, 0);
    let notes = f.notes().await;
    assert_eq!(notes["ep2425"].state, UnmatchedState::Unsure);
    assert!(
        notes["ep2425"].reason.contains("episodes 24 and 25"),
        "{}",
        notes["ep2425"].reason
    );
}

fn episodes(numbers: impl IntoIterator<Item = i64>) -> Vec<WantedEpisode> {
    numbers
        .into_iter()
        .map(|episode| WantedEpisode {
            season: 1,
            episode,
            air_date: None,
            title: None,
            monitored: true,
        })
        .collect()
}

#[tokio::test]
async fn an_episode_tvdb_drops_or_renumbers_loses_its_release() {
    let f = Fixture::new().await;
    let at = |n: i64| T0 + n * DAY;
    *f.candidates.0.lock().unwrap() = vec![
        cand("e09", ARY, &kmkt("Episode 9"), at(9), Some(38)),
        cand("e10", ARY, &kmkt("Episode 10"), at(10), Some(38)),
    ];
    for id in ["e09", "e10"] {
        f.meta.height(id, 1080);
    }
    f.run_with(episodes(1..=10)).await;
    assert_eq!(
        f.releases().await,
        BTreeMap::from([one(1, 9, "e09"), one(1, 10, "e10")])
    );

    // TVDB deletes E10: its release goes.
    let report = f.run_with(episodes(1..=9)).await;
    assert_eq!(f.releases().await, BTreeMap::from([one(1, 9, "e09")]));
    assert_eq!((report.released, report.replaced), (0, 1));

    // An empty Sonarr list says nothing yet: nothing is retired.
    f.run_with(vec![]).await;
    assert_eq!(f.releases().await, BTreeMap::from([one(1, 9, "e09")]));

    // E10 is back, then TVDB inserts a new E10 and the old one becomes E11, and the channel
    // renames its upload to match: it moves to E11 and keeps no release on E10.
    f.run_with(episodes(1..=10)).await;
    *f.candidates.0.lock().unwrap() = vec![
        cand("e09", ARY, &kmkt("Episode 9"), at(9), Some(38)),
        cand("e10", ARY, &kmkt("Episode 11"), at(10), Some(38)),
    ];
    let report = f.run_with(episodes(1..=11)).await;
    assert_eq!(
        f.releases().await,
        BTreeMap::from([one(1, 9, "e09"), one(1, 11, "e10")])
    );
    assert_eq!((report.released, report.replaced), (1, 1));

    // A pinned episode TVDB no longer lists keeps its release.
    let e09 = PinTarget::Episode {
        season: 1,
        episode: 9,
    };
    f.db.set_pin(f.mapping.id, &e09, "e09").await.unwrap();
    f.run_with(episodes([1, 11])).await;
    assert_eq!(
        f.releases().await,
        BTreeMap::from([one(1, 9, "e09"), one(1, 11, "e10")])
    );
}
