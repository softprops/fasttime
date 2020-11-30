<h1 align="center">
  fasttime
</h1>

<p align="center">
   A lightweight Fastly <a alt="GitHub Actions" href="https://www.fastly.com/products/edge-compute/serverless/">Compute@Edge</a> runtime for running and testing <a alt="wasm" href="https://webassembly.org/">WASM</a> applications locally
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

Fastly allows you to run WASM request handlers within a WASI-based runtime hosted on its managed edge servers. `fasttime` implements those runtime interfaces using [wasmtime](https://wasmtime.dev/) served up your application on a local HTTP server allowing you to run you Compute@Edge applications ✨ locally on your laptop ✨.

## 🤸 usage

### Building your app

The fastest way to get started with [Compute@Edge](https://www.fastly.com/products/edge-compute/serverless/) is though the [Fastly CLI](https://github.com/fastly/cli#installation)

```sh
$ fastly compute build
```

Alternatively, you can also build your WASM application using Rust's standard cargo tooling. 

> Fastly assumes a Rust toolchain version of `1.46.0`

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

#### ♻️ hot reloading

`fasttime` can monitor your WASM applicaion for changes, and gracefully reload the module when needed allowing for live-editing. Using the `--watch` flag, there's no need to restart `fasttime` after running `fastly compute build`!

```sh
$ fasttime -w target/wasm32-wasi/release/app.wasm \
    --watch
```

This pairs well with a `cargo watch` workflow. In another terminal, run

```sh
$ cargo +1.46.0 watch -x 'build --release --target wasm32-wasi'
```

You can then make changes to your application in your text editor, have cargo automatically rebuild your application, and have `fasttime` automatically reload it as you develop your application

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

A common way to look up key-value'd information in Fastly is to use [edge dictionaries](https://docs.fastly.com/en/guides/about-edge-dictionaries). `fasttime` supports providing multiple `-d | --dictionary` flags with values of the form `{dictionary}:{key}={value},{key2}={value2}`. 

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

#### 🔍 debugging

Set the `RUST_LOG` env variable to `fastime=debug` and run the cli as usual

```
RUST_LOG=fasttime=debug fasttime -w target/wasm32-wasi/release/app.wasm
```

## 🚧 roadmap

* tls support
* support config file based configuration

Doug Tangren (softprops) 2020
