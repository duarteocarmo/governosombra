use actix_files::Files;
use actix_web::middleware::Logger;
use actix_web::{get, post, web, App, HttpRequest, HttpResponse, HttpServer, Responder};
use aws_sdk_s3::Client as S3Client;
use cronjob::CronJob;
use env_logger::Env;
use futures::FutureExt;
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use std::env;
use std::io;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use tera::{Context, Tera};
use tokio::sync::Semaphore;

mod process;
use crate::process::get_all_books;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Snippet {
    timestamp: String,
    text: String,
}

struct AppState {
    s3_client: S3Client,
    processing: Arc<Semaphore>,
}

#[get("/healthz")]
async fn healthz() -> impl Responder {
    HttpResponse::Ok().body("ok")
}

#[get("/")]
async fn hello(templ: web::Data<Tera>) -> impl Responder {
    let episodes = match process::get_episodes().await {
        Ok(episodes) => episodes.into_iter().rev().collect::<Vec<_>>(),
        Err(error) => {
            error!("Could not load episodes: {error:#}");
            return HttpResponse::ServiceUnavailable().body("Episodes are temporarily unavailable");
        }
    };

    let mut context = Context::new();
    context.insert("episodes", &episodes);
    match templ.render("index.html", &context) {
        Ok(page) => HttpResponse::Ok().body(page),
        Err(error) => {
            error!("Could not render index: {error}");
            HttpResponse::InternalServerError().finish()
        }
    }
}

