# fasttime

A lightweight Fastly Compute@Edge runtime for running wasm applications locally

# usage

The fastest way to get started with Compute@Edge is though [Fastly CLI](https://github.com/fastly/cli#installation)

```sh
$ fastly compute build
```

To start fasttime, just provide it with the path to your Fastly Compute@Edge `.wasm` build artifact.

```sh
$ fasttime -w path/to/main.wasm
```

This starts up a localhost HTTP server listening on port `3000`

```sh
curl -i "http://localhost:3000"
```

Doug Tangren (softprops) 2020