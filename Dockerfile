FROM rust:1.70 AS builder

RUN rustup update && rustup default nightly
RUN rustup target add x86_64-unknown-linux-musl
# RUN apt update && apt install -y musl musl-tools musl-dev pkg-config libssl-dev

WORKDIR /usr/src/app
COPY .vscode /usr/src/app/.vscode
COPY ./gitlab_agent /usr/src/app/gitlab_agent
COPY ./polar /usr/src/app/polar
COPY .git /usr/src/app/.git
