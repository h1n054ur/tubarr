//! The film matcher on a Goldmines-style upload list: dubs, aliases, extras, remakes, re-uploads,
//! pins and idempotency.

use std::{collections::HashMap, sync::Mutex, time::Duration};

use tubarr::{
    matcher::{
        Candidate, Candidates, FilmSettings, Known, Metadata, Report, WantedFilms, match_films,
    },
    store::{
        Db, EpisodeFilter, Mapping, MappingKind, NewMapping, PinTarget, StoreError, StoredRelease,
        Unmatched, UnmatchedState,
    },
};

const GOLDMINES: &str = "UCgoldminesOfficial0000";
const GOLDMINES_MOVIES: &str = "UCgoldminesMovies000000";
const SHEMAROO: &str = "UCshemarooOfficial00000";
const OTHER: &str = "UCrandomReuploader00000";
const MIN: i64 = 60;
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

struct FakeWanted(Mutex<Vec<Mapping>>);

impl WantedFilms for FakeWanted {
    async fn films(&self) -> Result<Vec<Mapping>, StoreError> {
        Ok(self.0.lock().unwrap().clone())
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

fn cand(id: &str, channel: &str, title: &str, day: i64, mins: Option<i64>) -> Candidate {
    Candidate {
        video_id: id.to_owned(),
        channel_id: channel.to_owned(),
        title: title.to_owned(),
        published_at: T0 + day * 86_400,
        duration_secs: mins.map(|m| m * MIN),
        published_approx: false,
    }
}

/// What the watcher found on the film channels, traps included.
fn uploads() -> Vec<Candidate> {
    vec![
        // Theri: the main upload, a later 4K re-upload that runs longer, the Telugu dub title,
        // a re-upload on Shemaroo, and the extras.
        cand(
            "theri",
            GOLDMINES,
            "Theri (2016) Full Hindi Dubbed Movie | Vijay, Samantha, Amy Jackson | Atlee",
            1,
            Some(155),
        ),
        cand(
            "theri4k",
            GOLDMINES,
            "Theri Hindi Dubbed Full Movie 4K | Thalapathy Vijay",
            40,
            Some(158),
        ),
        cand(
            "policeodu",
            GOLDMINES_MOVIES,
            "Policeodu Full Movie Hindi Dubbed | Vijay, Samantha",
            2,
            Some(160),
        ),
        cand(
            "theriShemaroo",
            SHEMAROO,
            "Theri Full Movie | Hindi Dubbed | Vijay",
            3,
            Some(161),
        ),
        cand(
            "theriTrailer",
            GOLDMINES,
            "Theri Official Hindi Trailer | Vijay",
            0,
            Some(2),
        ),
        cand(
            "theriSong",
            GOLDMINES,
            "Jithu Jilladi Video Song | Theri Hindi Dubbed | Vijay",
            5,
            Some(4),
        ),
        cand(
            "theriScene",
            GOLDMINES,
            "Theri Best Action Scene | Hindi Dubbed Movie",
            6,
            Some(9),
        ),
        // Short and without a tell-tale word: dropped quietly next to the full uploads.
        cand(
            "theriClip",
            GOLDMINES,
            "Theri Hindi Dubbed Movie",
            7,
            Some(12),
        ),
        cand(
            "theri2",
            GOLDMINES,
            "Theri 2 Full Movie Hindi Dubbed",
            8,
            Some(150),
        ),
        // Kaththi: a remake year, two halves, a transliteration close enough for a note only.
        cand(
            "kaththi1990",
            GOLDMINES,
            "Kaththi (1990) Full Movie Hindi Dubbed",
            9,
            Some(140),
        ),
        cand(
            "kaththiP1",
            GOLDMINES,
            "Kaththi Full Movie Part 1 | Hindi Dubbed",
            10,
            Some(80),
        ),
        cand(
            "kaththiP2",
            GOLDMINES,
            "Kaththi Full Movie Part 2 | Hindi Dubbed",
            10,
            Some(80),
        ),
        cand(
            "kathhi",
            GOLDMINES,
            "Kathhi Full Movie Hindi Dubbed",
            11,
            Some(150),
        ),
        // Mersal only short so far, on a global channel,, and Sarrainodu with no duration yet.
        cand(
            "mersalShort",
            SHEMAROO,
            "Mersal (2017) Full Movie Hindi Dubbed",
            12,
            Some(45),
        ),
        cand(
            "sarrainodu",
            GOLDMINES,
            "Sarrainodu (2016) New Released Hindi Dubbed Full Movie | Allu Arjun",
            13,
            None,
        ),
        cand(
            "offchan",
            OTHER,
            "Theri Full Movie Hindi Dubbed",
            1,
            Some(155),
        ),
    ]
}

fn metadata() -> FakeMetadata {
    let meta = FakeMetadata::default();
    for id in [
        "theri",
        "policeodu",
        "theriShemaroo",
        "kathhi",
        "mersalShort",
    ] {
        meta.height(id, 1080);
    }
    meta.height("theri4k", 2160);
    meta
}

fn settings() -> FilmSettings {
    FilmSettings {
        channels: vec![GOLDMINES_MOVIES.to_owned(), SHEMAROO.to_owned()],
        ..FilmSettings::default()
    }
}

struct Fixture {
    db: Db,
    _dir: tempfile::TempDir,
    theri: Mapping,
    kaththi: Mapping,
    mersal: Mapping,
    sarrainodu: Mapping,
    candidates: FakeCandidates,
    wanted: FakeWanted,
    meta: FakeMetadata,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("tubarr.db")).await.unwrap();
        let add = async |tmdb: i64, title: &str, year: i64, imdb: &str, channels: &[&str]| {
            let mut m = db
                .insert_mapping(&NewMapping {
                    kind: MappingKind::Film,
                    tvdb_id: None,
                    tmdb_id: Some(tmdb),
                    title: title.to_owned(),
                    language: "Tamil".to_owned(),
                    channel_ids: channels.iter().map(|c| (*c).to_owned()).collect(),
                    episode_regex: None,
                    excludes: vec![],
                    min_duration_secs: None,
                })
                .await
                .unwrap();
            // Year and IMDb id come from the Radarr sync, which a hand-made mapping skips.
            m.year = Some(year);
            m.imdb_id = Some(imdb.to_owned());
            m
        };
        let theri = add(352_173, "Theri", 2016, "tt4938374", &[GOLDMINES]).await;
        let kaththi = add(298_250, "Kaththi", 2014, "tt3742378", &[GOLDMINES]).await;
        let mersal = add(463_612, "Mersal", 2017, "tt6587640", &[]).await;
        let sarrainodu = add(390_634, "Sarrainodu", 2016, "tt5446858", &[GOLDMINES]).await;
        db.set_aliases(theri.id, &["Policeodu".to_owned()])
            .await
            .unwrap();
        let wanted = FakeWanted(Mutex::new(vec![
            theri.clone(),
            kaththi.clone(),
            mersal.clone(),
            sarrainodu.clone(),
        ]));
        Self {
            db,
            _dir: dir,
            theri,
            kaththi,
            mersal,
            sarrainodu,
            candidates: FakeCandidates(Mutex::new(uploads())),
            wanted,
            meta: metadata(),
        }
    }

