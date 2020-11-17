use crate::BoxError;
use hyper::{Body, Request, Response};
use serde::Serialize;

#[derive(Serialize)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    pub utc_offset: isize,
}

impl Default for Geo {
    fn default() -> Self {
        Geo {
            as_name: "AS22252".into(),
            as_number: 22252,
            area_code: "".into(),
            city: "New York".into(),
            conn_speed: "".into(),
            conn_type: "".into(),
            country_code: "US".into(),
            country_code3: "USA".into(),
            country_name: "United States".into(),
            latitude: 40.69870,
            longitude: -73.98590,
            metro_code: 0,
            postal_code: "11201".into(),
            proxy_type: "".into(),
            region: None,
            utc_offset: -5,
        }
    }
}

pub struct GeoBackend;

impl crate::Backends for GeoBackend {
    fn send(
        &self,
        _: &str,
        req: Request<Body>,
    ) -> Result<Response<Body>, BoxError> {
        log::debug!("geo backend");
        // see fastly https://docs.rs/fastly/0.5.0/src/fastly/geo.rs.html#31
        log::debug!(
            "Fastly-XQD-arg1: {:?}",
            req.headers().get("Fastly-XQD-arg1")
        );
        Ok(Response::builder()
            .status(200)
            .body(Body::from(serde_json::to_string(&Geo::default())?))
            .expect("invalid response"))
    }
}
