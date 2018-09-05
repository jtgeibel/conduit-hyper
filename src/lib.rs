#![deny(warnings)]
extern crate conduit;
extern crate futures;
extern crate futures_cpupool;
extern crate http;
extern crate hyper;
extern crate semver;

#[macro_use]
extern crate log;

#[cfg(test)]
mod tests;

use std::io::{Cursor, Read};
use std::net::SocketAddr;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use futures::{future, Future, Stream};
use futures_cpupool::CpuPool;
use hyper::{Body, Chunk, Method, Request, Response, Server, StatusCode, Version};

#[derive(Debug)]
struct Parts(http::request::Parts);

impl conduit::Headers for Parts {
    /// Find all values associated with a header, or None.
    ///
    /// If the value of a header is not valid UTF-8, that value
    /// is replaced with the emtpy string.
    fn find(&self, key: &str) -> Option<Vec<&str>> {
        let values = self.headers()
            .get_all(key)
            .iter()
            .map(|v| v.to_str().unwrap_or(""))
            .collect::<Vec<&str>>();

        if values.is_empty() {
            None
        } else {
            Some(values)
        }
    }

    fn has(&self, key: &str) -> bool {
        self.headers().contains_key(key)
    }

    /// Returns a representation of all headers
    ///
    /// There is currently a bug where keys with mutliple values will be duplicated.
    /// See: https://github.com/hyperium/http/issues/199
    fn all(&self) -> Vec<(&str, Vec<&str>)> {
        let mut all = Vec::new();
        for key in self.headers().keys() {
            let key = key.as_str();
            let values = self.find(key)
                .expect("all keys should have at least one value");
            all.push((key, values));
        }
        all
    }
}

impl Parts {
    fn headers(&self) -> &http::HeaderMap {
        &self.0.headers
    }
}

struct ConduitRequest {
    parts: Parts,
    body: Cursor<Chunk>,
    extensions: conduit::Extensions,
}

impl conduit::Request for ConduitRequest {
    fn http_version(&self) -> semver::Version {
        match self.parts.0.version {
            Version::HTTP_09 => version(0, 9),
            Version::HTTP_10 => version(1, 0),
            Version::HTTP_11 => version(1, 1),
            Version::HTTP_2 => version(2, 0),
        }
    }

    fn conduit_version(&self) -> semver::Version {
        version(0, 1)
    }

    fn method(&self) -> conduit::Method {
        match self.parts.0.method {
            Method::CONNECT => conduit::Method::Connect,
            Method::DELETE => conduit::Method::Delete,
            Method::GET => conduit::Method::Get,
            Method::HEAD => conduit::Method::Head,
            Method::OPTIONS => conduit::Method::Options,
            Method::PATCH => conduit::Method::Patch,
            Method::POST => conduit::Method::Post,
            Method::PUT => conduit::Method::Put,
            Method::TRACE => conduit::Method::Trace,
            _ => conduit::Method::Other(self.parts.0.method.to_string()),
        }
    }

    /// Always returns Http
    fn scheme(&self) -> conduit::Scheme {
        conduit::Scheme::Http
    }

    fn headers(&self) -> &conduit::Headers {
        &self.parts
    }

    /// Returns the length of the buffered body
    fn content_length(&self) -> Option<u64> {
        Some(self.body.get_ref().len() as u64)
    }

    /// Always returns an address of 0.0.0.0:0
    fn remote_addr(&self) -> SocketAddr {
        // See: https://github.com/hyperium/hyper/issues/1410#issuecomment-356115678
        ([0, 0, 0, 0], 0).into()
    }

    fn virtual_root(&self) -> Option<&str> {
        None
    }

    fn path(&self) -> &str {
        &self.parts.0.uri.path()
    }

    fn extensions(&self) -> &conduit::Extensions {
        &self.extensions
    }

    fn mut_extensions(&mut self) -> &mut conduit::Extensions {
        &mut self.extensions
    }

    /// Returns the value of the `Host` header
    ///
    /// If the header is not present or invalid UTF-8, then the empty string is returned
    fn host(&self) -> conduit::Host<'_> {
        let host = self.parts
            .headers()
            .get("host")
            .map(|h| h.to_str().unwrap_or(""))
            .unwrap_or("");
        conduit::Host::Name(host)
    }

    fn query_string(&self) -> Option<&str> {
        self.parts.0.uri.query()
    }

    fn body(&mut self) -> &mut Read {
        self.body.set_position(0);
        &mut self.body
    }
}

