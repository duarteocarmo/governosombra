use actix_web::middleware::Logger;
use actix_web::{get, web, App, HttpResponse, HttpServer, Responder};
mod process;
use crate::process::get_all_books;
use cronjob::CronJob;
use env_logger::Env;
use log::info;
use serde::{Deserialize, Serialize};
use tera::{Context, Tera};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Snippet {
    timestamp: String,
    text: String,
}

#[get("/")]
async fn hello(templ: web::Data<Tera>) -> impl Responder {
    let episodes = process::get_episodes()
        .await
        .into_iter()
        .rev()
        .collect::<Vec<_>>();

    let mut context = Context::new();
    context.insert("episodes", &episodes);
    let s = templ.render("index.html", &context).unwrap();

    HttpResponse::Ok().body(s)
}

#[get("/episodes/{episode_id}")]
async fn episode_pages(path: web::Path<i32>, templ: web::Data<Tera>) -> impl Responder {
    let episode_id = path.into_inner();
    let episodes = process::get_episodes().await;
    let episode = episodes.iter().find(|e| e.number == episode_id).unwrap();
    let transcript = process::get_transcript_for(&episode).await.unwrap();

    let snippets: Vec<Snippet> = transcript
        .lines()
        .map(|line| {
            let parts: Vec<&str> = line.splitn(2, ": ").collect();
            Snippet {
                timestamp: parts[0].to_string(),
                text: parts[1].to_string(),
            }
        })
        .collect();

    let mut context = Context::new();
    context.insert("episode", episode);
    context.insert("snippets", &snippets);
    let s = templ.render("episode.html", &context).unwrap();

    HttpResponse::Ok().body(s)
}

#[get("/livros")]
async fn books(templ: web::Data<Tera>) -> impl Responder {
    let s3_client = process::get_s3_client().await.unwrap();
    let mut books = get_all_books(&s3_client).await.unwrap();

    // Sort books by episode number in descending order (most recent first)
    books.sort_by(|a, b| b.episode_number.cmp(&a.episode_number));

    let mut context = Context::new();
    context.insert("books", &books);
    let s = templ.render("books.html", &context).unwrap();

    HttpResponse::Ok().body(s)
}

fn on_cron(name: &str) {
    println!("{}: Updating episodes!", name);
    process::main();
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Log requests
    env_logger::init_from_env(Env::default().default_filter_or("info"));

    let debug = std::env::var("DEBUG")
        .unwrap_or("false".to_string())
        .to_lowercase();

    if debug != "true" {
        let sentry_dsn = std::env::var("SENTRY_DSN").unwrap();
        let _guard = sentry::init((
            sentry_dsn,
            sentry::ClientOptions {
                release: sentry::release_name!(),
                ..Default::default()
            },
        ));
        info!("Sentry initialized");
    }

    std::env::set_var("RUST_BACKTRACE", "1");

    // Updater
    let mut cron = CronJob::new("Test Cron", on_cron);
    cron.minutes("40");
    cron.seconds("0");
    cron.offset(0);
    CronJob::start_job_threaded(cron);

    HttpServer::new(|| {
        let tera = Tera::new(concat!(env!("CARGO_MANIFEST_DIR"), "/templates/**/*")).unwrap();

        App::new()
            .wrap(Logger::default())
            .wrap(Logger::new("%a %{User-Agent}i"))
            .wrap(sentry_actix::Sentry::new())
            .app_data(web::Data::new(tera))
            .service(hello)
            .service(episode_pages)
            .service(books)
    })
    .bind(("0.0.0.0", 8080))?
    .run()
    .await
}
