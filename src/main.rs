use actix::{Actor, StreamHandler};
use actix_web::{get, post, web, App, Error, HttpRequest, HttpResponse, HttpServer, Responder};
use actix_web_actors::ws;

/// Define HTTP actor
struct GameWs;

impl Actor for GameWs {
    type Context = ws::WebsocketContext<Self>;
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

async fn ws_upgrader(req: HttpRequest, stream: web::Payload) -> Result<HttpResponse, Error> {
    let resp = ws::start(GameWs {}, &req, stream);
    println!("{:?}", req);
    resp
}

#[get("/")]
async fn hello() -> impl Responder {
    HttpResponse::Ok().body("Hello world!")
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    HttpServer::new(|| {
        App::new()
            .service(hello)
            .route("/ws/", web::get().to(ws_upgrader))
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await
}
