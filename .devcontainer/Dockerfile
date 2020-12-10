# based on https://github.com/codespaces-examples/rust
FROM ubuntu:18.04

WORKDIR /home/

COPY . .

RUN bash ./setup.sh

ENV PATH="/root/.cargo/bin:$PATH"