use anyhow::Result;
use aws_config::meta::region::RegionProviderChain;
use aws_sdk_s3::config::Builder as S3Builder;
use aws_sdk_s3::Client as S3Client;
use chrono::{DateTime, Utc};
use feed_rs::{model::Feed, parser};
use futures::{stream, StreamExt};
use hound;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::env;
use std::fs::File;
use std::io::Cursor;
use std::io::Write;
use std::process::Command;
use tokio;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext};

#[allow(dead_code)]
const CONCURRENT_REQUESTS: usize = 30;
const RSS_FEED_LOCATION: &str = "https://www.omnycontent.com/d/playlist/8c0a4104-a688-4e57-91fd-ad7b00d5dddd/c2325e96-d6ad-4206-b72b-ad8e00e5f4fe/bbc8a8c5-8da7-46ef-843f-ad8e00e5f515/podcast.rss";
const AUDIO_FILE_FOLDER: &str = "episodes/";
const TRANSCRIPT_FOLDER: &str = "transcripts/";
const BOOKS_JSON_PATH: &str = "books.json";

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

async fn fetch_rss_feed() -> Result<Feed, Box<dyn std::error::Error>> {
    let resp = reqwest::get(RSS_FEED_LOCATION).await?.text().await?;
    let feed = parser::parse(resp.as_bytes()).unwrap();

    return Ok(feed);
}

async fn download_episode(episode: &Episode) {
    let client = Client::new();
    let response = client.get(&episode.url).send().await;
    let bytes = response.unwrap().bytes().await.unwrap();

    let mp3_file = episode.file_location.replace(".wav", ".mp3");
    let mut file = std::fs::File::create(&mp3_file).unwrap();
    let mut content = Cursor::new(bytes);
    std::io::copy(&mut content, &mut file).unwrap();
    println!("Downloaded {}", mp3_file);

    let _output = Command::new("ffmpeg")
        .arg("-i")
        .arg(&mp3_file)
        .arg("-ar")
        .arg("16000")
        .arg(&episode.file_location)
        .output()
        .expect("failed to execute process");
    println!("Converted into {}", episode.file_location);

    std::fs::remove_file(&mp3_file).unwrap();
    println!("Removed {}", mp3_file);
}

#[allow(dead_code)]
async fn download_episodes_from(list_of_episodes: &Vec<Episode>) {
    let client = Client::new();

    let episodes_to_download = list_of_episodes
        .iter()
        .filter(|episode| !std::path::Path::new(&episode.file_location).exists())
        .cloned()
        .collect::<Vec<Episode>>();

    println!(
        "{} episodes to download from {} total.",
        episodes_to_download.len(),
        list_of_episodes.len()
    );

    let bodies = stream::iter(episodes_to_download)
        .map(|episode| {
            let client = &client;

            async move {
                let response = client.get(&episode.url).send().await?;
                response.bytes().await.map(|bytes| (episode, bytes))
            }
        })
        .buffer_unordered(CONCURRENT_REQUESTS);

    bodies
        .for_each(|result| async move {
            match result {
                Ok((episode, bytes)) => {
                    let mp3_file = episode.file_location.replace(".wav", ".mp3");
                    let mut file = std::fs::File::create(&mp3_file).unwrap();
                    let mut content = Cursor::new(bytes);
                    std::io::copy(&mut content, &mut file).unwrap();
                    println!("Downloaded {}", mp3_file);

                    let _output = Command::new("ffmpeg")
                        .arg("-i")
                        .arg(&mp3_file)
                        .arg("-ar")
                        .arg("16000")
                        .arg(&episode.file_location)
                        .output()
                        .expect("failed to execute process");
                    println!("Converted into {}", episode.file_location);

                    std::fs::remove_file(&mp3_file).unwrap();
                    println!("Removed {}", mp3_file);
                }
                Err(e) => {
                    eprint!("Error: {}", e);
                }
            }
        })
        .await;
}

fn format_time(seconds: i64) -> String {
    let seconds = seconds as f32;
    let hours = seconds / 3600.0;
    let minutes = (seconds % 3600.0) / 60.0;
    let seconds = seconds % 60.0;
    format!(
        "{:02}:{:02}:{:02}",
        hours as u32, minutes as u32, seconds as u32
    )
}

