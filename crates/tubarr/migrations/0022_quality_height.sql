-- Heights become resolution labels: the standard tier Sonarr and Radarr know (2160, 1080, 720,
-- 576, 480, 360), by width and height as scene names give it. A cinematic 1920x804 was named
-- `804p`, which the arrs read as 480p. Lookups still cached know the width; without it the height
-- alone decides, and the next lookup corrects it. Same thresholds as `quality_height` in
-- `newznab/title.rs`.
CREATE TEMP TABLE frame_tier AS
WITH frames AS (
    SELECT v.video_id, v.height AS h,
        coalesce((SELECT json_extract(r.formats, '$.video.width') FROM resolve_cache r
                  WHERE r.video_id = v.video_id), 0) AS w
    FROM videos v WHERE v.height IS NOT NULL
    UNION ALL
    SELECT rel.video_id, rel.height AS h,
        coalesce((SELECT json_extract(r.formats, '$.video.width') FROM resolve_cache r
                  WHERE r.video_id = rel.video_id), 0) AS w
    FROM releases rel
),
sides AS (
    SELECT DISTINCT video_id, h,
        CASE WHEN w > 0 THEN max(w, h) ELSE 0 END AS long,
        CASE WHEN w > 0 THEN min(w, h) ELSE h END AS short
    FROM frames
)
SELECT video_id, h,
    CASE
        WHEN long >= 3200 OR short >= 1800 THEN 2160
        WHEN long >= 1800 OR short >= 1000 THEN 1080
        WHEN long >= 1200 OR short >= 700 THEN 720
        WHEN long >= 1000 OR short >= 560 THEN 576
        WHEN long >= 800 OR short >= 440 THEN 480
        ELSE 360
    END AS tier
FROM sides;

UPDATE videos SET height = (
    SELECT t.tier FROM frame_tier t WHERE t.video_id = videos.video_id AND t.h = videos.height
) WHERE height IS NOT NULL;

UPDATE releases SET height = (
    SELECT t.tier FROM frame_tier t WHERE t.video_id = releases.video_id AND t.h = releases.height
);

DROP TABLE frame_tier;