    async fn run(&self) -> Report {
        self.run_with(&settings()).await
    }

    async fn run_with(&self, settings: &FilmSettings) -> Report {
        match_films(
            &self.db,
            &self.candidates,
            &self.wanted,
            &self.meta,
            settings,
        )
        .await
        .unwrap()
    }

    async fn releases(&self, film: &Mapping) -> Vec<StoredRelease> {
        self.db
            .releases_for_mappings(&[film.id], &EpisodeFilter::Any, 100)
            .await
            .unwrap()
    }

    async fn notes(&self, film: &Mapping) -> HashMap<String, Unmatched> {
        self.db
            .unmatched(film.id)
            .await
            .unwrap()
            .into_iter()
            .map(|n| (n.video_id.clone(), n))
            .collect()
    }
}

#[tokio::test]
async fn first_channel_wins_then_the_longest_upload() {
    let fx = Fixture::new().await;
    fx.run().await;
    let releases = fx.releases(&fx.theri).await;
    assert_eq!(releases.len(), 1, "{releases:#?}");
    let r = &releases[0];
    // On the film's own channel, the 4K re-upload runs longest. The longer Shemaroo and
    // Policeodu uploads are on global channels, which come after the film's own.
    assert_eq!(r.video_id, "theri4k");
    assert_eq!(r.target, PinTarget::Film);
    assert_eq!(r.year, Some(2016));
    assert_eq!(r.imdb_id, Some(4_938_374));
    assert_eq!(r.height, 2160);
    assert_eq!(r.arr_title, "Theri");
    // The title says Hindi Dubbed: Radarr's Tamil does not apply.
    assert_eq!(r.language, "Hindi");

    let notes = fx.notes(&fx.theri).await;
    for id in ["theri", "policeodu", "theriShemaroo"] {
        let n = &notes[id];
        assert_eq!(n.state, UnmatchedState::Duplicate, "{id}");
        assert!(n.reason.contains("kept theri4k"), "{}", n.reason);
    }
    // Extras, other films, a clip next to the full uploads and other channels: no notes.
    for id in [
        "theriTrailer",
        "theriSong",
        "theriScene",
        "theriClip",
        "theri2",
        "offchan",
    ] {
        assert!(!notes.contains_key(id), "{id}: {:?}", notes.get(id));
    }
}

