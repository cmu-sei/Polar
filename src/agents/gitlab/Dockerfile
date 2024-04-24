# Pre Build Stage (Make an agent user)
FROM alpine:3.18.2 as prebuild

RUN apk --no-cache add shadow bash gcc
RUN groupadd -r -g 1000 agent && \
    useradd -r -u 1000 -g agent -m -d /opt/agent -s /bin/bash agent

# Build Stage - Builds the rust binary
FROM alpine:3.18.2 as build
RUN mkdir /app
RUN apk add bash curl git clang gcc g++ wget curl tar make cmake openssl-dev
# Get Rustup
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | bash -s -- -y
ENV PATH="/root/.cargo/bin:${PATH}"

# Source the newly installed rust stuff 
RUN rustup update && rustup default stable

# Copy the gitlab_agent into app
COPY . /app
WORKDIR /app

ENV RUSTFLAGS="-C target-feature=-crt-static -C target-cpu=native"
RUN cargo build --release

# Gitlab Agent
FROM prebuild as gitlab-observer
WORKDIR /opt/agent
COPY --from=build /app/target/release/gitlab_agent .

RUN chown 1000:1000 ./gitlab_agent && \
    chmod 755 ./gitlab_agent

USER agent

CMD ["./gitlab_agent"]

# Gitlab Consumer
FROM prebuild as gitlab-consumer
WORKDIR /opt/agent
COPY --from=build /app/target/release/gitlab_consumer .

RUN chown 1000:1000 ./gitlab_consumer  && \
    chmod 755 ./gitlab_consumer 

USER agent

CMD ["./gitlab_consumer"]