/// Loads a context and model, processes an audio file, and prints the resulting transcript to stdout.
fn get_transcript(episode: &Episode) -> Result<(), &'static str> {
    if std::path::Path::new(&episode.transcript_location).exists() {
        println!(
            "Transcript already exists for {}",
            episode.transcript_location
        );
        return Ok(());
    }
    // Load a context and model.
    let mut ctx = WhisperContext::new("ggml-base.bin").expect("failed to load model");

    // Create a params object for running the model.
    // Currently, only the Greedy sampling strategy is implemented, with BeamSearch as a WIP.
    // The number of past samples to consider defaults to 0.
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 0 });

    params.set_n_threads(4);
    params.set_language(Some("pt"));
    params.set_print_special(false);
    params.set_print_progress(true);
    params.set_print_realtime(true);
    params.set_print_timestamps(true);

    // Open the audio file.
    let mut reader = hound::WavReader::open(&episode.file_location).expect("failed to open file");
    #[allow(unused_variables)]
    let hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample,
        ..
    } = reader.spec();

    // Convert the audio to floating point samples.
    let mut audio = whisper_rs::convert_integer_to_float_audio(
        &reader
            .samples::<i16>()
            .map(|s| s.expect("invalid sample"))
            .collect::<Vec<_>>(),
    );

    // Convert audio to 16KHz mono f32 samples, as required by the model.
    // These utilities are provided for convenience, but can be replaced with custom conversion logic.
    // SIMD variants of these functions are also available on nightly Rust (see the docs).
    if channels == 2 {
        audio = whisper_rs::convert_stereo_to_mono_audio(&audio)?;
    } else if channels != 1 {
        panic!(">2 channels unsupported");
    }

    if sample_rate != 16000 {
        panic!("sample rate must be 16KHz");
    }

    // Run the model.
    ctx.full(params, &audio[..]).expect("failed to run model");

    // Create a file to write the transcript to.
    let mut file = File::create(&episode.transcript_location).expect("failed to create file");

    // Iterate through the segments of the transcript.
    let num_segments = ctx.full_n_segments();
    for i in 0..num_segments {
        let segment = match ctx.full_get_segment_text(i) {
            Ok(segment) => segment,
            Err(_) => {
                println!("Failed to get segment {}", i);
                continue;
            }
        };

        let start_timestamp = ctx.full_get_segment_t0(i);
        let end_timestamp = ctx.full_get_segment_t1(i);

        let start_timestamp_formatted = format_time(start_timestamp);
        let end_timestamp_formatted = format_time(end_timestamp);

        let formatted_string = format!(
            "[{} - {}]: {}\n",
            start_timestamp_formatted, end_timestamp_formatted, segment
        );

        file.write_all(formatted_string.as_bytes())
            .expect("failed to write to file");
    }
    Ok(())
}

pub async fn get_episodes() -> Vec<Episode> {
    let rss_feed = fetch_rss_feed().await.unwrap();
    let mut list_of_episode_urls = Vec::new();
    let mut list_of_episodes = Vec::new();

    for entry in rss_feed.entries.clone() {
        let media_content = entry.media.first().unwrap().content.first().unwrap();
        let url = media_content.url.clone().unwrap();
        let url_string = url.as_str();
        let title = entry.title.unwrap().content;
        let date = entry.published.unwrap();
        let thumbnail_url = entry
            .media
            .first()
            .unwrap()
            .thumbnails
            .first()
            .unwrap()
            .image
            .uri
            .clone();

        let episode = Episode {
            url: url_string.to_string(),
            title: title.to_string(),
            file_location: "".to_string(),
            transcript_location: "".to_string(),
            thumbnail_url: thumbnail_url.to_string(),
            date,
            number: 0,
        };

        list_of_episodes.push(episode);
        list_of_episode_urls.push(url_string.to_string());
    }

    list_of_episodes.sort_by_key(|episode| episode.date);

    for (i, episode) in list_of_episodes.iter_mut().enumerate() {
        episode.number = i as i32 + 1;
        let episode_file_name = format!("{:03}", episode.number) + ".wav";
        let episode_transcript_name = format!("{:03}", episode.number) + ".txt";

        episode.file_location = AUDIO_FILE_FOLDER.to_owned() + &episode_file_name;
        episode.transcript_location = TRANSCRIPT_FOLDER.to_owned() + &episode_transcript_name;
    }

    list_of_episodes
}

