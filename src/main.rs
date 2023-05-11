use std::{
    fs::{File, OpenOptions},
    io::Read,
    io::{BufReader, Write},
    path::Path,
};

use actix::{Actor, StreamHandler};
use actix_web::{
    error::Error,
    get, put,
    web::{self, BytesMut},
    App, HttpRequest, HttpResponse, HttpServer, Responder,
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
    usernames: Vec<String>,
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

#[derive(Deserialize, Serialize)]
struct PlayerData {
    id: i32,
    secret: String,
    username: String,
}

#[derive(Deserialize, Serialize)]
struct AllPlayerData {
    players: Vec<PlayerData>,
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
        .set_default("storage.player_filename", "players.json")
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
            .service(update_account)
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
    let player_secret = req
        .headers()
        .get("X-Player-Secret")
        .unwrap()
        .to_str()
        .unwrap()
        .parse::<Uuid>();
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
    if player_id.is_err() || player_secret.is_err() || level_id.is_err() || replay_length.is_err() {
        return Err(actix_web::error::ErrorBadRequest("Invalid headers"));
    }
    // Check the player secret is valid
    let player_data = read_player_data(req.app_data::<Config>().unwrap()).unwrap();
    if player_data
        .players
        .iter()
        .find(|&p| {
            &p.id == player_id.as_ref().unwrap()
                && p.secret == player_secret.as_ref().unwrap().to_string()
        })
        .is_none()
    {
        return Err(actix_web::error::ErrorUnauthorized("Invalid player secret"));
    }
    // Save the ghost values to the leaderboard
    let leaderboard_entry = LeaderboardEntry {
        player_id: player_id.unwrap(),
        level_id: level_id.unwrap(),
        ghost_id: id,
        time: f32::trunc(replay_length.unwrap() / 50.0 * 100.0) / 100.0,
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

    let mut leaderboard_section = if leaderboard.len() > (page * page_size) as usize {
        LeaderboardSection {
            page,
            page_size,
            total_pages: (leaderboard.len() as f32 / page_size as f32).ceil() as i32,
            entries: leaderboard
                .drain(((page - 1) * page_size) as usize..((page) * page_size) as usize)
                .collect(),
            usernames: Vec::new(),
        }
    } else if leaderboard.len() - ((page - 1) * page_size) as usize > 0 {
        LeaderboardSection {
            page,
            page_size: leaderboard.len() as i32 - ((page - 1) * page_size),
            total_pages: (leaderboard.len() as f32 / page_size as f32).ceil() as i32,
            entries: leaderboard
                .drain(((page - 1) * page_size) as usize..leaderboard.len())
                .collect(),
            usernames: Vec::new(),
        }
    } else {
        LeaderboardSection {
            page: 1,
            page_size: leaderboard.len() as i32,
            total_pages: 1,
            entries: leaderboard,
            usernames: Vec::new(),
        }
    };

    // Get player names in order using read_player_data
    let mut player_ids = Vec::new();
    for entry in &leaderboard_section.entries {
        player_ids.push(entry.player_id);
    }
    let player_data = read_player_data(req.app_data::<Config>().unwrap()).unwrap();
    for id in player_ids {
        for player in &player_data.players {
            if player.id == id {
                leaderboard_section.usernames.push(player.username.clone());
                break;
            }
        }
    }

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

// Update the username of an existing player, or create the player if it doesn't exist, storing data of all users in one json file
#[put("/api/v1/update-account")]
async fn update_account(req: HttpRequest, body: web::Bytes) -> impl Responder {
    // Get the player id from the header
    let player_id = req
        .headers()
        .get("X-Player-Id")
        .unwrap()
        .to_str()
        .unwrap()
        .parse::<i32>();
    // Check if the player id is valid
    if player_id.is_err() {
        return HttpResponse::BadRequest().body("Invalid player id");
    }
    let player_id = player_id.unwrap();

    // Get the player secret from the header
    let player_secret = req
        .headers()
        .get("X-Player-Secret")
        .unwrap()
        .to_str()
        .unwrap()
        .parse::<Uuid>();
    // Check if the player secret is valid
    if player_secret.is_err() {
        return HttpResponse::Unauthorized().body("Invalid player secret");
    }
    let player_secret = player_secret.unwrap();

    // Get the player data from the file
    let mut all_player_data = read_player_data(req.app_data::<Config>().unwrap())
        .unwrap_or_else(|_| AllPlayerData { players: vec![] });

    // Find the player with the given id or create a new one if not found
    let mut player_data = match all_player_data
        .players
        .iter_mut()
        .find(|p| p.id == player_id)
    {
        Some(player_data) => player_data,
        None => {
            let new_player_data = PlayerData {
                id: player_id,
                secret: player_secret.to_string(),
                username: "".to_owned(),
            };
            all_player_data.players.push(new_player_data);
            all_player_data.players.last_mut().unwrap()
        }
    };

    // Check if the player secret is correct
    if player_data.secret != player_secret.to_string() {
        return HttpResponse::Unauthorized().body("Invalid player secret");
    }

    // Get the username from the body
    let username = String::from_utf8(body.to_vec());
    // Check if the username is valid
    if username.is_err() {
        return HttpResponse::BadRequest().body("Invalid username");
    }
    // Update the username
    player_data.username = username.unwrap().trim().to_string();

    // Save the player data back to the file
    let player_data_str = serde_json::to_string_pretty(&all_player_data).unwrap();
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(get_player_file(req.app_data::<Config>().unwrap()))
        .unwrap();
    writeln!(file, "{}", player_data_str).unwrap();

    HttpResponse::Ok().body("Username updated")
}

// Read the player data from the file
fn read_player_data(config: &Config) -> std::io::Result<AllPlayerData> {
    let file = File::open(get_player_file(config))?;
    let reader = BufReader::new(file);
    let player_data: AllPlayerData = serde_json::from_reader(reader)?;
    Ok(player_data)
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

fn get_player_file(config: &Config) -> String {
    config.get_string("storage.base").unwrap()
        + "/"
        + &config.get_string("storage.player_filename").unwrap()
}
