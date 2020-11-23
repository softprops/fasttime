<h1 align="center">
  fasttime
</h1>

<p align="center">
   A lightweight Fastly <a alt="GitHub Actions" href="https://www.fastly.com/products/edge-compute/serverless/">Compute@Edge</a> runtime for running wasm applications locally
</p>

<div align="center">
  <a alt="GitHub Actions" href="https://github.com/softprops/fasttime/actions">
    <img src="https://github.com/softprops/fasttime/workflows/Main/badge.svg"/>
  </a>
  <a alt="license" href="LICENSE">
    <img src="https://img.shields.io/badge/license-MIT-brightgreen.svg"/>
  </a>
</div>

<br />

## about

Fastly allows you to run WASM request within a WASI-based runtime on its edge servers. `fasttime` implements those
runtime interfaces using [wasmtime](https://wasmtime.dev/) served on a local HTTP server allowing you to run you Compute@Edge applications ✨ locally
on your laptop ✨.

## 🤸 usage

### Building your app

The fastest way to get started with [Compute@Edge](https://www.fastly.com/products/edge-compute/serverless/) is though the [Fastly CLI](https://github.com/fastly/cli#installation)

```sh
$ fastly compute build
```

If you do not have Fastly CLI, you can also build with the standard cargo tooling. Fastly assumes a Rust toolchain version of `1.46.0`

```sh
# optionally install the wasm32 toolchain if you have not done so already
$ rustup target add wasm32-wasi --toolchain 1.46.0
# build a release mode .wasm executable
$ cargo +1.46.0 build --release --target wasm32-wasi
```

To start fasttime, just provide it with the path to your Fastly Compute@Edge `.wasm` build artifact.

### Serving up your app

```sh
$ fasttime -w target/wasm32-wasi/release/app.wasm
```

This starts up a localhost HTTP server listening on port `3000` which you can interact with with
an HTTP client like `curl`

```sh
curl -i "http://localhost:3000"
```

#### ↔️ backends

A common usecase for Fastly is proxying a set of backend hosts referred to by name. `fasttime` supports
providing multiple `-b | --backend` flags with values of the form `{backend}:{host}`. By default, if you
send a request to a backend that you have not mapped, a bad gateway response will be returned by the server.

```sh
$ fasttime -w target/wasm32-wasi/release/app.wasm \
    -b backend-two:localhost:3001 \
    -b backend-two:you.com
```

#### 📚 dictionaries

A common way to store lookup information in Fastly is to use [edge dictionaries](https://docs.fastly.com/en/guides/about-edge-dictionaries). `fasttime` supports
providing multiple `-d | --dictionary` flags with values of the form `{dictionary}:{key}={value},{key2}={value2}`. 

```sh
$ fasttime -w target/wasm32-wasi/release/app.wasm \
    -d dictionary-one:foo=bar \
    -d dictionary-two:baz=boom
```

#### 🪵 logging

The Compute@Edge runtime supports the notion of [remote logging endpoints](https://docs.fastly.com/en/guides/about-fastlys-realtime-log-streaming-features).
These are addressed by name within your applications.

```rust
use fastly::log::Endpoint;

let mut endpoint = Endpoint::from_name("endpoint-name");
writeln!(endpoint, "hello {}", "wasm");
```

`fasttime` currently support these by logging directly to stdout by default.

### tls

Using [mkcert](https://github.com/FiloSottile/mkcert), create a new tls certificate and private key

```sh
mkcert -key-file key.pem -cert-file cert.pem 127.0.0.1 localhost
```


```sh
$ fasttime -w target/wasm32-wasi/release/app.wasm \
    -tls-cert=./cert.pem \
    -tls-cert=./key.pem
```

#### 🔍 debugging

Set the `RUST_LOG` env variable to `fastime=debug` and run the cli as usual

```
RUST_LOG=fasttime=debug fasttime -w target/wasm32-wasi/release/app.wasm
```

## 🚧 roadmap

* tls support
* support config file based configuration
* hot reloading of wasm file

Doug Tangren (softprops) 2020
