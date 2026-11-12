//! The matcher for daily shows (by air date) and for shows with an official playlist, numbered by
//! its order or only fed by it.

use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::Mutex,
};

use tubarr::{
    matcher::{
        Candidate, Candidates, Known, Metadata, Report, Wanted, WantedEpisode, WantedSeries,
        match_shows,
    },
    store::{
        Db, EpisodeFilter, Mapping, MappingKind, MatchOptions, NewMapping, NewRelease, PinTarget,
        PlaylistMode, StoreError, Unmatched, UnmatchedState,
    },
};

const ARY_NEWS: &str = "UCaryNewsOfficial000000";
const TVF: &str = "UCtvfOfficial0000000000";
const MIN: i64 = 60;
const HOUR: i64 = 3600;
const DAY: i64 = 86_400;
/// 2026-10-01 00:00 at UTC+05:00.
const OCT1_PLUS5: i64 = 1_790_794_800;

/// Unix seconds of a UTC+05:00 wall clock time in October 2026.
fn plus5(day: i64, hour: i64, minute: i64) -> i64 {
    OCT1_PLUS5 + (day - 1) * DAY + hour * HOUR + minute * MIN
}

#[derive(Default)]
struct FakeCandidates {
    videos: Mutex<Vec<Candidate>>,
    playlists: Mutex<HashMap<String, Vec<String>>>,
}

impl Candidates for FakeCandidates {
    async fn on_channels(&self, channel_ids: &[String]) -> Result<Vec<Candidate>, StoreError> {
        let all = self.videos.lock().unwrap().clone();
        Ok(all
            .into_iter()
            .filter(|c| channel_ids.contains(&c.channel_id))
            .collect())
    }

    async fn playlist(&self, playlist_id: &str) -> Result<Option<Vec<String>>, StoreError> {
        Ok(self.playlists.lock().unwrap().get(playlist_id).cloned())
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

fn guessed(c: Candidate) -> Candidate {
    Candidate {
        published_approx: true,
        ..c
    }
}

fn hlp(rest: &str) -> String {
    format!("Har Lamha Purjosh | {rest}")
}

/// A month of a daily ARY News show, with the usual traps.
fn daily_uploads() -> Vec<Candidate> {
    vec![
        cand(
            "d01",
            ARY_NEWS,
            &hlp("1st October 2026 | ARY News"),
            plus5(1, 23, 40),
            Some(45),
        ),
        // No date in the title, up 20 minutes after midnight: the 2nd's broadcast.
        cand(
            "d02late",
            ARY_NEWS,
            &hlp("Waseem Badami | ARY News"),
            plus5(3, 0, 20),
            Some(44),
        ),
        cand(
            "d03",
            ARY_NEWS,
            &hlp("Waseem Badami | ARY News"),
            plus5(3, 21, 0),
            Some(44),
        ),
        // The same show twice on the 4th: the clean upload wins over the earlier Eng Sub one.
        cand(
            "d04",
            ARY_NEWS,
            &hlp("4th Oct 2026"),
            plus5(4, 23, 0),
            Some(45),
        ),
        cand(
            "d04sub",
            ARY_NEWS,
            &hlp("4 Oct 2026 | Eng Sub"),
            plus5(4, 22, 0),
            Some(45),
        ),
        // Title says the 5th, uploaded on the 9th: unsure.
        cand(
            "d05",
            ARY_NEWS,
            &hlp("5-10-2026"),
            plus5(9, 20, 0),
            Some(45),
        ),
        // Title and upload one day apart: the title wins.
        cand(
            "d06",
            ARY_NEWS,
            &hlp("Oct 6, 2026"),
            plus5(7, 0, 30),
            Some(45),
        ),
        cand("d07", ARY_NEWS, &hlp("07/10/26"), plus5(8, 22, 0), Some(45)),
        // Extras: a promo the excludes catch, and a short clip they miss.
        cand(
            "d08promo",
            ARY_NEWS,
            &hlp("Promo | 8 Oct 2026"),
            plus5(8, 12, 0),
            Some(1),
        ),
        cand(
            "d08clip",
            ARY_NEWS,
            &hlp("8 Oct 2026"),
            plus5(8, 23, 0),
            Some(12),
        ),
        // Two editions on the 10th: the morning one came first.
        cand(
            "d10am",
            ARY_NEWS,
            "Har Lamha Purjosh Morning Edition | 10 Oct 2026",
            plus5(10, 12, 0),
            Some(40),
        ),
        cand(
            "d10pm",
            ARY_NEWS,
            "Har Lamha Purjosh Night Edition | 10 Oct 2026",
            plus5(10, 23, 30),
            Some(45),
        ),
        // "Episode 14" is not a date: the upload date counts.
        cand(
            "d11",
            ARY_NEWS,
            "Har Lamha Purjosh Episode 14",
            plus5(11, 20, 0),
            Some(45),
        ),
        cand(
            "d12",
            ARY_NEWS,
            &hlp("12 October 2026"),
            plus5(12, 23, 0),
            None,
        ),
        cand(
            "d13",
            ARY_NEWS,
            &hlp("13 October 2026"),
            plus5(13, 23, 0),
            Some(45),
        ),
        cand(
            "d14",
            ARY_NEWS,
            &hlp("14 Oct 2026 | recap of 7 Oct 2026"),
            plus5(14, 23, 0),
            Some(45),
        ),
        // Three days late is normal for ARY, two days early is not.
        cand(
            "d15late",
            ARY_NEWS,
            &hlp("15 Oct 2026"),
            plus5(18, 20, 0),
            Some(45),
        ),
        cand(
            "d17early",
            ARY_NEWS,
            &hlp("17 Oct 2026"),
            plus5(15, 20, 0),
            Some(45),
        ),
        // Guessed upload times: the title date alone decides, with no title date nothing does.
        guessed(cand(
            "d18guess",
            ARY_NEWS,
            &hlp("ARY News"),
            plus5(18, 21, 0),
            Some(45),
        )),
        guessed(cand(
            "d19guess",
            ARY_NEWS,
            &hlp("19 Oct 2026"),
            plus5(25, 0, 0),
            Some(45),
        )),
        // Sonarr does not want the 30th.
        cand(
            "d30",
            ARY_NEWS,
            &hlp("30 Oct 2026"),
            plus5(30, 23, 0),
            Some(45),
        ),
        cand(
            "other",
            ARY_NEWS,
            "Off The Record | 1st October 2026",
            plus5(1, 23, 0),
            Some(45),
        ),
    ]
}

fn wanted_dates() -> Vec<WantedEpisode> {
    (1..=20)
        .map(|day| WantedEpisode {
            season: 2026,
            episode: day,
            air_date: Some(format!("2026-10-{day:02}")),
            title: None,
            monitored: true,
        })
        .collect()
}

struct Fixture {
    db: Db,
    dir: tempfile::TempDir,
    mapping: Mapping,
    candidates: FakeCandidates,
    meta: FakeMetadata,
    episodes: Vec<WantedEpisode>,
}

impl Fixture {
    async fn daily(edit: impl FnOnce(&mut NewMapping)) -> Self {
        let mut new = NewMapping {
            kind: MappingKind::Daily,
            tvdb_id: Some(400_100),
            tmdb_id: None,
            title: "Har Lamha Purjosh".to_owned(),
            language: "Urdu".to_owned(),
            channel_ids: vec![ARY_NEWS.to_owned()],
            episode_regex: None,
            excludes: vec![],
            min_duration_secs: None,
        };
        edit(&mut new);
        let f = Self::new(new, MatchOptions::default(), wanted_dates()).await;
        *f.candidates.videos.lock().unwrap() = daily_uploads();
        for id in [
            "d01", "d02late", "d03", "d04", "d04sub", "d06", "d07", "d11", "d12", "d15late",
            "d19guess",
        ] {
            f.meta.height(id, 1080);
        }
        f
    }

    async fn new(new: NewMapping, options: MatchOptions, episodes: Vec<WantedEpisode>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&db_path(&dir)).await.unwrap();
        let id = db.insert_mapping(&new).await.unwrap().id;
        let mapping = db.set_match_options(id, &options).await.unwrap().unwrap();
        Self {
            db,
            dir,
            mapping,
            candidates: FakeCandidates::default(),
            meta: FakeMetadata::default(),
            episodes,
        }
    }

    async fn run(&self) -> Report {
        let wanted = FakeWanted(vec![WantedSeries {
            mapping: self.mapping.clone(),
            episodes: self.episodes.clone(),
        }]);
        match_shows(&self.db, &self.candidates, &wanted, &self.meta)
            .await
            .unwrap()
    }

    /// Target (`YYYY-MM-DD` or `SxxEyy`) to video ids.
    async fn releases(&self) -> BTreeMap<String, Vec<String>> {
        let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for r in self
            .db
            .releases_for_mappings(&[self.mapping.id], &EpisodeFilter::Any, 1000)
            .await
            .unwrap()
        {
            let key = match r.target {
                PinTarget::AirDate(date) => date,
                PinTarget::Episode { season, episode } => format!("S{season:02}E{episode:02}"),
                PinTarget::Film => panic!("film release on a series: {}", r.video_id),
            };
            out.entry(key).or_default().push(r.video_id);
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

fn db_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("tubarr.db")
}

fn releases(pairs: &[(&str, &str)]) -> BTreeMap<String, Vec<String>> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), vec![(*v).to_owned()]))
        .collect()
}

