use std::{fs::File, io::Read, io::Write, path::Path};

use actix::{Actor, StreamHandler};
use actix_web::{
    get, put,
    web::{self, BytesMut},
    App, Error, HttpRequest, HttpResponse, HttpServer, Responder,
};
use actix_web_actors::ws;
use config::Config;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// constants
const MAX_SIZE: usize = 512 * 1024; // max payload size is 512k

// Define HTTP actor
struct GameWs;
impl Actor for GameWs {
    type Context = ws::WebsocketContext<Self>;
}

// Define the leaderboard response object
#[derive(Serialize)]
struct LeaderboardSection {
    entries: Vec<LeaderboardEntry>,
    page: i32,
    #[serde(rename = "pageSize")]
    page_size: i32,
    #[serde(rename = "totalPages")]
    total_pages: i32,
}

// Define the leaderboard entry object
#[derive(Deserialize, Serialize)]
struct LeaderboardEntry {
    #[serde(rename = "playerId")]
    player_id: i32,
    #[serde(rename = "levelId")]
    level_id: i32,
    #[serde(rename = "ghostId")]
    ghost_id: Uuid,
    time: f32,
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Load config
    let settings = Config::builder()
        .set_default("server.listen_address", "127.0.0.1")
        .unwrap()
        .set_default("server.port", 8080)
        .unwrap()
        .set_default("storage.base", "data")
        .unwrap()
        .set_default("storage.ghost_dir", "ghosts")
        .unwrap()
        .add_source(config::File::with_name("config"))
        .build()
        .unwrap();
    let settings_to_clone = settings.clone();
    // Default log level to info if not set
    if let Err(_) = std::env::var("RUST_LOG") {
        std::env::set_var("RUST_LOG", "info");
    }

    // Initialize the logger
    pretty_env_logger::init();
    log::info!("Starting server...");

    // Get ghost directory from config
    let ghost_dir = get_ghost_dir(&settings);
    // Create the ghosts directory if it doesn't exist
    if !std::path::Path::new(&ghost_dir).exists() {
        std::fs::create_dir_all(ghost_dir)?;
    }
    // Start the server
    HttpServer::new(move || {
        App::new()
            .service(web::redirect("/", "https://catpowered.net"))
            .service(submit_ghost)
            .service(get_ghost)
            .service(get_leaderboard_section)
            .route("/ws/", web::get().to(ws_upgrader))
            .app_data(settings_to_clone.clone())
    })
    .bind((
        settings.get_string("server.listen_address").unwrap(),
        settings.get_int("server.port").unwrap() as u16,
    ))?
    .run()
    .await
}

/// Handler for incoming ws::Message messages
impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for GameWs {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        match msg {
            Ok(ws::Message::Ping(msg)) => ctx.pong(&msg),
            Ok(ws::Message::Text(text)) => ctx.text(text),
            Ok(ws::Message::Binary(bin)) => ctx.binary(bin),
            _ => (),
        }
    }
}

// Sets up websocket connections
async fn ws_upgrader(req: HttpRequest, stream: web::Payload) -> Result<HttpResponse, Error> {
    let resp = ws::start(GameWs {}, &req, stream);
    println!("{:?}", req);
    resp
}

