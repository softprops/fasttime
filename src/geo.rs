use crate::BoxError;
use hyper::{Body, Request, Response};
use serde::Serialize;

#[derive(Serialize, Default)]
pub struct Geo {
    pub as_name: String,
    pub as_number: usize,
    pub area_code: String,
    pub city: String,
    pub conn_speed: String,
    pub conn_type: String,
    pub country_code: String,
    pub country_code3: String,
    pub country_name: String,
    pub latitude: f64,
    pub longitude: f64,
    pub metro_code: usize,
    pub postal_code: String,
    pub proxy_type: String,
    pub region: String,
    pub utc_offset: usize,
}

pub struct GeoBackend;

impl crate::Backends for GeoBackend {
    fn send(
        &self,
        _: &str,
        _: Request<Body>,
    ) -> Result<Response<Body>, BoxError> {
        log::debug!("geo backend");
        Ok(Response::builder()
            .status(200)
            .body(Body::from(serde_json::to_string(&Geo::default())?))
            .expect("invalid response"))
    }
}