#[tokio::test]
async fn daily_show_matches_by_title_date_or_broadcast_date() {
    let f = Fixture::daily(|_| {}).await;
    let report = f.run().await;
    assert_eq!(
        f.releases().await,
        releases(&[
            ("2026-10-01", "d01"),
            ("2026-10-02", "d02late"),
            ("2026-10-03", "d03"),
            ("2026-10-04", "d04"),
            ("2026-10-06", "d06"),
            ("2026-10-07", "d07"),
            ("2026-10-11", "d11"),
            ("2026-10-15", "d15late"),
            ("2026-10-19", "d19guess"),
        ])
    );
    assert_eq!(report.released, 9);
    assert_eq!(report.not_wanted, 1, "the 30th is not wanted");
    assert!(report.skipped.is_empty());
    let d01 = f.db.release_by_video("d01").await.unwrap().unwrap();
    assert_eq!(d01.target, PinTarget::AirDate("2026-10-01".to_owned()));
    assert_eq!((d01.height, d01.language.as_str()), (1080, "Urdu"));

    let notes = f.notes().await;
    let state = |id: &str| notes.get(id).map(|n| n.state);
    assert_eq!(state("d04sub"), Some(UnmatchedState::Duplicate));
    assert!(notes["d04sub"].reason.contains("kept d04"));
    assert_eq!(state("d05"), Some(UnmatchedState::Unsure));
    assert!(
        notes["d05"].reason.contains("2026-10-05") && notes["d05"].reason.contains("2026-10-09"),
        "{}",
        notes["d05"].reason
    );
    assert_eq!(state("d08clip"), Some(UnmatchedState::Unsure));
    assert!(notes["d08clip"].reason.contains("12 min"));
    // The earlier edition keeps the 10th (its height is not known yet), the later one waits for
    // a regex or a pin.
    assert_eq!(state("d10am"), Some(UnmatchedState::Pending));
    assert_eq!(state("d10pm"), Some(UnmatchedState::Unsure));
    assert!(notes["d10pm"].reason.contains("edition"));
    assert_eq!(state("d12"), Some(UnmatchedState::Pending));
    assert!(notes["d12"].reason.contains("duration"));
    assert_eq!(state("d13"), Some(UnmatchedState::Pending));
    assert!(notes["d13"].reason.contains("height"));
    assert_eq!(state("d14"), Some(UnmatchedState::Unsure));
    assert!(notes["d14"].reason.contains("several dates"));
    assert_eq!(state("d17early"), Some(UnmatchedState::Unsure));
    assert!(notes["d17early"].reason.contains("2026-10-15"));
    assert_eq!(state("d18guess"), Some(UnmatchedState::Unsure));
    assert!(notes["d18guess"].reason.contains("no exact upload time"));
    for quiet in ["d01", "d08promo", "d30", "other"] {
        assert_eq!(state(quiet), None, "{quiet}");
    }
}

#[tokio::test]
async fn a_lookups_exact_time_dates_a_guessed_daily_upload() {
    use tubarr::resolver::{Resolved, StreamFormat, VideoMeta};
    use tubarr::store::{SeenVideo, VideoSource};

    let f = Fixture::new(
        NewMapping {
            kind: MappingKind::Daily,
            tvdb_id: Some(400_100),
            tmdb_id: None,
            title: "Har Lamha Purjosh".to_owned(),
            language: "Urdu".to_owned(),
            channel_ids: vec![ARY_NEWS.to_owned()],
            episode_regex: None,
            excludes: vec![],
            min_duration_secs: None,
        },
        MatchOptions::default(),
        wanted_dates(),
    )
    .await;
    f.meta.height("d18", 1080);
    // A channel list read "2 weeks ago": a day at 00:00, a guess.
    f.db.upsert_seen_video(&SeenVideo {
        video_id: "d18".to_owned(),
        channel_id: ARY_NEWS.to_owned(),
        title: hlp("Waseem Badami | ARY News"),
        published_at: Some(plus5(17, 5, 0)),
        published_approx: true,
        duration_secs: Some(44 * MIN),
        source: VideoSource::Listing,
    })
    .await
    .unwrap();
    let wanted = FakeWanted(vec![WantedSeries {
        mapping: f.mapping.clone(),
        episodes: f.episodes.clone(),
    }]);
    let run = || match_shows(&f.db, &f.db, &wanted, &f.meta);
    run().await.unwrap();
    assert!(f.releases().await.is_empty());
    assert!(
        f.notes().await["d18"]
            .reason
            .contains("no exact upload time")
    );

    // The video's own lookup gives its exact time, on the evening of the 18th.
    let stream = |id: &str| StreamFormat {
        format_id: id.to_owned(),
        url: String::new(),
        ext: None,
        width: None,
        height: None,
        filesize: None,
        filesize_approx: None,
        http_headers: Default::default(),
    };
    let lookup = Resolved {
        meta: VideoMeta {
            id: "d18".to_owned(),
            title: hlp("Waseem Badami | ARY News"),
            duration: Some(44 * 60),
            upload_date: Some("20261018".to_owned()),
            timestamp: Some(plus5(18, 21, 30)),
            channel_id: Some(ARY_NEWS.to_owned()),
        },
        video: stream("137"),
        audio: stream("140"),
        subtitles: vec![],
        expires_at: None,
    };
    f.db.store_video(&lookup, 0).await.unwrap();
    run().await.unwrap();
    assert_eq!(f.releases().await, releases(&[("2026-10-18", "d18")]));
    let release = f.db.release_by_video("d18").await.unwrap().unwrap();
    assert_eq!(release.published_at, plus5(18, 21, 30));

    // A later channel list guesses again: the exact time and the release stay.
    f.db.upsert_seen_video(&SeenVideo {
        video_id: "d18".to_owned(),
        channel_id: ARY_NEWS.to_owned(),
        title: hlp("Waseem Badami | ARY News"),
        published_at: Some(plus5(16, 5, 0)),
        published_approx: true,
        duration_secs: Some(44 * MIN),
        source: VideoSource::Listing,
    })
    .await
    .unwrap();
    run().await.unwrap();
    assert_eq!(f.releases().await, releases(&[("2026-10-18", "d18")]));
}

