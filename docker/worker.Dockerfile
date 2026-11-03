FROM debian:bookworm-slim

LABEL org.opencontainers.image.source=https://github.com/cloudthinker-ai/cloudthinker-cli

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

ARG TARGETARCH
COPY dist/linux-${TARGETARCH}/cloudthinker /usr/local/bin/cloudthinker

RUN useradd --uid 10001 --create-home --shell /usr/sbin/nologin cloudthinker
USER 10001

ENTRYPOINT ["cloudthinker"]
CMD ["worker", "start"]
