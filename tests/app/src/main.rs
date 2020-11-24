//! Default Compute@Edge template program.

use fastly::{
    dictionary::Dictionary,
    log::Endpoint,
    http::{HeaderValue, Method, StatusCode},
    request::CacheOverride,
    Body, Error, Request, RequestExt, Response, ResponseExt,
};
use std::io::Write;

/// The name of a backend server associated with this service.
///
/// This should be changed to match the name of your own backend. See the the `Hosts` section of
/// the Fastly WASM service UI for more information.
const BACKEND_NAME: &str = "backend_name";


/// The name of a second backend associated with this service.
const OTHER_BACKEND_NAME: &str = "other_backend_name";

/// The entry point for your application.
///
/// This function is triggered when your service receives a client request. It could be used to
/// route based on the request properties (such as method or path), send the request to a backend,
/// make completely new requests, and/or generate synthetic responses.
///
/// If `main` returns an error, a 500 error response will be delivered to the client.
#[fastly::main]
fn main(mut req: Request<Body>) -> Result<impl ResponseExt, Error> {
    // Make any desired changes to the client request.
    req.headers_mut()
        .insert("Host", HeaderValue::from_static("example.com"));

    let mut log = Endpoint::from_name("my_endpoint");

    for hdr in fastly::downstream_original_header_names() {
        drop(writeln!(log, "{:?}", hdr))
    }


    if let Some(ua) = req.headers().get("User-Agent") {
        drop(writeln!(log, "{:?}", fastly::uap_parse(ua.to_str()?)));
    }

    drop(writeln!(log, "{:?}", fastly::downstream_original_header_count()));

    drop(writeln!(log, "{:?}", fastly::downstream_client_ip_addr()));

    // We can filter requests that have unexpected methods.
    const VALID_METHODS: [Method; 3] = [Method::HEAD, Method::GET, Method::POST];
    if !(VALID_METHODS.contains(req.method())) {
        return Ok(Response::builder()
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .body(Body::from("This method is not allowed"))?);
    }

    // Pattern match on the request method and path.
    match (req.method(), req.uri().path()) {
        // If request is a `GET` to the `/` path, send a default response.
        (&Method::GET, "/") => Ok(Response::builder()
            .status(StatusCode::OK)
            .body(Body::from("Welcome to Fastly Compute@Edge!"))?),

        (&Method::GET, "/dictionary-hit") => match Dictionary::open("dict").get("foo")
        {
            Some(foo) => Ok(Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(format!("dict::foo is {}", foo)))?),
            _ => Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from("dict::foo is unknown"))?),
        },
        (&Method::GET, "/dictionary-miss") => match Dictionary::open("bogus").get("foo")
        {
            Some(foo) => Ok(Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(format!("bogus::foo is {}", foo)))?),
            _ => Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from("dict::foo is unknown"))?),
        },

        (&Method::GET, "/geo") => 
        {
            let client_ip = fastly::downstream_client_ip_addr().unwrap();
            let geo = fastly::geo::geo_lookup(client_ip);
            Ok(Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(format!("ip {} {:?}", client_ip, geo)))?)
        },

        // If request is a `GET` to the `/backend` path, send to a named backend.
        (&Method::GET, "/backend") => {
            println!("sending to backend cache override {}", BACKEND_NAME);
            // Request handling logic could go here...
            // E.g., send the request to an origin backend and then cache the
            // response for one minute.
            *req.cache_override_mut() = CacheOverride::ttl(60);
            println!("sending to backend  {} uri {}", BACKEND_NAME, req.uri());
            let mut resp = req.send(BACKEND_NAME)?;
            resp.headers_mut().remove("foo");
            Ok(resp)
        }

        // If request is a `GET` to a path starting with `/other/`.
        (&Method::GET, path) if path.starts_with("/other/") => {
            println!("overriding cache to other {}", OTHER_BACKEND_NAME);
            // Send request to a different backend and don't cache response.
            *req.cache_override_mut() = CacheOverride::Pass;
            println!("sending to other {}", OTHER_BACKEND_NAME);
            Ok(req.send(OTHER_BACKEND_NAME)?)
        }

        // Catch all other requests and return a 404.
        _ => Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("The page you requested could not be found"))?),
    }
}
