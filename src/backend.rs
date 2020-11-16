use crate::BoxError;
use hyper::{http::HeaderValue, Body, Request, Response};
use log::debug;
use reqwest::Client;
use std::collections::HashMap;

pub trait Backends: 'static {
    fn send(
        &self,
        backend: &str,
        req: Request<Body>,
    ) -> Result<Response<Body>, BoxError>;
}

impl<F> Backends for F
where
    F: Fn(&str, Request<Body>) -> Result<Response<Body>, BoxError> + 'static,
{
    fn send(
        &self,
        backend: &str,
        req: Request<Body>,
    ) -> Result<Response<Body>, BoxError> {
        self(backend, req)
    }
}

pub struct Proxy {
    backends: HashMap<String, String>,
    client: Client,
}

impl Proxy {
    pub fn new(backends: HashMap<String, String>) -> Self {
        let client = Client::new();
        Proxy { backends, client }
    }
}

impl Backends for Proxy {
    fn send(
        &self,
        backend: &str,
        req: Request<Body>,
    ) -> Result<Response<Body>, BoxError> {
        match self.backends.get(backend) {
            Some(host) => {
                debug!("proxying backend '{}' to '{}'", backend, host);

                let mut rreq = reqwest::Request::new(
                    req.method().clone(),
                    req.uri()
                        .to_string()
                        .parse::<reqwest::Url>()
                        .expect("invalid uri"),
                );
                *rreq.headers_mut() = req.headers().clone();
                rreq.headers_mut().remove("host");
                rreq.headers_mut()
                    .append("host", HeaderValue::from_str(&host)?);

                let rresp = match futures_executor::block_on(self.client.execute(rreq)) {
                    Ok(r) => r,
                    Err(e) => {
                        log::error!("error calling backend {}", e);
                        return Err(e.into());
                    }
                };
                debug!("got response");
                let headers = rresp.headers().clone();
                let builder = Response::builder()
                    .status(rresp.status())
                    .version(rresp.version());

                let mut resp = builder
                    .body(Body::from(futures_executor::block_on(rresp.bytes())?))
                    .expect("invalid response");
                *resp.headers_mut() = headers;
                Ok(resp)
            }
            _ => GatewayError.send(backend, req),
        }
    }
}

struct GatewayError;

impl Backends for GatewayError {
    fn send(
        &self,
        backend: &str,
        _: Request<Body>,
    ) -> Result<Response<Body>, BoxError> {
        Ok(Response::builder()
            .status(502)
            .body(format!("Unknown backend {}", backend).into())
            .expect("invalid response"))
    }
}

pub fn default() -> Box<dyn Backends + 'static> {
    Box::new(GatewayError)
}