pub async fn get_s3_client() -> Result<S3Client, aws_sdk_s3::Error> {
    // Retrieve Cloudflare endpoint from an environment variable
    let cloudflare_endpoint = env::var("CLOUDFLARE_ENDPOINT")
        .expect("CLOUDFLARE_ENDPOINT environment variable is not set");

    let region_provider = RegionProviderChain::default_provider().or_else("us-east-1");
    let config = aws_config::from_env().region(region_provider).load().await;

    let s3_config = S3Builder::from(&config)
        .endpoint_url(cloudflare_endpoint)
        .build();

    let client = S3Client::from_conf(s3_config);

    Ok(client)
}

async fn get_transcribed_episodes(s3_client: &S3Client) -> Vec<i32> {
    s3_client
        .list_objects_v2()
        .bucket("governosombra")
        .prefix("transcripts")
        .send()
        .await
        .unwrap()
        .contents
        .unwrap_or_default()
        .into_iter()
        .filter_map(|obj| {
            let key = obj.key?;
            if !key.contains(".txt") {
                return None;
            }
            key.split('/').nth(1)?.split('.').next()?.parse().ok()
        })
        .collect()
}

pub async fn get_transcript_for(episode: &Episode) -> Result<String, Box<dyn std::error::Error>> {
    let _s3_client = get_s3_client().await.unwrap();
    let resp = _s3_client
        .get_object()
        .bucket("governosombra")
        .key(format!("transcripts/{:03}.txt", episode.number))
        .send()
        .await?;

    let data = resp.body.collect().await?;
    let string_data = String::from_utf8(data.to_vec())?;

    Ok(string_data)
}

async fn get_list_of_books_from(episode: &Episode) -> Result<Vec<Book>> {
    let transcript = get_transcript_for(episode).await.unwrap();
    let api_key = std::env::var("OPENAI_API_KEY")?;
    let client = reqwest::Client::new();

    const EXTRACT_BOOKS_PROMPT: &str = r#"
* You are a helpful assistant that excels at extracting book information from text. 
* The user will send you a transcript of a podcast episode.
* You must silently analyze the text and provide a list of books mentioned in the episode.
* Not all episode mention books, therefore you must return an empty list if no books are found.
* Return your response in a JSON that can be read with serde_json::from_str.
* DO NOT include any ```json ``` or ``` ``` in your response.
* You must use the following structure:
{
    "books": [
        {
            "title": "The Name of the Wind",
            "author": "Patrick Rothfuss"
        },
        {
            "title": "The Lord of the Rings",
            "author": "J.R.R. Tolkien"
        }
    ]
}
"#;

    let response = client
        .post("https://api.openai.com/v1/chat/completions")
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&json!({
            "model": "gpt-4o-mini",
            "messages": [
                {
                    "role": "system",
                    "content": EXTRACT_BOOKS_PROMPT
                },
                {
                    "role": "user",
                    "content": transcript
                }
            ]
        }))
        .send()
        .await?;

    let response_json: Value = response.json().await?;
    let response_string = response_json["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("Failed to get response")
        .to_string();
    let response_struct: Value = serde_json::from_str(&response_string)?;

    let books: Vec<Book> = response_struct["books"]
        .as_array()
        .unwrap()
        .iter()
        .map(|book| {
            let title = book["title"].as_str().unwrap().to_string();
            let author = book["author"].as_str().unwrap().to_string();
            let episode_number = episode.number;
            Book {
                title,
                author,
                episode_number,
            }
        })
        .collect();

    Ok(books)
}

