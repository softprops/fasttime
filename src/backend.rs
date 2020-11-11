use crate::BoxError;
use hyper::{Body, Request, Response};

pub trait Backend {
    fn send(
        &self,
        backend: &str,
        req: Request<Body>,
    ) -> Result<Response<Body>, BoxError>;
}

impl<F> Backend for F
where
    F: Fn(&str, Request<Body>) -> Result<Response<Body>, BoxError>,
{
    fn send(
        &self,
        backend: &str,
        req: Request<Body>,
    ) -> Result<Response<Body>, BoxError> {
        self(backend, req)
    }
}

pub fn default() -> impl Backend {
    move |backend: &str, _: Request<Body>| {
        Ok(Response::builder()
            .status(502)
            .body(format!("Unknown backend {}", backend).into())
            .unwrap())
    }
}