impl ConduitRequest {
    fn new(parts: Parts, body: Chunk) -> ConduitRequest {
        ConduitRequest {
            parts,
            body: Cursor::new(body),
            extensions: conduit::Extensions::new(),
        }
    }
}

/// Serve a `conduit::Handler` on a thread pool
pub struct Service<H> {
    pool: CpuPool,
    handler: Arc<H>,
}

// #[derive(Clone)] results in cloning a ref, and not the Service
impl<H> Clone for Service<H> {
    fn clone(&self) -> Self {
        Service {
            pool: self.pool.clone(),
            handler: self.handler.clone(),
        }
    }
}

impl<H: conduit::Handler> hyper::service::NewService for Service<H> {
    type ReqBody = Body;
    type ResBody = Body;
    type Error = hyper::Error;
    type Service = Service<H>;
    type Future = Box<Future<Item = Self::Service, Error = Self::InitError> + Send>;
    type InitError = hyper::Error;

    fn new_service(&self) -> Self::Future {
        Box::new(future::ok(self.clone()))
    }
}

impl<H: conduit::Handler> hyper::service::Service for Service<H> {
    type ReqBody = Body;
    type ResBody = Body;
    type Error = hyper::Error;
    type Future = Box<Future<Item = Response<Self::ResBody>, Error = Self::Error> + Send>;

    /// Returns a future which buffers the response body and then calls the conduit handler from a thread pool
    fn call(&mut self, request: Request<Self::ReqBody>) -> Self::Future {
        let pool = self.pool.clone();
        // Handlers that have interior mutability between requests are unlikely.  Any hanlders that
        // do must be prepared to handle poisoned mutexes.
        let handler = AssertUnwindSafe(self.handler.clone());

        let (parts, body) = request.into_parts();

        let response = body.concat2().and_then(move |full_body| {
            pool.spawn_fn(move || {
                // If the handler panics, the request is dropped and potentially inconsistent data
                // is not observable.
                let mut request = AssertUnwindSafe(ConduitRequest::new(Parts(parts), full_body));
                let response =
                    catch_unwind(move || {
                        handler
                            .call(&mut *request)
                            .map(good_response)
                            .unwrap_or_else(|e| error_response(e.description()))
                    }).unwrap_or_else(|_| error_response("Application handler paniced"));
                Ok(response)
            })
        });
        Box::new(response)
    }
}

impl<H: conduit::Handler> Service<H> {
    /// Create a multi-threaded `Service` from a `Handler`
    pub fn new(handler: H, threads: usize) -> Service<H> {
        Service {
            pool: CpuPool::new(threads),
            handler: Arc::new(handler),
        }
    }

    /// Run the `Service` bound to a given `SocketAddr`
    pub fn run(&self, addr: SocketAddr) {
        let server = Server::bind(&addr).serve(self.clone());
        hyper::rt::run(server.map_err(|e| error!("Server error: {}", e)));
    }
}

/// Builds a `hyper::Response` given a `conduit:Response`
fn good_response(mut response: conduit::Response) -> Response<Body> {
    let mut body = Vec::new();
    if response.body.write_body(&mut body).is_err() {
        return error_response("Error writing body");
    }

    let mut builder = Response::builder();
    let status = match StatusCode::from_u16(response.status.0 as u16) {
        Ok(s) => s,
        Err(e) => return error_response(&e.to_string()),
    };
    builder.status(status);

    for (key, values) in response.headers {
        for value in values {
            builder.header(key.as_str(), value.as_str());
        }
    }

    builder
        .body(body.into())
        .unwrap_or_else(|e| error_response(&e.to_string()))
}

/// Logs an error message and returns a generic status 500 response
fn error_response(message: &str) -> Response<Body> {
    error!("Internal Server Error: {}", message);
    let body = Body::from("Internal Server Error");
    Response::builder()
        .status(500)
        .body(body)
        .expect("unexpected invalid header")
}

fn version(major: u64, minor: u64) -> semver::Version {
    semver::Version {
        major,
        minor,
        patch: 0,
        pre: vec![],
        build: vec![],
    }
}
