use actix_web::middleware::Logger;
use actix_web::{get, web, App, HttpResponse, HttpServer, Responder};
mod process;
use cronjob::CronJob;
use env_logger::Env;
use std::fs::File;
use std::io::Read;
use tera::{Context, Tera};

#[get("/")]
async fn hello(templ: web::Data<Tera>) -> impl Responder {
    let episodes = process::get_episodes().await;
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

    let mut file = File::open(&episode.transcript_location).expect("Failed to open file");
    let mut transcript = String::new();
    file.read_to_string(&mut transcript)
        .expect("Failed to read file");

    let mut context = Context::new();
    context.insert("episode", episode);
    context.insert("transcript", &transcript);
    let s = templ.render("episode.html", &context).unwrap();

    HttpResponse::Ok().body(s)
}

fn on_cron(name: &str) {
    println!("{}: Updating episodes!", name);
    process::main();
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Updater
    let mut cron = CronJob::new("Test Cron", on_cron);
    cron.minutes("11");
    cron.seconds("30");
    cron.offset(0);
    CronJob::start_job_threaded(cron);

    // Log requests
    env_logger::init_from_env(Env::default().default_filter_or("info"));

    HttpServer::new(|| {
        let tera = Tera::new(concat!(env!("CARGO_MANIFEST_DIR"), "/templates/**/*")).unwrap();

        App::new()
            .wrap(Logger::default())
            .wrap(Logger::new("%a %{User-Agent}i"))
            .app_data(web::Data::new(tera))
            .service(hello)
            .service(episode_pages)
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await
}
