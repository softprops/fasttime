use crate::BoxError;
use hyper::{Body, Request, Response};
use serde::Serialize;
use std::net::IpAddr;

// https://docs.rs/fastly/0.5.0/src/fastly/geo.rs.html#44
#[derive(Serialize, Clone, PartialEq, Debug)]
pub struct Geo {
    pub as_name: String,
    pub as_number: u32,
    pub area_code: u16,
    pub city: String,
    pub conn_speed: String,
    pub conn_type: String,
    pub continent: String,
    pub country_code: String,
    pub country_code3: String,
    pub country_name: String,
    pub latitude: f64,
    pub longitude: f64,
    pub metro_code: i64,
    pub postal_code: String,
    pub proxy_description: String,
    pub proxy_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    pub utc_offset: i32,
}

impl Default for Geo {
    fn default() -> Self {
        Geo {
            as_name: "AS22252".into(),
            as_number: 22252,
            area_code: 10026,
            city: "New York".into(),
            conn_speed: "satellite".into(),
            conn_type: "satellite".into(),
            continent: "NA".into(),
            country_code: "US".into(),
            country_code3: "USA".into(),
            country_name: "United States".into(),
            latitude: 40.69870,
            longitude: -73.98590,
            metro_code: 0,
            postal_code: "11201".into(),
            proxy_description: "cloud".into(),
            proxy_type: "public".into(),
            region: None,
            utc_offset: -5,
        }
    }
}

/// Defines a way to lookup a `Geo` by ip address
///
/// An implementaion is provided for a closure as well as static values
pub trait Lookup {
    fn lookup(
        &self,
        ip: IpAddr,
    ) -> Geo;
}

impl<F> Lookup for F
where
    F: Fn(IpAddr) -> Geo,
{
    fn lookup(
        &self,
        ip: IpAddr,
    ) -> Geo {
        self(ip)
    }
}

impl Lookup for Geo {
    fn lookup(
        &self,
        _: IpAddr,
    ) -> Geo {
        self.clone()
    }
}

pub struct GeoBackend(pub Box<dyn Lookup>);

impl crate::Backends for GeoBackend {
    fn send(
        &self,
        _: &str,
        req: Request<Body>,
    ) -> Result<Response<Body>, BoxError> {
        log::debug!("geo backend");
        // see fastly https://docs.rs/fastly/0.5.0/src/fastly/geo.rs.html#31
        match req
            .headers()
            .get("Fastly-XQD-arg1")
            .and_then(|hdr| hdr.to_str().ok())
            .and_then(|s| s.parse::<IpAddr>().ok())
        {
            Some(ip) => Ok(Response::builder()
                .status(200)
                .body(Body::from(serde_json::to_string(&self.0.lookup(ip))?))
                .expect("invalid response")),
            _ => Err(anyhow::anyhow!("expected request containing Fastly-XQD-arg1 header").into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closures_lookup() -> Result<(), BoxError> {
        let closure = |_: IpAddr| Geo::default();
        assert_eq!(
            closure.lookup("127.0.0.0".parse::<IpAddr>()?),
            Geo::default()
        );
        Ok(())
    }

    #[test]
    fn static_values_lookup() -> Result<(), BoxError> {
        let value = Geo::default();
        assert_eq!(value.lookup("127.0.0.0".parse::<IpAddr>()?), value);
        Ok(())
    }
}
