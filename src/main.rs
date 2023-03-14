use std::fs::File;
use std::io::Write;

use actix::{Actor, StreamHandler};
use actix_web::{
    get, put,
    web::{self, BytesMut},
    App, Error, HttpRequest, HttpResponse, HttpServer, Responder,
};
use actix_web_actors::ws;
use config::Config;
use futures::StreamExt;
use uuid::Uuid;

// constants
const MAX_SIZE: usize = 512 * 1024; // max payload size is 512k

/// Define HTTP actor
struct GameWs;
impl Actor for GameWs {
    type Context = ws::WebsocketContext<Self>;
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
    let ghost_dir = get_ghost_dir(&req.app_data::<Config>().unwrap());

    let id = Uuid::new_v4(); // Generate a random UUID

    let mut f = File::create(format!("{ghost_dir}/{id}"))?; // Attempt to create the file
    if f.write_all(&ghost_data[..]).is_err() {
        return Err(actix_web::error::ErrorInternalServerError(
            "couldn't write to file",
        )); // Return error if file can't be written to
    }

    Ok(HttpResponse::Ok().body(format!("{id}")))
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
