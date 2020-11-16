<h1 align="center">
  fasttime
</h1>

<p align="center">
   A lightweight Fastly Compute@Edge runtime for running wasm applications locally
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

## 🤸 usage

The fastest way to get started with Compute@Edge is though [Fastly CLI](https://github.com/fastly/cli#installation)

```sh
$ fastly compute build
```

To start fasttime, just provide it with the path to your Fastly Compute@Edge `.wasm` build artifact.

```sh
$ fasttime -w path/to/main.wasm
```

This starts up a localhost HTTP server listening on port `3000` which you can interact with with
an HTTP client like `curl`

```sh
curl -i "http://localhost:3000"
```

### ↔️ backends

A common usecase for Fastly is proxying a set backend hosts referred to by name. `fasttime` supports
providing multiple `-b | --backend` flags with values of the form `{backend}:{host}`. By default, if you
send a request to a backend that you have not mapped, a bad gateway response will be returned by the server.

```sh
$ fasttime -w path/to/main.wasm \
    -b backend-two:localhost:3001 \
    -b backend-two:you.com
```

### 📚 dictionaries

A another common usecase for Fastly is looking up values in edge dictionaries. `fasttime` supports
providing multiple `-d | --dictionary` flags with values of the form `{dictionary}:{key}={value},{key2}={value2}`. 

```sh
$ fasttime -w path/to/main.wasm \
    -d dictionary-one:foo=bar \
     -d dictionary-two:baz=boom
```

### 🔍 debugging

Set the `RUST_LOG` env variable to `fastime=debug` and run as usual

```
RUST_LOG=fasttime=debug fasttime -w path/to/main.wasm
```

## 🚧 roadmap

* tls support
* support config file based configuration
* hot reloading of wasm file

Doug Tangren (softprops) 2020