#[tokio::test]
async fn equal_length_goes_to_the_higher_upload_then_the_earliest() {
    let fx = Fixture::new().await;
    {
        let mut all = fx.candidates.0.lock().unwrap();
        all.retain(|c| ["theri", "theri4k"].contains(&c.video_id.as_str()));
        for c in all.iter_mut() {
            c.duration_secs = Some(155 * MIN);
        }
        all.push(cand(
            "theriLater",
            GOLDMINES,
            "Theri Full Movie Hindi Dubbed",
            50,
            Some(155),
        ));
    }
    fx.meta.height("theriLater", 2160);
    fx.run().await;
    // theri4k and theriLater tie on length and height: the earlier upload wins.
    assert_eq!(fx.releases(&fx.theri).await[0].video_id, "theri4k");
}

#[tokio::test]
async fn remakes_and_near_names_are_unsure_and_parts_join() {
    let fx = Fixture::new().await;
    // The parts wait for their lookups.
    fx.run().await;
    assert!(fx.releases(&fx.kaththi).await.is_empty());
    let notes = fx.notes(&fx.kaththi).await;
    assert_eq!(notes["kaththiP1"].state, UnmatchedState::Pending);
    assert_eq!(notes["kaththiP2"].part, Some(2));

    // With no full upload of the 2014 film, its two halves are released as one.
    fx.meta.height("kaththiP1", 1080);
    fx.meta.height("kaththiP2", 1080);
    fx.run().await;
    let releases = fx.releases(&fx.kaththi).await;
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].key(), "kaththiP1,kaththiP2");
    assert_eq!(releases[0].language, "Hindi");
    let notes = fx.notes(&fx.kaththi).await;
    assert!(!notes.contains_key("kaththiP1") && !notes.contains_key("kaththiP2"));
    let remake = &notes["kaththi1990"];
    assert_eq!(remake.state, UnmatchedState::Unsure);
    assert!(remake.reason.contains("1990") && remake.reason.contains("2014"));
    let near = &notes["kathhi"];
    assert_eq!(near.state, UnmatchedState::Unsure);
    assert!(near.reason.contains("alias"), "{}", near.reason);

    // An alias settles the transliteration, and that full upload replaces the parts.
    fx.db
        .set_aliases(fx.kaththi.id, &["Kathhi".to_owned()])
        .await
        .unwrap();
    fx.run().await;
    let releases = fx.releases(&fx.kaththi).await;
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].video_id, "kathhi");
    assert!(!fx.notes(&fx.kaththi).await.contains_key("kathhi"));
}

