use anyhow::{anyhow, Context, Result};
use aws_config::meta::region::RegionProviderChain;
use aws_sdk_s3::{config::Builder as S3Builder, Client as S3Client};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use feed_rs::{model::Feed, parser};
use log::{info, warn};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    env,
    fs::File,
    io::Write,
    sync::OnceLock,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

const FEED: &str = "https://www.omnycontent.com/d/playlist/8c0a4104-a688-4e57-91fd-ad7b00d5dddd/c2325e96-d6ad-4206-b72b-ad8e00e5f4fe/bbc8a8c5-8da7-46ef-843f-ad8e00e5f515/podcast.rss";
const BUCKET: &str = "governosombra";
const OPENROUTER_MODEL: &str = "openai/gpt-5.6-luna";
const RSS_CACHE_TTL: Duration = Duration::from_secs(10 * 60);
const RSS_TIMEOUT: Duration = Duration::from_secs(30);
const AUDIO_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const OPENROUTER_TIMEOUT: Duration = Duration::from_secs(2 * 60);

static HTTP: OnceLock<Client> = OnceLock::new();
static CACHE: OnceLock<Mutex<Option<CachedEpisodes>>> = OnceLock::new();

struct CachedEpisodes {
    fetched_at: Instant,
    episodes: Vec<Episode>,
}

