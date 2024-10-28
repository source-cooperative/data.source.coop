mod apis;
mod backends;
mod guards;
mod route_handlers;
mod utils;

use crate::guards::GetObjectGuard;
use crate::route_handlers::get_object;
use crate::utils::context::LoadContext;

use actix_cors::Cors;
use actix_web::{get, middleware, web, App, HttpResponse, HttpServer, Responder};

use apis::source::SourceAPI;
use env_logger::Env;
use std::env;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[get("/")]
async fn index() -> impl Responder {
    HttpResponse::Ok().body(format!("Source Cooperative Data Proxy v{}", VERSION))
}

// Main function to set up and run the HTTP server
#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let source_api_url = env::var("SOURCE_API_URL").unwrap();
    let source_api = web::Data::new(SourceAPI::new(source_api_url));
    env_logger::init_from_env(Env::default().default_filter_or("info"));

    HttpServer::new(move || {
        App::new()
            .app_data(web::PayloadConfig::new(1024 * 1024 * 50))
            .app_data(source_api.clone())
            .wrap(
                // Configure CORS
                Cors::default()
                    .allow_any_origin()
                    .allow_any_method()
                    .allow_any_header()
                    .supports_credentials()
                    .block_on_origin_mismatch(false)
                    .max_age(3600),
            )
            .wrap(middleware::NormalizePath::trim())
            .wrap(middleware::DefaultHeaders::new().add(("X-Version", VERSION)))
            .wrap(LoadContext)
            .wrap(middleware::Logger::default())
            // Register the endpoints
            .service(
                web::resource("/{path:.*}").route(web::get().guard(GetObjectGuard).to(get_object)),
            )
            .service(index)
    })
    .bind("0.0.0.0:8080")?
    .run()
    .await
}
