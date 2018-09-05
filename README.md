# conduit-hyper

[![Build Status](https://travis-ci.org/jtgeibel/conduit-hyper.svg?branch=master)](https://travis-ci.org/jtgeibel/conduit-hyper)

This crate integrates a `hyper 0.12` server with a `conduit 0.8` application
stack.

## Error and Panic Handling

If the application handler returns an `Err(_)` the server will log the
description via the `log` crate and then return a generic 500 status response.

If the handler panics, the default panic handler prints a message to stderr,
the handler is unwound, an error is logged via `log`, and a generic 500 status
response is returned.

Handlers that rely on interior mutability (such as with a `Mutex`) must be
prepared to deal with possibly inconsistent state (such as a poisoned `Mutex`)
if a previous call has paniced.  It is unlikely that many handlers have such
shared state between requests.

## Request Processing

If the request includes a body, the entire body is buffered before the handler
is dispatched on a thread.  There is currently no restriction on the maximum
body size so a client can consume large amounts of memory by sending a large
body.  Therefore it is recommended to use a reverse proxy which limits the
maximum body size.

Header values that are not valid UTF-8 are replaced with an empty string.

### conduit::Request

The following methods on the `Request` provided to the application have
noteworthy behavior:

* `scheme` always returns Http as https is not currently directly supported
* `remote_addr` always returns 0.0.0.0:0
* `host` returns an empty string if the `Host` header is not valid UTF-8

All other methods on `Request` should behave as expected.

## TODO

* Include the `X-Request-Id` header when logging an error

## License

Licensed under either of these:

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
   https://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or
   https://opensource.org/licenses/MIT)
