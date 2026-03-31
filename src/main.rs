use std::{sync::Mutex, thread, time::Duration};

use actix_web::{Responder, Result, get, web};
use chrono::{DateTime, Utc};
use rand::Rng;
use scele_frontapi::get_frontpage;
use scraper::{Html, Selector};
use serde::Serialize;

#[derive(Serialize, Clone)]
struct AnnouncementResponse {
    pub id: String,
    pub title: String,
    pub author: String,
    pub date_time: DateTime<Utc>,
}

struct CachedResponse {
    pub announcements: Vec<AnnouncementResponse>,
    pub cached_at: chrono::DateTime<Utc>,
}

struct ServerState {
    pub request_count: Mutex<u8>,
    pub cache: Mutex<Option<CachedResponse>>,
}

fn parse_frontpage(page: Html) -> Vec<AnnouncementResponse> {
    let selector = Selector::parse("article").unwrap();
    let elements_iterator = page.select(&selector);
    let mut announcements = Vec::<AnnouncementResponse>::new();

    for element in elements_iterator {
        let id = String::from(element.attr("id").unwrap());
        let title = element
            .select(&Selector::parse("h3").unwrap())
            .next()
            .map(|e| e.text().collect::<String>())
            .unwrap();
        let author = element
            .select(&Selector::parse("a").unwrap())
            .next()
            .map(|e| e.text().collect::<String>())
            .unwrap();
        
        let announcement = AnnouncementResponse {
            id,
            title,
            author,
            date_time: Utc::now(), // TODO: Parse the time value from the HTML
        };

        announcements.push(announcement);
    }

    return announcements;
}

#[get("/announcements")]
async fn get_all_announcements(data: web::Data<ServerState>) -> Result<impl Responder> {
    // --- Cache: only fetch from SCELE on a cache miss ---
    let announcements = {
        let mut cache = data.cache.lock().unwrap();

        if let Some(ref cached) = *cache {
            // Cache hit: return stored announcements without hitting SCELE
            cached.announcements.clone()
        } else {
            // Cache miss: fetch, parse, and store in cache
            let page = get_frontpage("https://scele.cs.ui.ac.id").unwrap();
            let fetched = parse_frontpage(page);
            *cache = Some(CachedResponse {
                announcements: fetched.clone(),
                cached_at: Utc::now(),
            });
            fetched
        }
    }; // cache lock released here

    // --- Fixed critical section: Mutex guarantees mutual exclusion ---
    {
        let mut count = data.request_count.lock().unwrap();
        let val = *count;
        let delay_ms = rand::thread_rng().gen_range(0..1000_u64); // simulates interleaving — now safe
        thread::sleep(Duration::from_millis(delay_ms));
        *count = val + 1;
        println!("Request count: {}", *count);
    } // request_count lock released here

    Ok(web::Json(announcements))
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let state = web::Data::new(ServerState {
        request_count: Mutex::new(0),
        cache: Mutex::new(None),
    });

    use actix_web::{App, HttpServer};

    HttpServer::new(move || App::new()
        .app_data(state.clone())
        .service(get_all_announcements))
        .bind(("127.0.0.1", 8080))?
        .run()
        .await
}