#[tokio::test]
async fn daily_run_twice_changes_nothing() {
    let f = Fixture::daily(|_| {}).await;
    f.run().await;
    let (releases, notes) = (f.releases().await, f.notes().await);
    assert_eq!(
        f.run().await,
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
async fn daily_pins_win_and_survive_a_restart() {
    let f = Fixture::daily(|_| {}).await;
    f.run().await;
    let pin = |date: &str| PinTarget::AirDate(date.to_owned());
    // Hani prefers the Eng Sub upload on the 4th and settles the unsure 5th.
    f.db.set_pin(f.mapping.id, &pin("2026-10-04"), "d04sub")
        .await
        .unwrap();
    f.db.set_pin(f.mapping.id, &pin("2026-10-05"), "d05")
        .await
        .unwrap();
    f.meta.height("d05", 720);
    let report = f.run().await;
    assert_eq!((report.replaced, report.released), (1, 2));
    let got = f.releases().await;
    assert_eq!(got["2026-10-04"], ["d04sub"]);
    assert_eq!(got["2026-10-05"], ["d05"]);
    let notes = f.notes().await;
    assert_eq!(notes["d04"].state, UnmatchedState::Duplicate);
    assert!(notes["d04"].reason.contains("pinned"));
    assert!(!notes.contains_key("d05") && !notes.contains_key("d04sub"));

    // A restart: the database is opened again, the pin still decides.
    let Fixture {
        db,
        dir,
        mapping,
        candidates,
        meta,
        episodes,
    } = f;
    db.pool().close().await;
    let db = Db::open(&db_path(&dir)).await.unwrap();
    let f = Fixture {
        db,
        dir,
        mapping,
        candidates,
        meta,
        episodes,
    };
    assert_eq!(f.run().await.released, 0);
    assert_eq!(f.releases().await["2026-10-04"], ["d04sub"]);
}

#[tokio::test]
async fn a_regex_keeps_one_edition() {
    let f = Fixture::daily(|m| m.episode_regex = Some(r"(?i)night edition".to_owned())).await;
    f.meta.height("d10pm", 1080);
    f.run().await;
    assert_eq!(f.releases().await, releases(&[("2026-10-10", "d10pm")]));
    assert!(!f.notes().await.contains_key("d10am"));
}

#[tokio::test]
async fn an_alias_names_a_daily_show_and_is_no_second_edition() {
    // TVDB spells it differently from the channel: only the alias names the uploads.
    let f = Fixture::daily(|m| m.title = "Harr Lamha Pur Josh".to_owned()).await;
    f.run().await;
    assert!(f.releases().await.is_empty());

    f.db.set_aliases(f.mapping.id, &["Har Lamha Purjosh".to_owned()])
        .await
        .unwrap();
    f.candidates.videos.lock().unwrap().push(cand(
        "d20tvdb",
        ARY_NEWS,
        "Harr Lamha Pur Josh | 20th October 2026 | ARY News",
        plus5(20, 22, 0),
        Some(45),
    ));
    f.candidates.videos.lock().unwrap().push(cand(
        "d20alias",
        ARY_NEWS,
        &hlp("20th October 2026 | ARY News"),
        plus5(20, 23, 0),
        Some(45),
    ));
    f.meta.height("d20tvdb", 1080);
    f.run().await;
    let got = f.releases().await;
    assert_eq!(got.get("2026-10-01"), Some(&vec!["d01".to_owned()]));
    assert_eq!(got.get("2026-10-20"), Some(&vec!["d20tvdb".to_owned()]));
    let notes = f.notes().await;
    assert_eq!(notes["d20alias"].state, UnmatchedState::Duplicate);
    let d01 = f.db.release_by_video("d01").await.unwrap().unwrap();
    assert_eq!(d01.arr_title, "Harr Lamha Pur Josh");
}

#[tokio::test]
async fn the_channel_offset_moves_the_day_boundary() {
    // 00:45 UTC on the 15th: 05:45 at +05:00 (still the 14th), 06:15 in India (the 15th).
    let upload = cand("u", ARY_NEWS, &hlp("Show"), plus5(15, 5, 45), Some(45));
    // A time zone counts like its offset, and wins over an offset set with it.
    for (offset, timezone, want) in [
        (None, None, "2026-10-14"),
        (Some(330), None, "2026-10-15"),
        (None, Some("Asia/Kolkata"), "2026-10-15"),
        (Some(330), Some("Etc/GMT-5"), "2026-10-14"),
    ] {
        let f = Fixture::daily(|_| {}).await;
        f.db.set_match_options(
            f.mapping.id,
            &MatchOptions {
                utc_offset_mins: offset,
                timezone: timezone.map(str::to_owned),
                ..MatchOptions::default()
            },
        )
        .await
        .unwrap();
        let f = Fixture {
            mapping: f.db.mapping(f.mapping.id).await.unwrap().unwrap(),
            ..f
        };
        *f.candidates.videos.lock().unwrap() = vec![upload.clone()];
        f.meta.height("u", 1080);
        f.run().await;
        assert_eq!(
            f.releases().await,
            releases(&[(want, "u")]),
            "{offset:?} {timezone:?}"
        );
    }
}

#[tokio::test]
async fn an_unmonitored_date_keeps_its_upload_off_the_next_day() {
    // Dated by its upload time only, on the 16th. The 17th is monitored and has no upload yet.
    async fn fixture() -> Fixture {
        let upload = cand(
            "u16",
            ARY_NEWS,
            &hlp("ARY News"),
            plus5(16, 21, 0),
            Some(45),
        );
        let mut f = Fixture::daily(|_| {}).await;
        *f.candidates.videos.lock().unwrap() = vec![upload];
        f.meta.height("u16", 1080);
        f.episodes.retain(|e| e.episode >= 16 && e.episode <= 17);
        f
    }

    // With the 16th unmonitored the upload is that episode: it stays off the 17th.
    let mut f = fixture().await;
    f.episodes[0].monitored = false;
    let report = f.run().await;
    assert!(f.releases().await.is_empty());
    assert_eq!((report.released, report.not_wanted), (0, 0));
    let notes = f.notes().await;
    assert_eq!(notes["u16"].state, UnmatchedState::Unsure);
    assert!(
        notes["u16"]
            .reason
            .contains("2026-10-16 is unmonitored in Sonarr"),
        "{}",
        notes["u16"].reason
    );

    // Monitored, it is released on its own date.
    let f = fixture().await;
    f.run().await;
    assert_eq!(f.releases().await, releases(&[("2026-10-16", "u16")]));
    assert!(f.notes().await.is_empty());

    // Not listed at all, it still moves a day to the 17th as before.
    let mut f = fixture().await;
    f.episodes.remove(0);
    f.run().await;
    assert_eq!(f.releases().await, releases(&[("2026-10-17", "u16")]));
}

#[tokio::test]
async fn a_date_sonarr_stops_listing_loses_its_release() {
    let upload = cand(
        "u16",
        ARY_NEWS,
        &hlp("ARY News"),
        plus5(16, 21, 0),
        Some(45),
    );
    let mut f = Fixture::daily(|_| {}).await;
    *f.candidates.videos.lock().unwrap() = vec![upload];
    f.meta.height("u16", 1080);
    f.episodes.retain(|e| e.episode >= 16 && e.episode <= 17);
    f.run().await;
    assert_eq!(f.releases().await, releases(&[("2026-10-16", "u16")]));

    // TVDB drops the 16th: its release goes, and the upload moves to the 17th, one release only.
    f.episodes.remove(0);
    let report = f.run().await;
    assert_eq!(f.releases().await, releases(&[("2026-10-17", "u16")]));
    assert_eq!((report.released, report.replaced), (1, 1));

    // And the 17th too: nothing is left.
    f.episodes.clear();
    f.episodes.push(WantedEpisode {
        season: 2026,
        episode: 20,
        air_date: Some("2026-10-20".to_owned()),
        title: None,
        monitored: true,
    });
    let report = f.run().await;
    assert!(f.releases().await.is_empty());
    assert_eq!((report.released, report.replaced), (0, 1));

    // An unmonitored date still listed keeps the release it has.
    let mut f = Fixture::daily(|_| {}).await;
    *f.candidates.videos.lock().unwrap() = vec![cand(
        "u16",
        ARY_NEWS,
        &hlp("ARY News"),
        plus5(16, 21, 0),
        Some(45),
    )];
    f.meta.height("u16", 1080);
    f.episodes.retain(|e| e.episode == 16);
    f.run().await;
    f.episodes[0].monitored = false;
    f.run().await;
    assert_eq!(f.releases().await, releases(&[("2026-10-16", "u16")]));
}

#[tokio::test]
async fn an_upload_moved_to_another_date_keeps_one_release() {
    // Up on the 16th, which TVDB does not list: it takes the 15th, the day before.
    let u16 = cand(
        "u16",
        ARY_NEWS,
        &hlp("ARY News"),
        plus5(16, 21, 0),
        Some(45),
    );
    let mut f = Fixture::daily(|_| {}).await;
    *f.candidates.videos.lock().unwrap() = vec![u16.clone()];
    f.meta.height("u16", 1080);
    f.episodes.retain(|e| e.episode == 15 || e.episode == 17);
    f.run().await;
    assert_eq!(f.releases().await, releases(&[("2026-10-15", "u16")]));

    // The 15th gets its own upload, not looked up yet: the 15th waits for it, and u16 moves to
    // the 17th without keeping its release on the 15th.
    let t15 = cand(
        "t15",
        ARY_NEWS,
        &hlp("15th October 2026 | ARY News"),
        plus5(15, 23, 0),
        Some(45),
    );
    *f.candidates.videos.lock().unwrap() = vec![u16, t15];
    let report = f.run().await;
    assert_eq!(f.releases().await, releases(&[("2026-10-17", "u16")]));
    assert_eq!((report.released, report.replaced), (1, 1));
    assert_eq!(f.notes().await["t15"].state, UnmatchedState::Pending);

    // Looked up, it takes the 15th.
    f.meta.height("t15", 1080);
    f.run().await;
    assert_eq!(
        f.releases().await,
        releases(&[("2026-10-15", "t15"), ("2026-10-17", "u16")])
    );
}

// Playlist numbered shows.

const PLAYLIST: &str = "PLpitchersSeason1";

fn pitchers(rest: &str) -> String {
    format!("Pitchers | {rest}")
}

async fn playlist_fixture(edit: impl FnOnce(&mut NewMapping)) -> Fixture {
    let mut new = NewMapping {
        kind: MappingKind::Show,
        tvdb_id: Some(300_200),
        tmdb_id: None,
        title: "Pitchers".to_owned(),
        language: "Hindi".to_owned(),
        channel_ids: vec![TVF.to_owned()],
        episode_regex: None,
        excludes: vec![],
        min_duration_secs: None,
    };
    edit(&mut new);
    let options = MatchOptions {
        playlist_id: Some(PLAYLIST.to_owned()),
        playlist_mode: PlaylistMode::Order,
        title_number: false,
        ..MatchOptions::default()
    };
    let episodes = (1..=6)
        .map(|episode| WantedEpisode {
            season: 1,
            episode,
            air_date: None,
            title: None,
            monitored: true,
        })
        .collect();
    let f = Fixture::new(new, options, episodes).await;
    let at = |n: i64| OCT1_PLUS5 + n * 7 * DAY;
    *f.candidates.videos.lock().unwrap() = vec![
        cand(
            "trailer",
            TVF,
            &pitchers("Official Trailer"),
            at(0),
            Some(2),
        ),
        cand("p1", TVF, &pitchers("Ab Hum Karenge"), at(1), Some(30)),
        cand("p2", TVF, &pitchers("Jhol"), at(2), Some(32)),
        cand("p3", TVF, &pitchers("Dhanda"), at(3), Some(33)),
        cand("clip", TVF, &pitchers("Behind The Scenes"), at(3), Some(4)),
        cand("p4", TVF, &pitchers("Pitch"), at(4), Some(31)),
    ];
    // The finale is on another channel, known only from an earlier lookup.
    f.meta.set(
        "p5",
        Known {
            title: Some(pitchers("Finale")),
            duration_secs: Some(29 * MIN),
            published_at: Some(at(5)),
            height: Some(1080),
            size: None,
        },
    );
    for id in ["p1", "p2", "p3", "p4"] {
        f.meta.height(id, 1080);
    }
    f.candidates.playlists.lock().unwrap().insert(
        PLAYLIST.to_owned(),
        ["trailer", "p1", "p2", "p3", "clip", "p4", "p5"]
            .map(str::to_owned)
            .to_vec(),
    );
    f
}

#[tokio::test]
async fn playlist_order_numbers_episodes_until_an_uncaught_extra() {
    let f = playlist_fixture(|_| {}).await;
    let report = f.run().await;
    assert_eq!(
        f.releases().await,
        releases(&[("S01E01", "p1"), ("S01E02", "p2"), ("S01E03", "p3")])
    );
    assert_eq!(report.released, 3);
    let notes = f.notes().await;
    assert_eq!(notes["clip"].state, UnmatchedState::Unsure);
    assert!(
        notes["clip"].reason.contains("4 min"),
        "{}",
        notes["clip"].reason
    );
    assert_eq!(notes["p4"].state, UnmatchedState::Unsure);
    assert_eq!(
        (notes["p4"].season, notes["p4"].episode),
        (Some(1), Some(4))
    );
    assert_eq!(notes["p5"].episode, Some(5));
    assert!(!notes.contains_key("trailer"));

    // Same input, same answer: the mapping is matched again and writes nothing.
    let again = f.run().await;
    assert_eq!(
        again,
        Report {
            matched: 1,
            ..Report::default()
        }
    );
}

#[tokio::test]
async fn a_playlist_episode_tvdb_drops_or_renumbers_loses_its_release() {
    let mut f = playlist_fixture(|_| {}).await;
    f.run().await;

    // TVDB drops E02: its release goes, the others stay.
    f.episodes.retain(|e| e.episode != 2);
    let report = f.run().await;
    assert_eq!(
        f.releases().await,
        releases(&[("S01E01", "p1"), ("S01E03", "p3")])
    );
    assert_eq!((report.released, report.replaced), (0, 1));

    // TVDB lists it again and the channel takes the pilot out of the playlist: every video moves
    // up one, and p3 keeps only E02, not its old E03 too.
    let mut f = playlist_fixture(|_| {}).await;
    f.run().await;
    f.candidates
        .playlists
        .lock()
        .unwrap()
        .get_mut(PLAYLIST)
        .unwrap()
        .retain(|v| v != "p1");
    let report = f.run().await;
    assert_eq!(
        f.releases().await,
        releases(&[("S01E01", "p2"), ("S01E02", "p3")])
    );
    assert_eq!((report.released, report.replaced), (2, 3));

    // An empty Sonarr list says nothing yet: nothing is retired.
    f.episodes.clear();
    f.run().await;
    assert_eq!(
        f.releases().await,
        releases(&[("S01E01", "p2"), ("S01E02", "p3")])
    );
}

#[tokio::test]
async fn an_exclude_for_the_extra_frees_the_rest() {
    let f = playlist_fixture(|m| m.excludes = vec!["Behind The Scenes".to_owned()]).await;
    f.run().await;
    assert_eq!(
        f.releases().await,
        releases(&[
            ("S01E01", "p1"),
            ("S01E02", "p2"),
            ("S01E03", "p3"),
            ("S01E04", "p4"),
            ("S01E05", "p5"),
        ])
    );
    assert!(f.notes().await.is_empty());
}

#[tokio::test]
async fn playlist_pins_settle_blocked_numbers() {
    let f = playlist_fixture(|_| {}).await;
    let target = PinTarget::Episode {
        season: 1,
        episode: 4,
    };
    f.db.set_pin(f.mapping.id, &target, "p4").await.unwrap();
    f.run().await;
    let got = f.releases().await;
    assert_eq!(got["S01E04"], ["p4"]);
    assert!(!got.contains_key("S01E05"));
    let notes = f.notes().await;
    assert!(!notes.contains_key("p4"));
    assert_eq!(notes["p5"].state, UnmatchedState::Unsure);
}

#[tokio::test]
async fn playlist_offset_and_unknown_durations() {
    let f = playlist_fixture(|_| {}).await;
    f.db.set_match_options(
        f.mapping.id,
        &MatchOptions {
            playlist_id: Some(PLAYLIST.to_owned()),
            playlist_offset: 2,
            playlist_mode: PlaylistMode::Order,
            title_number: false,
            ..MatchOptions::default()
        },
    )
    .await
    .unwrap();
    let f = Fixture {
        mapping: f.db.mapping(f.mapping.id).await.unwrap().unwrap(),
        ..f
    };
    // p2's duration is not known yet: what comes after it waits.
    f.candidates
        .videos
        .lock()
        .unwrap()
        .iter_mut()
        .filter(|c| c.video_id == "p2")
        .for_each(|c| c.duration_secs = None);
    f.run().await;
    assert_eq!(f.releases().await, releases(&[("S01E02", "p1")]));
    let notes = f.notes().await;
    assert_eq!(notes["p2"].state, UnmatchedState::Pending);
    assert_eq!(notes["p3"].state, UnmatchedState::Pending);
    assert!(
        notes["p3"].reason.contains("duration of video 3"),
        "{}",
        notes["p3"].reason
    );
}

#[tokio::test]
async fn a_playlist_not_listed_yet_skips_the_mapping() {
    let f = playlist_fixture(|_| {}).await;
    f.candidates.playlists.lock().unwrap().clear();
    let report = f.run().await;
    assert_eq!(report.skipped.len(), 1);
    assert!(report.skipped[0].1.contains("not listed"));
    assert!(f.releases().await.is_empty());
}

// A playlist as a source of videos: titles give the numbers.

const DRAMA_PLAYLIST: &str = "PLdarENijaatOfficial0000000001";
const ARY: &str = "UC4JCksJF76g_MdzPVBJoC3Q";

/// An official playlist with a promo, the episodes out of order, one episode in two parts and a
/// gap where episode 4 should be.
async fn source_fixture(mode: PlaylistMode) -> Fixture {
    let new = NewMapping {
        kind: MappingKind::Show,
        tvdb_id: Some(460_100),
        tmdb_id: None,
        title: "Dar E Nijaat".to_owned(),
        language: "Urdu".to_owned(),
        channel_ids: vec![ARY.to_owned()],
        episode_regex: None,
        excludes: vec!["Promo".to_owned()],
        min_duration_secs: None,
    };
    let options = MatchOptions {
        playlist_id: Some(DRAMA_PLAYLIST.to_owned()),
        playlist_mode: mode,
        title_number: false,
        ..MatchOptions::default()
    };
    let episodes = (1..=5)
        .map(|episode| WantedEpisode {
            season: 1,
            episode,
            air_date: None,
            title: None,
            monitored: true,
        })
        .collect();
    let f = Fixture::new(new, options, episodes).await;
    let at = |n: i64| OCT1_PLUS5 + n * DAY;
    let title = |rest: &str| format!("Dar E Nijaat {rest} | Yumna Zaidi | ARY Digital");
    *f.candidates.videos.lock().unwrap() = vec![
        cand("promo", ARY, &title("Promo"), at(0), Some(1)),
        cand("e2", ARY, &title("Episode 2"), at(2), Some(38)),
        cand("e1", ARY, &title("Episode 1"), at(1), Some(39)),
        cand("e3a", ARY, &title("Episode 3 - [Part 1]"), at(3), Some(20)),
        cand("e3b", ARY, &title("Episode 3 - [Part 2]"), at(3), Some(19)),
        cand("e5", ARY, &title("Episode 5"), at(5), Some(37)),
    ];
    for id in ["e1", "e2", "e3a", "e3b", "e5"] {
        f.meta.height(id, 1080);
    }
    f.candidates.playlists.lock().unwrap().insert(
        DRAMA_PLAYLIST.to_owned(),
        ["promo", "e2", "e1", "e3a", "e3b", "e5"]
            .map(str::to_owned)
            .to_vec(),
    );
    f
}

#[tokio::test]
async fn playlist_source_mode_numbers_by_title() {
    let f = source_fixture(PlaylistMode::Source).await;
    let report = f.run().await;
    assert!(report.skipped.is_empty(), "{report:?}");
    let got = f.releases().await;
    assert_eq!(got["S01E01"], ["e1"]);
    assert_eq!(got["S01E02"], ["e2"]);
    // The two parts make one episode, and the gap leaves episode 4 open instead of shifting 5.
    assert_eq!(got["S01E03"], ["e3a"]);
    let joined = f.db.release_by_video("e3a,e3b").await.unwrap().unwrap();
    assert_eq!(joined.part_ids, ["e3a", "e3b"]);
    assert_eq!(got["S01E05"], ["e5"]);
    assert!(!got.contains_key("S01E04"));
    assert!(!f.notes().await.contains_key("promo"));

    // Not waiting on a listing either: titles alone are enough.
    f.candidates.playlists.lock().unwrap().clear();
    assert_eq!(
        f.run().await,
        Report {
            matched: 1,
            ..Report::default()
        }
    );
}

#[tokio::test]
async fn playlist_order_mode_still_numbers_by_place() {
    let f = source_fixture(PlaylistMode::Order).await;
    f.run().await;
    let got = f.releases().await;
    // By place the out of order upload takes the wrong number: the reason source is the default.
    assert_eq!(got["S01E01"], ["e2"]);
    assert_eq!(got["S01E02"], ["e1"]);
}

const JRE: &str = "UCjreOfficial0000000000";

/// Unix seconds of a Chicago wall clock time (-05:00) in October 2026.
fn cdt(day: i64, hour: i64) -> i64 {
    plus5(day, hour, 0) + 10 * HOUR
}

fn jre(rest: &str) -> String {
    format!("Joe Rogan Experience {rest}")
}

/// TVDB's 2026 season of The Joe Rogan Experience: the podcast number lives in the title, and
/// the newest episode has none yet.
fn jre_episodes() -> Vec<WantedEpisode> {
    [
        (134, Some("#2567 - Guest A"), "2026-10-17"),
        (135, Some("#2568 - Guest B"), "2026-10-20"),
        (136, Some("#2569 - Bill Thompson"), "2026-10-21"),
        (137, Some("#2566 - Guest X"), "2026-10-14"),
        (138, Some("#2566 - Guest X (Encore)"), "2026-10-15"),
        (139, Some("TBA"), "2026-10-23"),
    ]
    .into_iter()
    .map(|(episode, title, date)| WantedEpisode {
        season: 2026,
        episode,
        air_date: Some(date.to_owned()),
        title: title.map(str::to_owned),
        monitored: true,
    })
    .collect()
}

async fn podcast_fixture(title_number: bool) -> Fixture {
    let new = NewMapping {
        kind: MappingKind::Show,
        tvdb_id: Some(326_959),
        tmdb_id: None,
        title: "The Joe Rogan Experience".to_owned(),
        language: "English".to_owned(),
        channel_ids: vec![JRE.to_owned()],
        episode_regex: None,
        excludes: vec![],
        min_duration_secs: None,
    };
    let options = MatchOptions {
        utc_offset_mins: Some(-300),
        title_number,
        ..MatchOptions::default()
    };
    let f = Fixture::new(new, options, jre_episodes()).await;
    f.db.set_aliases(f.mapping.id, &["Joe Rogan Experience".to_owned()])
        .await
        .unwrap();
    *f.candidates.videos.lock().unwrap() = vec![
        cand(
            "j2569",
            JRE,
            &jre("#2569 - Bill Thompson"),
            cdt(21, 13),
            Some(170),
        ),
        // Clips with the same number: one over the floor but under half the episode, and one
        // under the floor. Both went up first.
        cand(
            "j2569best",
            JRE,
            &jre("#2569 - Bill Thompson best moments"),
            cdt(21, 11),
            Some(40),
        ),
        cand(
            "j2569clip",
            JRE,
            &jre("#2569 clip - Bill on aliens"),
            cdt(21, 12),
            Some(12),
        ),
        // TVDB has not numbered #2570 yet: matched by its upload date to the TBA episode.
        cand(
            "j2570",
            JRE,
            &jre("#2570 - New Guest"),
            cdt(23, 14),
            Some(150),
        ),
        // An old episode Sonarr does not want.
        cand(
            "j2400",
            JRE,
            &jre("#2400 - Old Guest"),
            cdt(2, 14),
            Some(150),
        ),
        // TVDB has #2566 twice.
        cand(
            "j2566",
            JRE,
            &jre("#2566 - Guest X"),
            cdt(14, 14),
            Some(150),
        ),
        // Two numbers in one title.
        cand(
            "jboth",
            JRE,
            &jre("#2567 and #2568 back to back"),
            cdt(20, 14),
            Some(300),
        ),
        // Another show on the channel.
        cand(
            "mma",
            JRE,
            "JRE MMA Show #160 with Guest",
            cdt(21, 15),
            Some(120),
        ),
    ];
    for id in ["j2569", "j2569best", "j2570"] {
        f.meta.height(id, 1080);
    }
    f
}

#[tokio::test]
async fn a_podcast_matches_by_the_number_in_tvdb_titles() {
    let mut f = podcast_fixture(true).await;
    let report = f.run().await;
    assert!(report.skipped.is_empty(), "{report:?}");
    assert_eq!(
        f.releases().await,
        releases(&[("S2026E136", "j2569"), ("S2026E139", "j2570")])
    );
    assert_eq!(report.not_wanted, 1, "#2400 is not wanted");

    let notes = f.notes().await;
    let best = &notes["j2569best"];
    assert_eq!(best.state, UnmatchedState::Duplicate);
    assert!(
        best.reason
            .contains("#2569: a 40 min clip next to the 170 min episode j2569"),
        "{}",
        best.reason
    );
    assert_eq!((best.season, best.episode), (Some(2026), Some(136)));
    // A clip under the floor next to a full upload is not worth a note.
    assert!(!notes.contains_key("j2569clip"));
    assert_eq!(notes["j2566"].state, UnmatchedState::Unsure);
    assert!(
        notes["j2566"]
            .reason
            .contains("TVDB titles name #2566 on S2026E137 and S2026E138"),
        "{}",
        notes["j2566"].reason
    );
    assert_eq!(notes["jboth"].state, UnmatchedState::Unsure);
    assert!(notes["jboth"].reason.contains("#2567 and #2568"));
    assert!(!notes.contains_key("mma"));

    // Once TVDB names the episode, the number finds the same one and nothing changes.
    f.episodes[5].title = Some("#2570 - New Guest".to_owned());
    let again = f.run().await;
    assert_eq!((again.released, again.replaced), (0, 0));
    assert_eq!(
        f.releases().await,
        releases(&[("S2026E136", "j2569"), ("S2026E139", "j2570")])
    );
    // A later upload numbered above every TVDB title lands on no numbered episode's date.
    f.candidates.videos.lock().unwrap().push(cand(
        "j2571",
        JRE,
        &jre("#2571 - Next Guest"),
        cdt(22, 14),
        Some(150),
    ));
    let late = f.run().await;
    assert_eq!(late.released, 0);
    assert_eq!(late.not_wanted, 1, "#2400 only, #2571 awaits metadata");
    assert_missing_podcast_target(&f.notes().await, "j2571", 2571);
}

fn assert_missing_podcast_target(notes: &HashMap<String, Unmatched>, id: &str, number: u64) {
    let note = notes
        .get(id)
        .unwrap_or_else(|| panic!("missing note for {id}"));
    assert_eq!(note.state, UnmatchedState::Unsure);
    assert_eq!((note.season, note.episode, note.part), (None, None, None));
    assert_eq!(
        note.reason,
        format!(
            "no TVDB title names #{number} yet and no free listed air date fits this upload, \
             wait for TVDB/Sonarr metadata or pin it to its episode"
        )
    );
}

async fn missing_podcast_target_fixture(day: i64) -> Fixture {
    let f = podcast_fixture(true).await;
    *f.candidates.videos.lock().unwrap() = vec![cand(
        "j2571",
        JRE,
        &jre("#2571 - Next Guest"),
        cdt(day, 14),
        Some(150),
    )];
    f.meta.height("j2571", 1080);
    f
}

#[tokio::test]
async fn a_podcast_missing_target_note_clears_when_metadata_arrives() {
    // No unnumbered date is near this upload: Dating::Unlisted.
    let mut f = missing_podcast_target_fixture(28).await;
    let first = f.run().await;
    assert!(f.releases().await.is_empty());
    assert_eq!(
        (first.released, first.not_wanted, first.notes_changed),
        (0, 0, 1)
    );
    let notes = f.notes().await;
    assert_missing_podcast_target(&notes, "j2571", 2571);
    let note = notes["j2571"].clone();
    assert!(f.db.set_unmatched_ignored(note.id, true).await.unwrap());

    let again = f.run().await;
    assert_eq!(
        (again.released, again.not_wanted, again.notes_changed),
        (0, 0, 0)
    );
    assert_eq!(f.notes().await["j2571"], note);
    let ignored: bool = sqlx::query_scalar("SELECT ignored FROM unmatched WHERE id = ?")
        .bind(note.id)
        .fetch_one(f.db.pool())
        .await
        .unwrap();
    assert!(
        ignored,
        "normal note reconciliation preserves the ignored flag"
    );

    // The same upload becomes releasable by number, without needing a date fallback.
    f.episodes.push(WantedEpisode {
        season: 2026,
        episode: 140,
        air_date: Some("2026-10-28".to_owned()),
        title: Some("#2571 - Next Guest".to_owned()),
        monitored: true,
    });
    let resolved = f.run().await;
    assert_eq!(f.releases().await, releases(&[("S2026E140", "j2571")]));
    assert_eq!(
        (
            resolved.released,
            resolved.not_wanted,
            resolved.notes_changed
        ),
        (1, 0, 0)
    );
    assert!(f.notes().await.is_empty(), "stale notes use normal cleanup");
    assert_eq!(f.run().await.notes_changed, 0);
}

#[tokio::test]
async fn a_podcast_missing_target_with_no_known_maximum_stays_unsure() {
    // Both an empty catalogue and a distant TBA catalogue have no known running number.
    for empty in [false, true] {
        let mut f = missing_podcast_target_fixture(28).await;
        f.episodes.retain(|e| !empty && e.episode == 139);
        let report = f.run().await;
        assert!(f.releases().await.is_empty());
        assert_eq!(
            (report.released, report.not_wanted, report.notes_changed),
            (0, 0, 1)
        );
        assert_missing_podcast_target(&f.notes().await, "j2571", 2571);
    }
}

#[tokio::test]
async fn a_podcast_missing_target_on_an_occupied_neighbour_stays_unsure() {
    // Dating succeeds, but the 23rd has its own full upload and cannot take the 24th's.
    let f = missing_podcast_target_fixture(24).await;
    f.candidates.videos.lock().unwrap().push(cand(
        "j2570",
        JRE,
        &jre("#2570 - New Guest"),
        cdt(23, 14),
        Some(150),
    ));
    f.meta.height("j2570", 1080);
    let report = f.run().await;
    assert_eq!(f.releases().await, releases(&[("S2026E139", "j2570")]));
    assert_eq!(
        (report.released, report.not_wanted, report.notes_changed),
        (1, 0, 1)
    );
    assert_missing_podcast_target(&f.notes().await, "j2571", 2571);
}

#[tokio::test]
async fn a_podcast_missing_target_on_a_pinned_neighbour_stays_unsure() {
    let f = missing_podcast_target_fixture(24).await;
    f.candidates.videos.lock().unwrap().push(cand(
        "pinned",
        JRE,
        &jre("#2570 - Pinned Guest"),
        cdt(23, 14),
        Some(150),
    ));
    f.meta.height("pinned", 1080);
    f.db.set_pin(
        f.mapping.id,
        &PinTarget::Episode {
            season: 2026,
            episode: 139,
        },
        "pinned",
    )
    .await
    .unwrap();
    let report = f.run().await;
    assert_eq!(f.releases().await, releases(&[("S2026E139", "pinned")]));
    assert_eq!(
        (report.released, report.not_wanted, report.notes_changed),
        (1, 0, 1)
    );
    let notes = f.notes().await;
    assert_missing_podcast_target(&notes, "j2571", 2571);
    assert!(!notes.contains_key("pinned"));
}

#[tokio::test]
async fn a_podcast_missing_target_with_an_explicit_neighbour_date_stays_unsure() {
    let f = missing_podcast_target_fixture(24).await;
    f.candidates.videos.lock().unwrap()[0].title = jre("#2571 - Next Guest | 24 October 2026");
    let report = f.run().await;
    assert!(
        f.releases().await.is_empty(),
        "a title date must not drift to the 23rd"
    );
    assert_eq!(
        (report.released, report.not_wanted, report.notes_changed),
        (0, 0, 1)
    );
    assert_missing_podcast_target(&f.notes().await, "j2571", 2571);
}

#[tokio::test]
async fn a_podcast_missing_target_still_uses_a_free_tba_neighbour() {
    let f = missing_podcast_target_fixture(24).await;
    let report = f.run().await;
    assert_eq!(f.releases().await, releases(&[("S2026E139", "j2571")]));
    assert_eq!(
        (report.released, report.not_wanted, report.notes_changed),
        (1, 0, 0)
    );
    assert!(f.notes().await.is_empty());
}

#[tokio::test]
async fn podcast_missing_target_notes_do_not_include_filtered_or_short_uploads() {
    for day in [24, 28] {
        let mut f = missing_podcast_target_fixture(day).await;
        f.mapping.excludes = vec!["Members".to_owned()];
        *f.candidates.videos.lock().unwrap() = vec![
            cand(
                "short",
                JRE,
                &jre("#2571 - Short Guest"),
                cdt(day, 14),
                Some(12),
            ),
            cand(
                "looked-up-short",
                JRE,
                &jre("#2572 - Short Guest"),
                cdt(day, 14),
                None,
            ),
            cand(
                "excluded",
                JRE,
                &jre("#2573 - Members episode"),
                cdt(day, 14),
                Some(150),
            ),
            cand("promo", JRE, &jre("#2574 - Promo"), cdt(day, 14), Some(150)),
            cand(
                "shorts",
                JRE,
                &jre("#2575 #shorts"),
                cdt(day, 14),
                Some(150),
            ),
            cand(
                "other-show",
                JRE,
                "JRE MMA Show #2576",
                cdt(day, 14),
                Some(150),
            ),
            cand(
                "other-channel",
                TVF,
                &jre("#2577 - Guest"),
                cdt(day, 14),
                Some(150),
            ),
            cand(
                "old",
                JRE,
                &jre("#2400 - Old Guest"),
                cdt(day, 14),
                Some(150),
            ),
        ];
        f.meta.set(
            "looked-up-short",
            Known {
                duration_secs: Some(12 * MIN),
                ..Known::default()
            },
        );
        let report = f.run().await;
        assert!(f.releases().await.is_empty());
        assert!(
            f.notes().await.is_empty(),
            "filtered candidates do not gain missing-target notes"
        );
        assert_eq!(report.not_wanted, 3, "two short uploads and one old number");
    }
}

#[tokio::test]
async fn podcast_missing_target_notes_use_the_mapping_duration_floor() {
    for day in [24, 28] {
        let mut f = missing_podcast_target_fixture(day).await;
        // An explicit date cannot drift onto the free TBA neighbour on the 23rd.
        f.candidates.videos.lock().unwrap()[0].title =
            jre(&format!("#2571 - Next Guest | {day} October 2026"));
        f.mapping.min_duration_secs = Some(180 * MIN);
        let short = f.run().await;
        assert!(f.releases().await.is_empty());
        assert!(f.notes().await.is_empty());
        assert_eq!((short.not_wanted, short.notes_changed), (1, 0));

        // The watcher's duration is absent, but the resolver knows it meets the floor.
        f.candidates.videos.lock().unwrap()[0].duration_secs = None;
        f.meta.set(
            "j2571",
            Known {
                duration_secs: Some(180 * MIN),
                height: Some(1080),
                ..Known::default()
            },
        );
        let full = f.run().await;
        assert!(f.releases().await.is_empty());
        assert_eq!((full.not_wanted, full.notes_changed), (0, 1));
        assert_missing_podcast_target(&f.notes().await, "j2571", 2571);
    }
}

#[tokio::test]
async fn podcast_missing_target_notes_keep_unknown_duration_provisionally_eligible() {
    for day in [24, 28] {
        let f = missing_podcast_target_fixture(day).await;
        f.candidates.videos.lock().unwrap()[0].title =
            jre(&format!("#2571 - Next Guest | {day} October 2026"));
        f.candidates.videos.lock().unwrap()[0].duration_secs = None;
        let report = f.run().await;
        assert!(f.releases().await.is_empty());
        assert_eq!((report.not_wanted, report.notes_changed), (0, 1));
        assert_missing_podcast_target(&f.notes().await, "j2571", 2571);
    }
}

#[tokio::test]
async fn a_podcast_clip_under_the_floor_never_takes_the_episode() {
    let f = podcast_fixture(true).await;
    f.candidates
        .videos
        .lock()
        .unwrap()
        .retain(|c| c.video_id == "j2569clip");
    f.run().await;
    assert!(f.releases().await.is_empty());
    let notes = f.notes().await;
    assert_eq!(notes["j2569clip"].state, UnmatchedState::Unsure);
    assert!(
        notes["j2569clip"]
            .reason
            .contains("#2569: 12 min is under the 25 min floor"),
        "{}",
        notes["j2569clip"].reason
    );
}

#[tokio::test]
async fn a_backfill_limit_keeps_the_newest_numbered_back_episode() {
    let mut f = podcast_fixture(true).await;
    f.mapping =
        f.db.set_backfill_limit(f.mapping.id, Some(1))
            .await
            .unwrap()
            .unwrap();
    // Added after every upload, so all of them are back episodes.
    f.mapping.created_at = cdt(30, 0);
    f.run().await;
    assert_eq!(f.releases().await, releases(&[("S2026E139", "j2570")]));
    let notes = f.notes().await;
    assert_eq!(notes["j2569"].state, UnmatchedState::Capped);
    assert!(
        notes["j2569"]
            .reason
            .contains("keeps only the newest back episode"),
        "{}",
        notes["j2569"].reason
    );
}

#[tokio::test]
async fn theo_von_titles_match_by_number() {
    let new = NewMapping {
        kind: MappingKind::Show,
        tvdb_id: Some(393_443),
        tmdb_id: None,
        title: "This Past Weekend w/ Theo Von".to_owned(),
        language: "English".to_owned(),
        channel_ids: vec![JRE.to_owned()],
        episode_regex: None,
        excludes: vec![],
        min_duration_secs: None,
    };
    let options = MatchOptions {
        title_number: true,
        ..MatchOptions::default()
    };
    let episodes = vec![WantedEpisode {
        season: 2026,
        episode: 40,
        air_date: Some("2026-10-20".to_owned()),
        title: Some("Nate Bargatze | #660".to_owned()),
        monitored: true,
    }];
    let f = Fixture::new(new, options, episodes).await;
    *f.candidates.videos.lock().unwrap() = vec![cand(
        "theo660",
        JRE,
        "Nate Bargatze | This Past Weekend w/ Theo Von #660",
        cdt(20, 10),
        Some(95),
    )];
    f.meta.height("theo660", 1080);
    f.run().await;
    assert_eq!(f.releases().await, releases(&[("S2026E40", "theo660")]));
}

/// Theo Von #677 to #686 as TVDB's weekly S2026E30 to S2026E39, all added a month after they went
/// up, and a channel list that dates them "1 month ago": every upload shares one rough date, and
/// #677 even looks a day newer than the rest. Listed newest number first, as the channel does.
async fn theo_back_catalogue(limit: i64) -> Fixture {
    let new = NewMapping {
        kind: MappingKind::Show,
        tvdb_id: Some(393_443),
        tmdb_id: None,
        title: "This Past Weekend w/ Theo Von".to_owned(),
        language: "English".to_owned(),
        channel_ids: vec![JRE.to_owned()],
        episode_regex: None,
        excludes: vec![],
        min_duration_secs: None,
    };
    let options = MatchOptions {
        title_number: true,
        ..MatchOptions::default()
    };
    let episodes = (677..=686)
        .map(|n| WantedEpisode {
            season: 2026,
            episode: n - 647,
            air_date: Some(format!("2026-08-{:02}", n - 670)),
            title: Some(format!("Guest {n} | #{n}")),
            monitored: true,
        })
        .collect();
    let mut f = Fixture::new(new, options, episodes).await;
    f.mapping =
        f.db.set_backfill_limit(f.mapping.id, Some(limit))
            .await
            .unwrap()
            .unwrap();
    f.mapping.created_at = cdt(30, 0);
    *f.candidates.videos.lock().unwrap() = (677..=686)
        .rev()
        .map(|n| {
            let at = if n == 677 { cdt(2, 0) } else { cdt(1, 0) };
            guessed(cand(
                &format!("theo{n}"),
                JRE,
                &format!("Guest {n} | This Past Weekend w/ Theo Von #{n}"),
                at,
                Some(90),
            ))
        })
        .collect();
    for n in 677..=686 {
        f.meta.height(&format!("theo{n}"), 1080);
    }
    f
}

fn theo(numbers: impl IntoIterator<Item = i64>) -> BTreeMap<String, Vec<String>> {
    numbers
        .into_iter()
        .map(|n| (format!("S2026E{}", n - 647), vec![format!("theo{n}")]))
        .collect()
}

#[tokio::test]
async fn a_backfill_limit_keeps_the_newest_episodes_whatever_their_rough_upload_dates() {
    let f = theo_back_catalogue(5).await;
    f.run().await;
    assert_eq!(f.releases().await, theo(682..=686));
    let notes = f.notes().await;
    for n in 677..=681 {
        assert_eq!(notes[&format!("theo{n}")].state, UnmatchedState::Capped);
    }
    // A second run keeps the same five.
    let again = f.run().await;
    assert_eq!((again.released, again.replaced), (0, 0));

    // A feed gives #686 its exact time: its release takes it, and the five stay.
    for c in f.candidates.videos.lock().unwrap().iter_mut() {
        if c.video_id == "theo686" {
            c.published_at = cdt(1, 17) - 30 * DAY;
            c.published_approx = false;
        }
    }
    let exact = f.run().await;
    assert_eq!((exact.released, exact.replaced), (0, 0));
    assert_eq!(f.releases().await, theo(682..=686));
    let stored =
        f.db.releases_for_mappings(&[f.mapping.id], &EpisodeFilter::Any, 100)
            .await
            .unwrap();
    let r686 = stored.iter().find(|r| r.video_id == "theo686").unwrap();
    assert_eq!(r686.published_at, cdt(1, 17) - 30 * DAY);
}

#[tokio::test]
async fn a_corrected_order_swaps_the_kept_episodes_and_leaves_pins_alone() {
    let f = theo_back_catalogue(3).await;
    // What the old order released: #677 to #679 by their rough dates, and #678 pinned.
    for n in 677..=679 {
        f.db.insert_release(&NewRelease {
            mapping_id: f.mapping.id,
            video_id: format!("theo{n}"),
            part_ids: vec![],
            target: PinTarget::Episode {
                season: 2026,
                episode: n - 647,
            },
            year: None,
            imdb_id: None,
            video_title: format!("Guest {n} | This Past Weekend w/ Theo Von #{n}"),
            language: "English".to_owned(),
            height: 1080,
            size: None,
            published_at: cdt(1, 0),
        })
        .await
        .unwrap();
    }
    let pinned = PinTarget::Episode {
        season: 2026,
        episode: 678 - 647,
    };
    f.db.set_pin(f.mapping.id, &pinned, "theo678")
        .await
        .unwrap();

    let report = f.run().await;
    assert_eq!(f.releases().await, theo([678, 684, 685, 686]));
    assert_eq!((report.released, report.replaced), (3, 2), "{report:?}");
    let notes = f.notes().await;
    for n in [677, 679, 680] {
        assert_eq!(notes[&format!("theo{n}")].state, UnmatchedState::Capped);
    }
    assert!(!notes.contains_key("theo678"));
}

#[tokio::test]
async fn an_unmonitored_number_is_not_released_and_not_dated() {
    let mut f = podcast_fixture(true).await;
    // #2569 is S2026E136, and the TBA episode a day after its upload: were #2569 missing, its
    // upload would go by date and take the TBA episode ahead of #2570.
    f.episodes[2].monitored = false;
    f.episodes[5].air_date = Some("2026-10-22".to_owned());
    let report = f.run().await;
    assert_eq!(
        f.releases().await,
        releases(&[("S2026E139", "j2570")]),
        "#2569 stays off the TBA episode"
    );
    assert_eq!(report.not_wanted, 1, "#2400 only");
    let notes = f.notes().await;
    let j2569 = &notes["j2569"];
    assert_eq!(j2569.state, UnmatchedState::Unsure);
    assert!(
        j2569
            .reason
            .contains("#2569 is S2026E136, unmonitored in Sonarr"),
        "{}",
        j2569.reason
    );
    assert_eq!((j2569.season, j2569.episode), (Some(2026), Some(136)));
}

#[tokio::test]
async fn a_dated_podcast_episode_tvdb_drops_loses_its_release() {
    let mut f = podcast_fixture(true).await;
    f.run().await;
    assert_eq!(
        f.releases().await,
        releases(&[("S2026E136", "j2569"), ("S2026E139", "j2570")])
    );

    // TVDB replaces the TBA episode with one a day later: #2570 moves there, one release only.
    f.episodes[5].episode = 140;
    f.episodes[5].air_date = Some("2026-10-24".to_owned());
    let report = f.run().await;
    assert_eq!(
        f.releases().await,
        releases(&[("S2026E136", "j2569"), ("S2026E140", "j2570")])
    );
    assert_eq!((report.released, report.replaced), (1, 1));
}

#[tokio::test]
async fn without_title_number_a_podcast_matches_as_before() {
    let f = podcast_fixture(false).await;
    let report = f.run().await;
    assert!(f.releases().await.is_empty());
    assert_eq!(report.released, 0);
    // The show matcher reads "#2569" as no episode number.
    let notes = f.notes().await;
    assert_eq!(notes["j2569"].state, UnmatchedState::Unsure);
    assert!(
        notes["j2569"].reason.contains("no episode number"),
        "{}",
        notes["j2569"].reason
    );
}

const PODCAST: &str = "UCpodcastOfficial000000";

#[tokio::test]
async fn a_clip_never_takes_a_daily_episode_under_the_floor() {
    let new = NewMapping {
        kind: MappingKind::Daily,
        tvdb_id: Some(400_200),
        tmdb_id: None,
        title: "The Joe Rogan Experience".to_owned(),
        language: "English".to_owned(),
        channel_ids: vec![PODCAST.to_owned()],
        episode_regex: Some(r"^#\d+ - ".to_owned()),
        excludes: vec![],
        min_duration_secs: Some(20 * MIN),
    };
    let f = Fixture::new(new, MatchOptions::default(), wanted_dates()).await;
    // The clip goes up before the full episode on the 2nd. On the 3rd only a clip is up so far.
    *f.candidates.videos.lock().unwrap() = vec![
        cand(
            "clip2569",
            PODCAST,
            "#2569 - Bill Thompson (clip)",
            plus5(2, 9, 0),
            Some(3),
        ),
        cand(
            "full2569",
            PODCAST,
            "#2569 - Bill Thompson",
            plus5(2, 10, 0),
            Some(150),
        ),
        cand(
            "clip2570",
            PODCAST,
            "#2570 - Bill Thompson (clip)",
            plus5(3, 9, 0),
            Some(3),
        ),
    ];
    for id in ["clip2569", "full2569", "clip2570"] {
        f.meta.height(id, 1080);
    }
    f.run().await;
    assert_eq!(f.releases().await, releases(&[("2026-10-02", "full2569")]));
    let notes = f.notes().await;
    assert!(!notes.contains_key("clip2569"));
    assert_eq!(notes["clip2570"].state, UnmatchedState::Unsure);
    assert!(
        notes["clip2570"]
            .reason
            .contains("3 min is under the 20 min floor"),
        "{}",
        notes["clip2570"].reason
    );
}