fn http() -> &'static Client {
    HTTP.get_or_init(|| {
        Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .pool_idle_timeout(Duration::from_secs(90))
            .build()
            .expect("the shared HTTP client configuration is valid")
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    pub url: String,
    pub title: String,
    pub file_location: String,
    pub thumbnail_url: String,
    pub transcript_location: String,
    pub number: i32,
    pub date: DateTime<Utc>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Book {
    pub title: String,
    pub author: String,
    pub episode_number: i32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BooksDatabase {
    pub processed_episodes: Vec<i32>,
    pub books: Vec<Book>,
}

async fn fetch_rss_feed() -> Result<Feed> {
    let bytes = http()
        .get(FEED)
        .timeout(RSS_TIMEOUT)
        .send()
        .await
        .context("request RSS feed")?
        .error_for_status()
        .context("RSS status")?
        .bytes()
        .await
        .context("read RSS feed")?;
    parser::parse(bytes.as_ref()).context("parse RSS feed")
}

pub async fn get_episodes() -> Result<Vec<Episode>> {
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    let mut guard = cache.lock().await;
    if let Some(cached) = guard.as_ref() {
        if cached.fetched_at.elapsed() < RSS_CACHE_TTL {
            return Ok(cached.episodes.clone());
        }
    }
    let stale = guard.as_ref().map(|cached| cached.episodes.clone());
    let feed = match fetch_rss_feed().await {
        Ok(f) => f,
        Err(error) => {
            if let Some(value) = stale {
                warn!("RSS refresh failed, serving cached episodes: {error:#}");
                return Ok(value);
            }
            return Err(error);
        }
    };
    let mut episodes = Vec::new();
    for entry in feed.entries {
        let Some(media) = entry.media.first() else {
            continue;
        };
        let Some(content) = media.content.first() else {
            continue;
        };
        let Some(url) = content.url.as_ref() else {
            continue;
        };
        let Some(title) = entry.title.as_ref() else {
            continue;
        };
        let Some(date) = entry.published else {
            continue;
        };
        let thumb = media
            .thumbnails
            .first()
            .map(|t| t.image.uri.to_string())
            .unwrap_or_default();
        episodes.push(Episode {
            url: url.to_string(),
            title: title.content.clone(),
            file_location: String::new(),
            thumbnail_url: thumb,
            transcript_location: String::new(),
            number: 0,
            date,
        });
    }
    if episodes.is_empty() {
        return Err(anyhow!("RSS feed contained no usable episodes"));
    }

    episodes.sort_by_key(|e| e.date);
    for (i, e) in episodes.iter_mut().enumerate() {
        e.number = i as i32 + 1;
        e.file_location = format!("episodes/{:03}.wav", e.number);
        e.transcript_location = format!("transcripts/{:03}.txt", e.number);
    }
    *guard = Some(CachedEpisodes {
        fetched_at: Instant::now(),
        episodes: episodes.clone(),
    });
    Ok(episodes)
}

async fn download_episode(episode: &Episode) -> Result<()> {
    let mp3 = episode.file_location.replace(".wav", ".mp3");
    let mut response = http()
        .get(&episode.url)
        .timeout(AUDIO_TIMEOUT)
        .send()
        .await
        .context("download episode")?
        .error_for_status()?;
    let mut file = tokio::fs::File::create(&mp3).await?;
    while let Some(chunk) = response.chunk().await? {
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await?;
    }
    let status = tokio::process::Command::new("ffmpeg")
        .args(["-y", "-i", &mp3, "-ar", "16000", &episode.file_location])
        .status()
        .await
        .context("run ffmpeg")?;
    if !status.success() {
        return Err(anyhow!("ffmpeg failed: {status}"));
    }
    tokio::fs::remove_file(mp3).await?;
    Ok(())
}

fn format_time(s: i64) -> String {
    format!("{:02}:{:02}:{:02}", (s / 3600), (s % 3600) / 60, s % 60)
}
fn get_transcript(e: &Episode) -> Result<()> {
    if std::path::Path::new(&e.transcript_location).exists() {
        return Ok(());
    }
    let ctx = WhisperContext::new_with_params("ggml-base.bin", WhisperContextParameters::default())
        .context("load whisper model")?;
    let mut p = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    p.set_n_threads(4);
    p.set_language(Some("pt"));
    p.set_print_special(false);
    p.set_print_progress(true);
    p.set_print_realtime(true);
    p.set_print_timestamps(true);
    let reader = hound::WavReader::open(&e.file_location).context("open wav")?;
    let spec = reader.spec();
    let samples: Vec<i16> = reader
        .into_samples()
        .collect::<std::result::Result<_, _>>()?;
    let mut audio = vec![0f32; samples.len()];
    whisper_rs::convert_integer_to_float_audio(&samples, &mut audio)?;
    if spec.channels == 2 {
        audio = whisper_rs::convert_stereo_to_mono_audio(&audio)?
    } else if spec.channels != 1 {
        return Err(anyhow!("unsupported channel count"));
    }
    if spec.sample_rate != 16000 {
        return Err(anyhow!("sample rate must be 16KHz"));
    }
    let mut state = ctx.create_state().context("create whisper state")?;
    state.full(p, &audio).context("transcribe audio")?;
    let mut file = File::create(&e.transcript_location)?;
    for i in 0..state.full_n_segments() {
        if let Some(seg) = state.get_segment(i) {
            writeln!(
                file,
                "[{} - {}]: {}",
                format_time(seg.start_timestamp()),
                format_time(seg.end_timestamp()),
                seg.to_str_lossy().context("decode transcript segment")?
            )?;
        }
    }
    Ok(())
}

pub async fn get_s3_client() -> Result<S3Client> {
    let endpoint = env::var("CLOUDFLARE_ENDPOINT").context("CLOUDFLARE_ENDPOINT")?;
    let region = RegionProviderChain::default_provider().or_else("us-east-1");
    let config = aws_config::from_env().region(region).load().await;
    Ok(S3Client::from_conf(
        S3Builder::from(&config).endpoint_url(endpoint).build(),
    ))
}
async fn get_transcribed_episodes(c: &S3Client) -> Result<Vec<i32>> {
    let out = c
        .list_objects_v2()
        .bucket(BUCKET)
        .prefix("transcripts")
        .send()
        .await?;
    Ok(out
        .contents
        .unwrap_or_default()
        .into_iter()
        .filter_map(|o| o.key?.split('/').nth(1)?.split('.').next()?.parse().ok())
        .collect())
}
fn marker() -> String {
    let cet_now = Utc::now() + ChronoDuration::hours(1);
    format!("daily-transcriptions/{}.txt", cet_now.format("%Y-%m-%d"))
}

async fn has_transcribed_today(c: &S3Client) -> Result<bool> {
    match c.head_object().bucket(BUCKET).key(marker()).send().await {
        Ok(_) => Ok(true),
        Err(error)
            if error
                .as_service_error()
                .is_some_and(|service_error| service_error.is_not_found()) =>
        {
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}
async fn mark_transcribed_today(c: &S3Client, e: &Episode) -> Result<()> {
    c.put_object()
        .bucket(BUCKET)
        .key(marker())
        .body(e.number.to_string().into_bytes().into())
        .send()
        .await?;
    Ok(())
}
pub async fn get_transcript_for(c: &S3Client, e: &Episode) -> Result<String> {
    let data = c
        .get_object()
        .bucket(BUCKET)
        .key(format!("transcripts/{:03}.txt", e.number))
        .send()
        .await?
        .body
        .collect()
        .await?;
    Ok(String::from_utf8(data.to_vec())?)
}

#[derive(Debug, Deserialize)]
struct BookData {
    books: Vec<BookFields>,
}

#[derive(Debug, Deserialize)]
struct BookFields {
    title: String,
    author: String,
}

#[derive(Debug, Deserialize)]
struct OpenRouterResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: Message,
}

#[derive(Debug, Deserialize)]
struct Message {
    content: Option<String>,
}

fn parse_openrouter_books(response: OpenRouterResponse, episode_number: i32) -> Result<Vec<Book>> {
    let content = response
        .choices
        .first()
        .and_then(|choice| choice.message.content.as_deref())
        .ok_or_else(|| anyhow!("OpenRouter response has no choices or message content"))?;
    let data: BookData = serde_json::from_str(content).context("parse OpenRouter book JSON")?;

    data.books
        .into_iter()
        .map(|book| {
            let title = book.title.trim();
            let author = book.author.trim();
            if title.is_empty() || author.is_empty() {
                return Err(anyhow!(
                    "OpenRouter returned a book with an empty title or author"
                ));
            }
            Ok(Book {
                title: title.to_owned(),
                author: author.to_owned(),
                episode_number,
            })
        })
        .collect()
}

async fn get_list_of_books_from(c: &S3Client, e: &Episode) -> Result<Vec<Book>> {
    const EXTRACT_BOOKS_PROMPT: &str = "Extract every book mentioned in the podcast transcript. Return an empty books array when no books are mentioned. Do not invent titles or authors.";

    let transcript = get_transcript_for(c, e).await?;
    let key = env::var("OPENROUTER_API_KEY").context("OPENROUTER_API_KEY is not set")?;
    let body = json!({
        "model": OPENROUTER_MODEL,
        "messages": [
            {"role": "system", "content": EXTRACT_BOOKS_PROMPT},
            {"role": "user", "content": transcript}
        ],
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "book_mentions",
                "strict": true,
                "schema": {
                    "type": "object",
                    "properties": {
                        "books": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "title": {"type": "string"},
                                    "author": {"type": "string"}
                                },
                                "required": ["title", "author"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["books"],
                    "additionalProperties": false
                }
            }
        }
    });
    let response = http()
        .post("https://openrouter.ai/api/v1/chat/completions")
        .timeout(OPENROUTER_TIMEOUT)
        .bearer_auth(key)
        .header("HTTP-Referer", "https://governosombra.duarteocarmo.com")
        .header("X-OpenRouter-Title", "Governo Sombra Transcripts")
        .json(&body)
        .send()
        .await
        .context("request book extraction from OpenRouter")?
        .error_for_status()
        .context("OpenRouter returned an error status")?;
    let response: OpenRouterResponse = response
        .json()
        .await
        .context("decode OpenRouter response")?;
    parse_openrouter_books(response, e.number)
}

async fn update_books_list(c: &S3Client, new: &[Book], processed: &[i32]) -> Result<()> {
    let mut db = match c.get_object().bucket(BUCKET).key("books.json").send().await {
        Ok(response) => serde_json::from_slice(&response.body.collect().await?.to_vec())?,
        Err(error)
            if error
                .as_service_error()
                .is_some_and(|service_error| service_error.is_no_such_key()) =>
        {
            BooksDatabase {
                processed_episodes: vec![],
                books: vec![],
            }
        }
        Err(error) => return Err(error.into()),
    };
    for n in processed {
        if !db.processed_episodes.contains(n) {
            db.processed_episodes.push(*n)
        }
    }
    for b in new {
        if !db.books.iter().any(|x| {
            x.title == b.title && x.author == b.author && x.episode_number == b.episode_number
        }) {
            db.books.push(b.clone())
        }
    }
    c.put_object()
        .bucket(BUCKET)
        .key("books.json")
        .body(serde_json::to_vec(&db)?.into())
        .send()
        .await?;
    Ok(())
}
async fn get_episodes_with_books(c: &S3Client) -> Result<Vec<i32>> {
    let response = match c.get_object().bucket(BUCKET).key("books.json").send().await {
        Ok(response) => response,
        Err(error)
            if error
                .as_service_error()
                .is_some_and(|service_error| service_error.is_no_such_key()) =>
        {
            return Ok(Vec::new());
        }
        Err(error) => return Err(error.into()),
    };
    Ok(
        serde_json::from_slice::<BooksDatabase>(&response.body.collect().await?.to_vec())?
            .processed_episodes,
    )
}
pub async fn get_all_books(c: &S3Client) -> Result<Vec<Book>> {
    let r = c
        .get_object()
        .bucket(BUCKET)
        .key("books.json")
        .send()
        .await?;
    Ok(serde_json::from_slice::<BooksDatabase>(&r.body.collect().await?.to_vec())?.books)
}

pub async fn run(c: &S3Client) -> Result<()> {
    let episodes = get_episodes().await?;
    info!("Found {} episodes in the feed", episodes.len());

    let transcribed = get_transcribed_episodes(c).await?;
    if has_transcribed_today(c).await? {
        info!("A transcript has already been created today");
    } else if let Some(episode) = episodes
        .iter()
        .find(|episode| !transcribed.contains(&episode.number))
    {
        info!("Transcribing episode {}", episode.number);
        download_episode(episode).await?;
        let episode_for_transcription = episode.clone();
        tokio::task::spawn_blocking(move || get_transcript(&episode_for_transcription))
            .await
            .context("join transcription task")??;
        c.put_object()
            .bucket(BUCKET)
            .key(format!("transcripts/{:03}.txt", episode.number))
            .body(tokio::fs::read(&episode.transcript_location).await?.into())
            .send()
            .await?;
        mark_transcribed_today(c, episode).await?;

        for path in [&episode.file_location, &episode.transcript_location] {
            if let Err(error) = tokio::fs::remove_file(path).await {
                warn!("Could not remove temporary file {path}: {error}");
            }
        }
        info!("Uploaded transcript for episode {}", episode.number);
    } else {
        info!("All episodes have transcripts");
    }

    let episodes_with_books = get_episodes_with_books(c).await?;
    let transcribed = get_transcribed_episodes(c).await?;
    let mut episodes_to_process = episodes
        .iter()
        .filter(|episode| {
            transcribed.contains(&episode.number) && !episodes_with_books.contains(&episode.number)
        })
        .collect::<Vec<_>>();
    episodes_to_process.sort_by_key(|episode| std::cmp::Reverse(episode.number));

    let mut books = vec![];
    let mut processed = vec![];
    for episode in episodes_to_process.into_iter().take(10) {
        match get_list_of_books_from(c, episode).await {
            Ok(found_books) => {
                info!(
                    "Found {} books in episode {}",
                    found_books.len(),
                    episode.number
                );
                books.extend(found_books);
                processed.push(episode.number);
            }
            Err(error) => {
                warn!(
                    "Book extraction failed for episode {}: {error:#}",
                    episode.number
                )
            }
        }
    }

    if !processed.is_empty() {
        update_books_list(c, &books, &processed).await?;
        info!("Updated books for {} episodes", processed.len());
    }
    Ok(())
}

#[allow(dead_code)]
#[tokio::main]
async fn main() -> Result<()> {
    let c = get_s3_client().await?;
    run(&c).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openrouter_books() {
        let response: OpenRouterResponse = serde_json::from_str(
            r#"{"choices":[{"message":{"content":"{\"books\":[{\"title\":\"Dune\",\"author\":\"Frank Herbert\"}]}"}}]}"#,
        )
        .expect("valid fixture");

        let books = parse_openrouter_books(response, 483).expect("books should parse");
        assert_eq!(books.len(), 1);
        assert_eq!(books[0].title, "Dune");
        assert_eq!(books[0].author, "Frank Herbert");
        assert_eq!(books[0].episode_number, 483);
    }

    #[test]
    fn rejects_missing_openrouter_content() {
        let response: OpenRouterResponse =
            serde_json::from_str(r#"{"choices":[]}"#).expect("valid fixture");
        assert!(parse_openrouter_books(response, 483).is_err());
    }

    #[test]
    fn rejects_invalid_book_fields() {
        let response: OpenRouterResponse = serde_json::from_str(
            r#"{"choices":[{"message":{"content":"{\"books\":[{\"title\":\"\",\"author\":\"Unknown\"}]}"}}]}"#,
        )
        .expect("valid fixture");
        assert!(parse_openrouter_books(response, 483).is_err());
    }
}