#[get("/episodes/{episode_id}")]
async fn episode_pages(
    path: web::Path<i32>,
    templ: web::Data<Tera>,
    state: web::Data<AppState>,
) -> impl Responder {
    let episode_id = path.into_inner();
    let episodes = match process::get_episodes().await {
        Ok(episodes) => episodes,
        Err(error) => {
            error!("Could not load episodes: {error:#}");
            return HttpResponse::ServiceUnavailable().finish();
        }
    };
    let Some(episode) = episodes.iter().find(|episode| episode.number == episode_id) else {
        return HttpResponse::NotFound().body("Episode not found");
    };

    let transcript = match process::get_transcript_for(&state.s3_client, episode).await {
        Ok(transcript) => transcript,
        Err(error) => {
            info!("Transcript for episode {episode_id} is unavailable: {error}");
            return HttpResponse::Ok().body("Volta mais tarde amigo(a).");
        }
    };

    let snippets = transcript
        .lines()
        .filter_map(|line| {
            let (timestamp, text) = line.split_once(": ")?;
            Some(Snippet {
                timestamp: timestamp.to_owned(),
                text: text.to_owned(),
            })
        })
        .collect::<Vec<_>>();

    let mut context = Context::new();
    context.insert("episode", episode);
    context.insert("snippets", &snippets);
    match templ.render("episode.html", &context) {
        Ok(page) => HttpResponse::Ok().body(page),
        Err(error) => {
            error!("Could not render episode {episode_id}: {error}");
            HttpResponse::InternalServerError().finish()
        }
    }
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn authorized_to_process(request: &HttpRequest, expected_token: &str) -> bool {
    let Some(header) = request.headers().get("Authorization") else {
        return false;
    };
    let Ok(header) = header.to_str() else {
        return false;
    };
    let Some(token) = header.strip_prefix("Bearer ") else {
        return false;
    };
    constant_time_equal(token.as_bytes(), expected_token.as_bytes())
}

fn try_start_processing(state: web::Data<AppState>, source: &'static str) -> bool {
    let Ok(permit) = state.processing.clone().try_acquire_owned() else {
        info!("Ignoring {source} processing request because a run is already active");
        return false;
    };
    let s3_client = state.s3_client.clone();

    tokio::spawn(async move {
        info!("Starting {source} processing run");
        let result = AssertUnwindSafe(process::run(&s3_client))
            .catch_unwind()
            .await;
        match result {
            Ok(Ok(())) => info!("Finished {source} processing run"),
            Ok(Err(error)) => error!("{source} processing run failed: {error:#}"),
            Err(_) => error!("{source} processing run panicked; future runs will continue"),
        }
        drop(permit);
    });
    true
}

#[post("/process")]
async fn trigger_process(request: HttpRequest, state: web::Data<AppState>) -> impl Responder {
    let Some(expected_token) = env::var("PROCESS_TOKEN")
        .ok()
        .filter(|token| !token.is_empty())
    else {
        return HttpResponse::NotFound().finish();
    };
    if !authorized_to_process(&request, &expected_token) {
        return HttpResponse::Unauthorized().finish();
    }
    if try_start_processing(state, "manual") {
        HttpResponse::Accepted().body("Processing started")
    } else {
        HttpResponse::Conflict().body("Processing is already running")
    }
}

#[get("/livros")]
async fn books(templ: web::Data<Tera>, state: web::Data<AppState>) -> impl Responder {
    let mut books = match get_all_books(&state.s3_client).await {
        Ok(books) => books,
        Err(error) => {
            error!("Could not load books: {error:#}");
            return HttpResponse::ServiceUnavailable().finish();
        }
    };
    books.sort_by_key(|book| std::cmp::Reverse(book.episode_number));

    let mut context = Context::new();
    context.insert("books", &books);
    match templ.render("books.html", &context) {
        Ok(page) => HttpResponse::Ok().body(page),
        Err(error) => {
            error!("Could not render books: {error}");
            HttpResponse::InternalServerError().finish()
        }
    }
}

#[actix_web::main]
async fn main() -> io::Result<()> {
    env_logger::init_from_env(Env::default().default_filter_or("info"));
    std::env::set_var("RUST_BACKTRACE", "1");

    let _sentry_guard = env::var("SENTRY_DSN")
        .ok()
        .filter(|dsn| !dsn.is_empty())
        .map(|dsn| {
            info!("Sentry initialized");
            sentry::init((
                dsn,
                sentry::ClientOptions {
                    release: sentry::release_name!(),
                    ..Default::default()
                },
            ))
        });
    if _sentry_guard.is_none() {
        warn!("SENTRY_DSN is not set; error reporting is disabled");
    }

    let s3_client = process::get_s3_client()
        .await
        .map_err(|error| io::Error::other(format!("initialize S3 client: {error:#}")))?;
    let state = web::Data::new(AppState {
        s3_client,
        processing: Arc::new(Semaphore::new(1)),
    });
    let templates = web::Data::new(
        Tera::new(concat!(env!("CARGO_MANIFEST_DIR"), "/templates/**/*"))
            .map_err(|error| io::Error::other(format!("load templates: {error}")))?,
    );

    let runtime = tokio::runtime::Handle::current();
    let scheduled_state = state.clone();
    let mut cron = CronJob::new("Daily Processing", move |_: &str| {
        let state = scheduled_state.clone();
        runtime.spawn(async move {
            try_start_processing(state, "scheduled");
        });
    });
    cron.hours("8");
    cron.minutes("0");
    cron.seconds("0");
    cron.offset(60 * 60);
    CronJob::start_job_threaded(cron);

    HttpServer::new(move || {
        App::new()
            .wrap(Logger::default())
            .wrap(sentry_actix::Sentry::new())
            .app_data(state.clone())
            .app_data(templates.clone())
            .service(healthz)
            .service(
                Files::new("/static", concat!(env!("CARGO_MANIFEST_DIR"), "/static"))
                    .show_files_listing(),
            )
            .service(hello)
            .service(episode_pages)
            .service(trigger_process)
            .service(books)
    })
    .bind(("0.0.0.0", 8080))?
    .run()
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[actix_web::test]
    async fn health_check_has_no_dependencies() {
        let app = actix_web::test::init_service(App::new().service(healthz)).await;
        let request = actix_web::test::TestRequest::get()
            .uri("/healthz")
            .to_request();
        let response = actix_web::test::call_service(&app, request).await;
        assert!(response.status().is_success());
    }

    #[test]
    fn token_comparison_checks_content_and_length() {
        assert!(constant_time_equal(b"correct", b"correct"));
        assert!(!constant_time_equal(b"correct", b"wrong__"));
        assert!(!constant_time_equal(b"correct", b"short"));
    }
}