async fn update_books_list(
    s3_client: &S3Client,
    new_books: &Vec<Book>,
    processed_episode_numbers: &[i32],
) -> Result<(), Box<dyn std::error::Error>> {
    // First, try to get existing database
    let mut database = match s3_client
        .get_object()
        .bucket("governosombra")
        .key(BOOKS_JSON_PATH)
        .send()
        .await
    {
        Ok(resp) => {
            let data = resp.body.collect().await?;
            serde_json::from_slice(&data.to_vec())?
        }
        Err(_) => BooksDatabase {
            processed_episodes: Vec::new(),
            books: Vec::new(),
        },
    };

    // Add new processed episodes
    for &episode_num in processed_episode_numbers {
        if !database.processed_episodes.contains(&episode_num) {
            database.processed_episodes.push(episode_num);
        }
    }

    // Add new books
    for new_book in new_books {
        if !database.books.iter().any(|book| {
            book.title == new_book.title
                && book.author == new_book.author
                && book.episode_number == new_book.episode_number
        }) {
            database.books.push(new_book.clone());
        }
    }

    // Upload updated database to S3
    let database_json = serde_json::to_string(&database)?;
    s3_client
        .put_object()
        .bucket("governosombra")
        .key(BOOKS_JSON_PATH)
        .body(database_json.into_bytes().into())
        .send()
        .await?;

    Ok(())
}

async fn get_episodes_with_books(s3_client: &S3Client) -> Vec<i32> {
    match s3_client
        .get_object()
        .bucket("governosombra")
        .key(BOOKS_JSON_PATH)
        .send()
        .await
    {
        Ok(resp) => {
            let data = resp.body.collect().await.unwrap();
            let database: BooksDatabase = serde_json::from_slice(&data.to_vec()).unwrap();
            database.processed_episodes
        }
        Err(_) => Vec::new(),
    }
}

pub async fn get_all_books(s3_client: &S3Client) -> Result<Vec<Book>, Box<dyn std::error::Error>> {
    match s3_client
        .get_object()
        .bucket("governosombra")
        .key(BOOKS_JSON_PATH)
        .send()
        .await
    {
        Ok(resp) => {
            let data = resp.body.collect().await?;
            let database: BooksDatabase = serde_json::from_slice(&data.to_vec())?;
            Ok(database.books)
        }
        Err(e) => Err(Box::new(e)),
    }
}

#[tokio::main]
pub async fn main() {
    let episodes = get_episodes().await;
    println!("Number of episodes in feed: {}", episodes.len());

    let _s3_client = get_s3_client().await.unwrap();
    let transcribed_episodes = get_transcribed_episodes(&_s3_client).await;
    // let processed_episodes = get_processed_episodes(&_s3_client).await;

    for episode in episodes.iter() {
        if transcribed_episodes.contains(&episode.number) {
            continue;
        }
        println!("Episode {} not processed yet", episode.number);

        download_episode(&episode).await;
        println!("Downloaded episode {}", episode.number);

        let _parsing = get_transcript(episode);
        println!("Got transcript in {}", episode.transcript_location);

        _s3_client
            .put_object()
            .bucket("governosombra")
            .key(format!("transcripts/{:03}.txt", episode.number))
            .body(std::fs::read(&episode.transcript_location).unwrap().into())
            .send()
            .await
            .expect("failed to upload transcript");
        println!("Uploaded transcript for episode {}", episode.number);

        std::fs::remove_file(&episode.file_location).expect("failed to delete file");
        println!("Deleted file {}", episode.file_location);

        std::fs::remove_file(&episode.transcript_location).expect("failed to delete file");
        println!("Deleted file {}", episode.transcript_location);
    }

    let episodes_with_books = get_episodes_with_books(&_s3_client).await;
    println!(
        "Total episodes parsed for books: {}",
        episodes_with_books.len()
    );

    let mut processed_episode_numbers = Vec::new();
    let mut all_books = Vec::new();

    let episodes_to_process = {
        let mut unprocessed = episodes
            .iter()
            .filter(|ep| !episodes_with_books.contains(&ep.number))
            .collect::<Vec<_>>();
        unprocessed.sort_by_key(|ep| -ep.number); // Negative to sort in descending order
        unprocessed.into_iter().take(10)
    };

    for episode in episodes_to_process {
        let books = get_list_of_books_from(&episode).await.unwrap();
        all_books.extend(books.clone());
        processed_episode_numbers.push(episode.number);
        println!("Books found in episode {}: {:?}", episode.number, books);
    }

    // Update books list in S3
    if let Err(e) = update_books_list(&_s3_client, &all_books, &processed_episode_numbers).await {
        eprintln!("Failed to update books list: {}", e);
    }
    println!("Updated books database in S3");

    let total_books = get_all_books(&_s3_client).await.unwrap();
    println!("Total books in database: {}", total_books.len());
}
