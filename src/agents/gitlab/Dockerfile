# Polar (OSS)

# Copyright 2024 Carnegie Mellon University.

# NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING
# INSTITUTE MATERIAL IS FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON
# UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER EXPRESSED OR IMPLIED, AS
# TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR PURPOSE
# OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE
# MATERIAL. CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND
# WITH RESPECT TO FREEDOM FROM PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

# Licensed under a MIT-style license, please see license.txt or contact
# permission@sei.cmu.edu for full terms.

# [DISTRIBUTION STATEMENT A] This material has been approved for public release
# and unlimited distribution.  Please see Copyright notice for non-US
# Government use and distribution.

# This Software includes and/or makes use of Third-Party Software each subject
# to its own license.

# DM24-0470

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