#[tokio::test]
async fn duration_floor_and_pending_lookups() {
    let fx = Fixture::new().await;
    let report = fx.run().await;
    // Mersal: global channels only, 45 min is under the 80 min floor.
    assert!(fx.releases(&fx.mersal).await.is_empty());
    let short = &fx.notes(&fx.mersal).await["mersalShort"];
    assert_eq!(short.state, UnmatchedState::Unsure);
    assert!(short.reason.contains("45 min is under the 80 min floor"));
    // Sarrainodu: duration unknown until the resolver looks it up.
    assert_eq!(
        fx.notes(&fx.sarrainodu).await["sarrainodu"].state,
        UnmatchedState::Pending
    );
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);

    // A lower configured floor lets Mersal through.
    let low = FilmSettings {
        min_duration: Duration::from_secs(40 * 60),
        ..settings()
    };
    fx.run_with(&low).await;
    assert_eq!(fx.releases(&fx.mersal).await[0].video_id, "mersalShort");

    // The resolver learns Sarrainodu's duration and height.
    fx.meta.set(
        "sarrainodu",
        Known {
            duration_secs: Some(160 * MIN),
            height: Some(1080),
            ..Known::default()
        },
    );
    fx.run().await;
    assert_eq!(fx.releases(&fx.sarrainodu).await.len(), 1);
}

#[tokio::test]
async fn no_channels_at_all_is_skipped() {
    let fx = Fixture::new().await;
    let report = fx.run_with(&FilmSettings::default()).await;
    assert_eq!(
        report.skipped,
        [(fx.mersal.id, "no film channels set".to_owned())]
    );
}

#[tokio::test]
async fn running_twice_changes_nothing() {
    let fx = Fixture::new().await;
    let first = fx.run().await;
    assert_eq!(first.released, 1);
    assert!(first.notes_changed > 0);
    let before = fx.releases(&fx.theri).await;
    let second = fx.run().await;
    assert_eq!(second.released, 0);
    assert_eq!(second.replaced, 0);
    assert_eq!(second.notes_changed, 0);
    assert_eq!(fx.releases(&fx.theri).await, before);
}

#[tokio::test]
async fn a_pin_overrides_the_pick() {
    let fx = Fixture::new().await;
    fx.run().await;
    fx.db
        .set_pin(fx.theri.id, &PinTarget::Film, "policeodu")
        .await
        .unwrap();
    let report = fx.run().await;
    assert_eq!(report.replaced, 1);
    let releases = fx.releases(&fx.theri).await;
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].video_id, "policeodu");
    assert_eq!(releases[0].language, "Hindi");
    let notes = fx.notes(&fx.theri).await;
    assert!(!notes.contains_key("policeodu"));
    assert!(notes["theri4k"].reason.contains("pinned"));

    // A pin on a video the resolver has not looked up yet: the current release stays until the
    // lookup lands, as for shows.
    fx.db
        .set_pin(fx.theri.id, &PinTarget::Film, "unseen00001")
        .await
        .unwrap();
    fx.run().await;
    assert_eq!(fx.releases(&fx.theri).await[0].video_id, "policeodu");
    assert_eq!(
        fx.notes(&fx.theri).await["unseen00001"].state,
        UnmatchedState::Pending
    );
    fx.meta.set(
        "unseen00001",
        Known {
            title: Some("Theri Full Movie".to_owned()),
            published_at: Some(T0),
            height: Some(720),
            duration_secs: Some(150 * MIN),
            size: None,
        },
    );
    fx.run().await;
    let releases = fx.releases(&fx.theri).await;
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].video_id, "unseen00001");
    // No language in the pinned title: the mapping's.
    assert_eq!(releases[0].language, "Tamil");
}

#[tokio::test]
async fn shows_are_left_alone() {
    let fx = Fixture::new().await;
    let show = fx
        .db
        .insert_mapping(&NewMapping {
            kind: MappingKind::Show,
            tvdb_id: Some(1),
            tmdb_id: None,
            title: "Theri".to_owned(),
            language: "Urdu".to_owned(),
            channel_ids: vec![GOLDMINES.to_owned()],
            episode_regex: None,
            excludes: vec![],
            min_duration_secs: None,
        })
        .await
        .unwrap();
    *fx.wanted.0.lock().unwrap() = vec![show.clone()];
    let report = fx.run().await;
    assert_eq!(report, Report::default());
    assert!(fx.releases(&show).await.is_empty());
}
