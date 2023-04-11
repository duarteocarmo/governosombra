use feed_rs::{parser, model::Feed};
use hound;
use std::io::Cursor;
use std::fs::File;
use std::io::Write;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext};
use futures::{stream, StreamExt}; 
use reqwest::Client; 
use chrono::{DateTime, Utc};
use tokio; 

const CONCURRENT_REQUESTS: usize = 30;
const RSS_FEED_LOCATION: &str = "https://www.omnycontent.com/d/playlist/8c0a4104-a688-4e57-91fd-ad7b00d5dddd/c2325e96-d6ad-4206-b72b-ad8e00e5f4fe/bbc8a8c5-8da7-46ef-843f-ad8e00e5f515/podcast.rss";
const DOWNLOAD_LOCATION: &str = "episodes/";

#[derive (Debug, Clone)]
struct Episode {
    url: String,
    title: String,
    location: String,
    number: i32,
    date: DateTime<Utc>,
}

#[tokio::main]
async fn fetch_rss_feed() -> Result<Feed, Box<dyn std::error::Error>> {
    let resp = reqwest::get(RSS_FEED_LOCATION).await?.text().await?;
    let feed = parser::parse(resp.as_bytes()).unwrap();

    return Ok(feed);
}


#[tokio::main]
async fn download_episodes_from(list_of_episodes: Vec<Episode>)  {
    let client = Client::new();

    let episodes_to_download = list_of_episodes
        .iter()
        .filter(|episode| !std::path::Path::new(&episode.location).exists())
        .cloned()
        .collect::<Vec<Episode>>();

    println!("{} episodes to download from {} total.", episodes_to_download.len(), list_of_episodes.len());

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
            match result  {
                Ok((episode, bytes)) => {

                    let mut file = std::fs::File::create(&episode.location).unwrap();
                    let mut content =  Cursor::new(bytes);
                    std::io::copy(&mut content, &mut file).unwrap();
                    println!("Downloaded {}", episode.location);

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
fn parse_wav_file(file_path: &str) -> Result<(), &'static str> {
    // Load a context and model.
    let mut ctx = WhisperContext::new("ggml-base.bin").expect("failed to load model");

    // Create a params object for running the model.
    // Currently, only the Greedy sampling strategy is implemented, with BeamSearch as a WIP.
    // The number of past samples to consider defaults to 0.
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 0 });

    params.set_n_threads(3);
    params.set_language(Some("pt"));
    params.set_print_special(false);
    params.set_print_progress(true);
    params.set_print_realtime(true);
    params.set_print_timestamps(true);

    // Open the audio file.
    let mut reader = hound::WavReader::open(file_path).expect("failed to open file");
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
    let mut file = File::create("transcript.txt").expect("failed to create file");

    // Iterate through the segments of the transcript.
    let num_segments = ctx.full_n_segments();
    for i in 0..num_segments {
        // Get the transcribed text and timestamps for the current segment.
        let segment = ctx.full_get_segment_text(i).expect("failed to get segment");
        let start_timestamp = ctx.full_get_segment_t0(i);
        let end_timestamp = ctx.full_get_segment_t1(i);

        let start_timestamp_formatted = format_time(start_timestamp);
        let end_timestamp_formatted = format_time(end_timestamp);

        let formatted_string = format!(
            "[{} - {}]: {}\n",
            start_timestamp_formatted, end_timestamp_formatted, segment
        );

        println!("{}", formatted_string.replace("\n", " "));

        file.write_all(formatted_string.as_bytes())
            .expect("failed to write to file");
    }
    Ok(())
}


fn main() -> Result<(), &'static str> {
    // let _parsing = parse_wav_file("sample.wav");
    let rss_feed = fetch_rss_feed().unwrap();

    
    let mut list_of_episode_urls = Vec::new();
    let mut list_of_episodes = Vec::new();

    for entry in rss_feed.entries.clone() {
        let media_content = entry.media.first().unwrap().content.first().unwrap();
        let url = media_content.url.clone().unwrap();
        let url_string = url.as_str();
        let title = entry.title.unwrap().content;
        let date = entry.published.unwrap();


        let episode = Episode {
            url: url_string.to_string(),
            title: title.to_string(),
            location: "".to_string(),
            date,
            number: 0,
        };
    
        list_of_episodes.push(episode);
        list_of_episode_urls.push(url_string.to_string());
    }

    list_of_episodes.sort_by_key(|episode| episode.date);

    for (i, episode) in list_of_episodes.iter_mut().enumerate() {
        episode.number = i as i32 + 1;
        let episode_file_name = format!("{:03}", episode.number) + ".mp3";
        episode.location = DOWNLOAD_LOCATION.to_owned() + &episode_file_name;
    }

    let total_episodes = list_of_episodes.len();
    println!("Total episodes: {}", total_episodes);

    let first_episode = list_of_episodes.first();
    println!("First episode: {:?}", first_episode);

    download_episodes_from(list_of_episodes);

    return Ok(());
}
