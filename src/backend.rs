use crate::BoxError;
use hyper::{Body, Request, Response};
use log::debug;
use reqwest::Client;

pub trait Backend: 'static {
    fn send(
        &self,
        backend: &str,
        req: Request<Body>,
    ) -> Result<Response<Body>, BoxError>;
}

impl<F> Backend for F
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
    addr: String,
    client: Client,
}

impl Proxy {
    pub fn new(addr: String) -> Self {
        let client = Client::new(); //: Client<_, hyper::Body> = Client::builder().build(HttpsConnector::new());
        Proxy { addr, client }
    }
}

impl Backend for Proxy {
    fn send(
        &self,
        backend: &str,
        req: Request<Body>,
    ) -> Result<Response<Body>, BoxError> {
        debug!("proxying backend '{}' to '{}'", backend, self.addr);
        let mut rreq = reqwest::Request::new(
            req.method().clone(),
            req.uri()
                .to_string()
                .parse::<reqwest::Url>()
                .expect("invalid uri"),
        );
        *rreq.headers_mut() = req.headers().clone();
        rreq.headers_mut()
            .append("host", hyper::http::HeaderValue::from_static("httpbin.org"));

        let rresp = match futures_executor::block_on(self.client.execute(rreq)) {
            Ok(r) => r,
            Err(e) => {
                log::error!("error calling backend {}", e);
                Err(e)?
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
}

struct GatewayError;

impl Backend for GatewayError {
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

pub fn default() -> impl Backend {
    GatewayError
}