// Handles the submission of a ghost binary
#[put("/api/v1/submit-ghost")]
async fn submit_ghost(req: HttpRequest, mut payload: web::Payload) -> Result<HttpResponse, Error> {
    let mut ghost_data = BytesMut::new();
    // Load the payload into memory
    while let Some(chunk) = payload.next().await {
        let chunk = chunk?;
        // Limit max size of in-memory payload
        if (ghost_data.len() + chunk.len()) > MAX_SIZE {
            return Err(actix_web::error::ErrorPayloadTooLarge("Payload Too Large"));
        }
        ghost_data.extend_from_slice(&chunk);
    }
    // ghost_data is loaded, now we can save to file

    // Get the ghost directory from the config
    let config = req.app_data::<Config>().unwrap();
    let ghost_dir = get_ghost_dir(&config);
    let data_dir = config.get_string("storage.base").unwrap();

    let id = Uuid::new_v4(); // Generate a random UUID

    let mut f = File::create(format!("{ghost_dir}/{id}"))?; // Attempt to create the file
    if f.write_all(&ghost_data[..]).is_err() {
        return Err(actix_web::error::ErrorInternalServerError(
            "couldn't write to ghost file",
        )); // Return error if file can't be written to
    }

    // Get header values
    let player_id = req
        .headers()
        .get("X-Player-Id")
        .unwrap()
        .to_str()
        .unwrap()
        .parse::<i32>();
    let level_id = req
        .headers()
        .get("X-Level-Id")
        .unwrap()
        .to_str()
        .unwrap()
        .parse::<i32>();
    let replay_length = req
        .headers()
        .get("X-Replay-Length")
        .unwrap()
        .to_str()
        .unwrap()
        .parse::<f32>();
    // Check they are valid integers
    if player_id.is_err() || level_id.is_err() || replay_length.is_err() {
        return Err(actix_web::error::ErrorBadRequest("Invalid headers"));
    }
    // Save the ghost values to the leaderboard
    let leaderboard_entry = LeaderboardEntry {
        player_id: player_id.unwrap(),
        level_id: level_id.unwrap(),
        ghost_id: id,
        time: f32::trunc(replay_length.unwrap() / 25.0 * 100.0) / 100.0,
    };
    // Get the current leaderboard from the file leaderboard.json and add the new entry
    let mut leaderboard = get_leaderboard(&data_dir);
    leaderboard.push(leaderboard_entry);
    // Sort the leaderboard by time
    leaderboard.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap());
    // Save the leaderboard back to leaderboard.json
    let mut f = File::create(format!("{data_dir}/leaderboard.json"))?;
    if f.write_all(
        serde_json::to_string_pretty(&leaderboard)
            .unwrap()
            .as_bytes(),
    )
    .is_err()
    {
        return Err(actix_web::error::ErrorInternalServerError(
            "couldn't write to leaderboard file",
        ));
    } // Return error if file can't be written to

    Ok(HttpResponse::Ok().body(format!("{id}")))
}

// Returns leaderboard section as JSON
#[get("/api/v1/leaderboard/{page}/{page_size}")]
async fn get_leaderboard_section(
    req: HttpRequest,
    params: web::Path<(i32, i32)>,
) -> impl Responder {
    let (page, page_size) = params.into_inner();

    let mut leaderboard = get_leaderboard(
        req.app_data::<Config>()
            .unwrap()
            .get_string("storage.base")
            .unwrap()
            .as_str(),
    );

    let leaderboard_section = if leaderboard.len() > (page * page_size) as usize {
        LeaderboardSection {
            page,
            page_size,
            total_pages: (leaderboard.len() as f32 / page_size as f32).ceil() as i32,
            entries: leaderboard
                .drain(((page - 1) * page_size) as usize..((page) * page_size) as usize)
                .collect(),
        }
    } else if leaderboard.len() - ((page - 1) * page_size) as usize > 0 {
        LeaderboardSection {
            page,
            page_size: leaderboard.len() as i32 - ((page - 1) * page_size),
            total_pages: (leaderboard.len() as f32 / page_size as f32).ceil() as i32,
            entries: leaderboard
                .drain(((page - 1) * page_size) as usize..leaderboard.len())
                .collect(),
        }
    } else {
        LeaderboardSection {
            page: 1,
            page_size: leaderboard.len() as i32,
            total_pages: 1,
            entries: leaderboard,
        }
    };

    HttpResponse::Ok().json(web::Json(leaderboard_section))
}

// Handles the retrieval of a ghost binary
#[get("/api/v1/get-ghost/{id}")]
async fn get_ghost(req: HttpRequest, id: web::Path<String>) -> impl Responder {
    // Check if the UUID is valid
    let uuid = Uuid::parse_str(&id);
    if uuid.is_err() {
        return HttpResponse::BadRequest().body("Invalid UUID");
    }
    // Get the ghost directory from the config
    let ghost_dir = get_ghost_dir(&req.app_data::<Config>().unwrap());
    // Check if the file exists
    let path = format!("{ghost_dir}/{id}", id = id);
    if !std::path::Path::new(&path).exists() {
        return HttpResponse::NotFound().body("Ghost not found");
    }
    // Return the file
    let ghost_data = std::fs::read(path);
    if ghost_data.is_err() {
        return HttpResponse::InternalServerError().body("Couldn't read data");
    }
    HttpResponse::Ok().body(ghost_data.unwrap())
}

fn get_ghost_dir(config: &Config) -> String {
    config.get_string("storage.base").unwrap()
        + "/"
        + &config.get_string("storage.ghost_dir").unwrap()
}

fn get_leaderboard(data_dir: &str) -> Vec<LeaderboardEntry> {
    if !Path::new(&format!("{data_dir}/leaderboard.json")).exists() {
        return Vec::new();
    }
    let mut leaderboard_file = File::open(format!("{data_dir}/leaderboard.json")).unwrap();
    let mut leaderboard_data = String::new();
    leaderboard_file
        .read_to_string(&mut leaderboard_data)
        .unwrap();

    serde_json::from_str(&leaderboard_data).unwrap()
}
