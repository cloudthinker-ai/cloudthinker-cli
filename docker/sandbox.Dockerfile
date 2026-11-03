FROM debian:bookworm-slim

LABEL org.opencontainers.image.source=https://github.com/cloudthinker-ai/cloudthinker-cli

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash ca-certificates coreutils curl git jq less nodejs npm procps python3 python3-pip python3-venv ripgrep unzip util-linux \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --uid 10001 --create-home --shell /bin/bash worker
USER 10001
ENV HOME=/home/worker
WORKDIR /home/worker
